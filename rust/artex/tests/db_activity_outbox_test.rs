//! Differential tests for `db/activity_outbox.go` — port of `db/activity_outbox_test.go`
//! (marshal round-trip keeps Detail, replay once, stuck row dropped after 5).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::exploration::Activity;

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

#[tokio::test]
async fn flush_activity_outbox_replays_once() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let exp_id = d.create_exploration("outbox", "replay").await.unwrap();
    let _ = d
        .execute(
            "DELETE FROM activity_outbox WHERE exploration_id=$1",
            &[&exp_id],
        )
        .await;
    let store = d.exploration(exp_id);
    store
        .enqueue_activity(
            &Activity {
                worker: "worker".to_string(),
                kind: "text".to_string(),
                summary: "kept".to_string(),
                detail: "body".to_string(),
                ..Activity::default()
            },
            "insert failed",
        )
        .await
        .unwrap();

    let n = store.flush_activity_outbox().await.unwrap();
    assert_eq!(n, 1, "one activity replayed");
    let activities: i64 = d
        .query_one(
            "SELECT count(*) FROM activity WHERE exploration_id=$1 AND summary='kept'",
            &[&exp_id],
        )
        .await
        .unwrap()
        .get(0);
    let queued: i64 = d
        .query_one(
            "SELECT count(*) FROM activity_outbox WHERE exploration_id=$1",
            &[&exp_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(activities, 1);
    assert_eq!(queued, 0);
    // Replayed row keeps its detail.
    let detail: String = d
        .query_one(
            "SELECT detail FROM activity WHERE exploration_id=$1 AND summary='kept'",
            &[&exp_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(detail, "body", "detail preserved through the outbox");

    // Second flush: nothing queued.
    let n = store.flush_activity_outbox().await.unwrap();
    assert_eq!(n, 0);

    let _ = d
        .execute(
            "DELETE FROM activity_outbox WHERE exploration_id=$1",
            &[&exp_id],
        )
        .await;
    let _ = d
        .execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await;
}

#[tokio::test]
async fn flush_activity_outbox_drops_stuck_row() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let missing = 9_000_000_000_001i64; // no parent exploration -> FK violation
    let _ = d
        .execute(
            "DELETE FROM activity_outbox WHERE exploration_id=$1",
            &[&missing],
        )
        .await;
    let store = d.exploration(missing);
    store
        .enqueue_activity(
            &Activity {
                worker: "worker".to_string(),
                kind: "text".to_string(),
                summary: "stuck".to_string(),
                ..Activity::default()
            },
            "no parent",
        )
        .await
        .unwrap();

    // 4 attempts keep the row; the 5th drops it.
    for attempt in 1..5 {
        let n = store.flush_activity_outbox().await.unwrap();
        assert_eq!(n, 0, "attempt {attempt}");
        let queued: i64 = d
            .query_one(
                "SELECT count(*) FROM activity_outbox WHERE exploration_id=$1",
                &[&missing],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(queued, 1, "attempt {attempt}: row kept");
    }
    let n = store.flush_activity_outbox().await.unwrap();
    assert_eq!(n, 0);
    let queued: i64 = d
        .query_one(
            "SELECT count(*) FROM activity_outbox WHERE exploration_id=$1",
            &[&missing],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(queued, 0, "row dropped after max attempts");

    let _ = d
        .execute(
            "DELETE FROM activity_outbox WHERE exploration_id=$1",
            &[&missing],
        )
        .await;
}
