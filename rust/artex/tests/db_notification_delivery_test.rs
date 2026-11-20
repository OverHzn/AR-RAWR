//! Differential tests for `db/notification_delivery.go` — state transitions,
//! filter/list paging, truncate/placeholders helpers. Runs against `ARTEX_PG_DSN`.

use artex::db::notification_channels::NotificationChannel;
use artex::db::notification_delivery::{
    placeholders, truncate_notify_error, NotificationDeliveryFilter,
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
    assert_eq!(placeholders(3, &[7, 8]), ("$3,$4".to_string(), vec![7, 8]));
    assert_eq!(placeholders(1, &[]), (String::new(), Vec::<i64>::new()));
    assert_eq!(truncate_notify_error("short"), "short");
    let long = "é".repeat(300); // 600 bytes
    let out = truncate_notify_error(&long);
    assert!(out.len() <= 503 && out.ends_with('…'));
    assert!(out.is_char_boundary(out.len()));
}

#[tokio::test]
async fn delivery_state_transitions_and_list() {
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
        "t_deliv_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d
        .execute(
            "DELETE FROM notification_channels WHERE name LIKE $1",
            &[&format!("{name}%")],
        )
        .await;

    let mut c = NotificationChannel {
        name: name.clone(),
        kind: "webhook".to_string(),
        mode: "realtime".to_string(),
        config: serde_json::json!({"url":"https://hooks.example/x"}),
        filter: serde_json::json!({}),
        enabled: Some(true),
        rate_per_min: 0,
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
    let cid = d.save_notification_channel(&mut c).await.unwrap();

    // Seed two events + deliveries via raw SQL (fan-out not ported yet).
    let mut ids = Vec::new();
    for i in 0..3 {
        let eid = d.query_one(
            "INSERT INTO notification_events(kind, finding_id, snapshot, fanned_out) VALUES ($1,$2,'{}'::jsonb,true) RETURNING id",
            &[&"finding_created", &(i as i64)],
        ).await.unwrap().get::<_, i64>(0);
        let did = d.query_one(
            "INSERT INTO notification_deliveries(event_id, channel_id, state, attempts, next_attempt_at, last_error) \
             VALUES ($1,$2,'pending',1,now(),'') RETURNING id",
            &[&eid, &cid],
        ).await.unwrap().get::<_, i64>(0);
        ids.push(did);
    }

    // List: filter by channel + state, newest first.
    let (list, total) = d
        .list_notification_deliveries(
            &NotificationDeliveryFilter {
                channel_id: cid,
                state: "pending".to_string(),
                event_kind: String::new(),
            },
            1,
            50,
        )
        .await
        .unwrap();
    assert_eq!(total, 3);
    assert_eq!(list.len(), 3);
    assert!(list[0].id > list[1].id, "newest first");
    assert_eq!(
        list[0].channel.as_ref().unwrap().kind,
        "webhook",
        "channel join-loaded"
    );

    // Mark sent -> state sent, sent_at set.
    d.mark_deliveries_sent(&[ids[0]]).await.unwrap();
    let (_, total2) = d
        .list_notification_deliveries(
            &NotificationDeliveryFilter {
                channel_id: cid,
                state: "sent".to_string(),
                event_kind: String::new(),
            },
            1,
            50,
        )
        .await
        .unwrap();
    assert_eq!(total2, 1);

    // Reschedule -> pending with delayed retry.
    d.reschedule_deliveries(&[ids[1]], 60.0, "rate limited")
        .await
        .unwrap();
    let state: String = d
        .query_one(
            "SELECT state FROM notification_deliveries WHERE id=$1",
            &[&ids[1]],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(state, "pending");

    // Defer -> pending, attempts decremented (1 -> 0).
    d.execute(
        "UPDATE notification_deliveries SET attempts=2 WHERE id=$1",
        &[&ids[2]],
    )
    .await
    .unwrap();
    d.defer_deliveries(&[ids[2]], "segmented").await.unwrap();
    let attempts: i32 = d
        .query_one(
            "SELECT attempts FROM notification_deliveries WHERE id=$1",
            &[&ids[2]],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(attempts, 1, "attempt revoked");

    // Fail -> state failed; retry -> pending with attempts 0.
    d.fail_deliveries(&[ids[0]], "permanent error")
        .await
        .unwrap();
    let state: String = d
        .query_one(
            "SELECT state FROM notification_deliveries WHERE id=$1",
            &[&ids[0]],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(state, "failed");
    d.retry_notification_delivery(ids[0]).await.unwrap();
    let (state, attempts): (String, i32) = {
        let row = d
            .query_one(
                "SELECT state, attempts FROM notification_deliveries WHERE id=$1",
                &[&ids[0]],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(state, "pending");
    assert_eq!(attempts, 0);

    // Empty batch no-ops.
    d.mark_deliveries_sent(&[]).await.unwrap();
    d.fail_deliveries(&[], "x").await.unwrap();

    let _ = d
        .execute("DELETE FROM notification_channels WHERE id=$1", &[&cid])
        .await;
}
