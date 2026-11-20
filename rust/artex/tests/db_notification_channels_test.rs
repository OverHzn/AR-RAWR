//! Differential tests for the notification channel slice of `db/notification.go`
//! — channel CRUD, sealed config, status-with-notify, stats, asset names.
//! Runs against `ARTEX_PG_DSN`; skips when unreachable.

use artex::db::notification_channels::{
    asset_display_name, valid_notify_mode, NotificationChannel,
};
use artex::db::secrets::{set_active_secret_box, SecretBox};

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<artex::db::DB> {
    match artex::db::DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

#[test]
fn helpers() {
    assert!(valid_notify_mode("realtime") && valid_notify_mode("digest"));
    assert!(!valid_notify_mode("push"));
    assert_eq!(
        asset_display_name("root_domain", "example.com", "1.2.3.4", "u", "a", "b"),
        "example.com"
    );
    assert_eq!(
        asset_display_name("ip", "", "1.2.3.4", "", "", ""),
        "1.2.3.4"
    );
    assert_eq!(
        asset_display_name("app", "", "", "", "MyApp", "bundle"),
        "MyApp"
    );
    assert_eq!(
        asset_display_name("endpoint", "", "", "http://x", "", ""),
        "http://x"
    );
    assert_eq!(asset_display_name("unknown", "", "", "", "", ""), "");
}

#[tokio::test]
async fn channel_crud_sealed_config_and_status_notify() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    set_active_secret_box(SecretBox::new(b"0123456789abcdef0123456789abcdef").unwrap());
    let name = format!(
        "t_chan_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d
        .execute("DELETE FROM notification_channels WHERE name=$1", &[&name])
        .await;

    let mut c = NotificationChannel {
        name: name.clone(),
        kind: "webhook".to_string(),
        mode: "realtime".to_string(),
        config: serde_json::json!({"url":"https://hooks.example/x","token":"secret-token-abc"}),
        filter: serde_json::json!({}),
        enabled: Some(true),
        rate_per_min: 0, // explicit zero preserved
        ..NotificationChannel {
            id: 0,
            name: String::new(),
            kind: String::new(),
            mode: String::new(),
            config: serde_json::Value::Null,
            filter: serde_json::Value::Null,
            enabled: None,
            rate_per_min: 0,
            created_at: std::time::SystemTime::UNIX_EPOCH,
            updated_at: std::time::SystemTime::UNIX_EPOCH,
        }
    };
    let id = d.save_notification_channel(&mut c).await.unwrap();
    assert!(id > 0);

    // Config sealed at rest, clear via getters.
    let raw: serde_json::Value = {
        let row = d
            .query_one(
                "SELECT config FROM notification_channels WHERE id=$1",
                &[&id],
            )
            .await
            .unwrap();
        row.get(0)
    };
    let raw_s = raw.to_string();
    assert!(raw_s.contains("__artex_enc"), "config sealed: {raw_s}");
    assert!(!raw_s.contains("secret-token-abc"));
    let got = d.notification_channel_by_id(id).await.unwrap();
    assert_eq!(got.config, c.config);
    assert_eq!(got.rate_per_min, 0, "explicit 0 preserved");
    assert!(got.is_enabled());

    // Update (rate 5).
    c.id = id;
    c.rate_per_min = 5;
    d.save_notification_channel(&mut c).await.unwrap();
    assert_eq!(
        d.notification_channel_by_id(id).await.unwrap().rate_per_min,
        5
    );

    // Negative rate rejected.
    c.rate_per_min = -1;
    assert!(d.save_notification_channel(&mut c).await.is_err());
    c.rate_per_min = 5;

    // List includes it.
    assert!(d
        .list_notification_channels()
        .await
        .unwrap()
        .iter()
        .any(|x| x.id == id));

    // Disable marks deliveries skipped; enable again.
    d.set_notification_channel_enabled(id, false).await.unwrap();
    assert!(!d.notification_channel_by_id(id).await.unwrap().is_enabled());
    d.set_notification_channel_enabled(id, true).await.unwrap();
    assert!(d.notification_channel_by_id(id).await.unwrap().is_enabled());

    // Notification stats snapshot runs.
    let stats = d.notification_stats_snapshot().await.unwrap();
    assert!(stats.channels >= 1);

    // Asset names.
    let aid = d.query_one(
        "INSERT INTO assets(type, domain, company_source) VALUES ('root_domain', $1, 'explicit') RETURNING id",
        &[&format!("asset-{id}.example")],
    ).await.unwrap().get::<_, i64>(0);
    let names = d
        .notification_asset_names(&[aid, 99_999_999])
        .await
        .unwrap();
    assert_eq!(names.len(), 1, "unresolved id dropped");
    assert!(names[0].contains("asset-"));

    // Delete + cascade.
    d.delete_notification_channel(id).await.unwrap();
    assert!(d.notification_channel_by_id(id).await.is_err());
    let _ = d.execute("DELETE FROM assets WHERE id=$1", &[&aid]).await;
}

#[tokio::test]
async fn status_with_notify_only_on_real_change() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    set_active_secret_box(SecretBox::new(b"0123456789abcdef0123456789abcdef").unwrap());
    let fid = d
        .add_finding(0, 0, "vc", "name", "high", "sum", "ev", "w", &[])
        .await
        .unwrap();
    let before: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_events WHERE finding_id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);

    // First change: registered.
    let (from, found, notified) = d
        .set_finding_status_with_notify(fid, "confirmed")
        .await
        .unwrap();
    assert!(found && from == "pending" && notified);
    let after1: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_events WHERE finding_id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(after1, before + 1);

    // Same status: no new event.
    let (_, found2, notified2) = d
        .set_finding_status_with_notify(fid, "confirmed")
        .await
        .unwrap();
    assert!(found2 && !notified2);
    let after2: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_events WHERE finding_id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(after2, after1, "no push on no-op");

    // Batch: one new event for a real change, none for no-op.
    let res = d
        .set_finding_statuses_with_notify(&[fid, 99_999_999], "resolved")
        .await
        .unwrap();
    assert_eq!(res.len(), 2);
    assert!(res[0].changed && res[0].notified);
    assert!(!res[1].found);
    let after3: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_events WHERE finding_id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(after3, after2 + 1);

    let _ = d.delete_finding(fid).await;
}
