//! Differential tests for `FanOutPendingEvents` (`db/notification.go`) — port of
//! `TestFanOutRoutesEventsByFilter` + `TestFanOutMarksEventsWithNoMatchingChannel`.
//! Runs against `ARTEX_PG_DSN`; skips when unreachable.

use artex::db::notification_channels::NotificationChannel;
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

fn channel(name: &str, filter: serde_json::Value) -> NotificationChannel {
    NotificationChannel {
        name: name.to_string(),
        kind: "webhook".to_string(),
        mode: "realtime".to_string(),
        config: serde_json::json!({"url":"https://hooks.example/x"}),
        filter,
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
    }
}

#[tokio::test]
async fn fan_out_routes_by_filter_and_marks_no_match() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    set_active_secret_box(SecretBox::new(b"0123456789abcdef0123456789abcdef").unwrap());
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d
        .execute(
            "DELETE FROM notification_channels WHERE name LIKE $1",
            &[&format!("t_fo-{tag}%")],
        )
        .await;
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE finding_id BETWEEN $1 AND $2",
            &[&(-(i64::MAX)), &0],
        )
        .await;

    // Channel A: filter matches severity high. Channel B: filter that matches nothing.
    let mut ca = channel(
        &format!("t_fo-a-{tag}"),
        serde_json::json!({"min_severity":"high"}),
    );
    let cid_a = d.save_notification_channel(&mut ca).await.unwrap();
    let mut cb = channel(
        &format!("t_fo-b-{tag}"),
        serde_json::json!({"min_severity":"critical"}),
    );
    let cid_b = d.save_notification_channel(&mut cb).await.unwrap();

    // Two events: one finding_created (high severity) -> matches A; one finding_created low -> no match.
    let snap_high = serde_json::json!({
        "kind":"finding_created","finding_id":1,"task_id":0,"vulnclass":"vc","name":"n",
        "severity":"high","summary":"s","asset_ids":[],"from_status":"","to_status":""
    });
    let snap_low = serde_json::json!({
        "kind":"finding_created","finding_id":2,"task_id":0,"vulnclass":"vc","name":"n",
        "severity":"low","summary":"s","asset_ids":[],"from_status":"","to_status":""
    });
    let e1 = d.query_one(
        "INSERT INTO notification_events(kind,finding_id,snapshot) VALUES ('finding_created',1,$1::jsonb) RETURNING id",
        &[&snap_high],
    ).await.unwrap().get::<_, i64>(0);
    let e2 = d.query_one(
        "INSERT INTO notification_events(kind,finding_id,snapshot) VALUES ('finding_created',2,$1::jsonb) RETURNING id",
        &[&snap_low],
    ).await.unwrap().get::<_, i64>(0);

    let (events, deliveries) = d.fan_out_pending_events(200).await.unwrap();
    assert!(events >= 2, "both events dispatched (got {events})");
    assert!(
        deliveries >= 1,
        "at least the high-severity event produced a delivery"
    );

    // Both events marked fanned_out.
    let fan: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_events WHERE id IN ($1,$2) AND fanned_out",
            &[&e1, &e2],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(fan, 2, "no-match event also marked fanned_out");

    // Second fan-out: no pending events -> 0/0.
    let (events2, deliveries2) = d.fan_out_pending_events(200).await.unwrap();
    // Other tests may have created pending events; ours are definitely gone.
    assert_eq!(events2, 0, "our events were marked fanned_out");
    let _ = deliveries2;

    // High event got a delivery on channel A (filter by channel).
    let n_a: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_deliveries WHERE event_id=$1 AND channel_id=$2",
            &[&e1, &cid_a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n_a, 1, "high event matched channel A");
    // Channel B (min_severity=critical) did not match a high-severity event.
    let n_b: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_deliveries WHERE event_id=$1 AND channel_id=$2",
            &[&e1, &cid_b],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n_b, 0, "critical threshold filter did not match high event");

    let _ = d
        .execute(
            "DELETE FROM notification_channels WHERE id=ANY($1::bigint[])",
            &[&vec![cid_a, cid_b]],
        )
        .await;
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE id=ANY($1::bigint[])",
            &[&vec![e1, e2]],
        )
        .await;
}
