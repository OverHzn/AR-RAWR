//! Differential test for the exploration store handle — fresh spec for the
//! `ExplorationStore` skeleton (`CreateExploration`/`Exploration`/`ID`/`Root`).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Cleans up the seeded exploration by id.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos + (N.fetch_add(1, Ordering::Relaxed) as u128) * 7919
}

async fn open(dsn: &str) -> Option<DB> {
    match DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

#[tokio::test]
async fn exploration_handle_root_round_trip() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let description = format!("desc-{stamp}");
    let goal = format!("goal-{stamp}");

    let id = d
        .create_exploration(&description, &goal)
        .await
        .expect("create");
    assert!(id > 0, "new exploration id positive");

    let store = d.exploration(id);
    assert_eq!(store.id(), id, "handle bound to the id");

    let (got_desc, got_goal) = store.root().await.expect("root");
    assert_eq!(got_desc, description);
    assert_eq!(got_goal, goal);

    // A NULL description reads back as "" (Go sql.NullString.String).
    d.execute(
        "UPDATE explorations SET description=NULL WHERE id=$1",
        &[&id],
    )
    .await
    .expect("null description");
    let (empty_desc, goal2) = store.root().await.expect("root null");
    assert_eq!(empty_desc, "", "null description -> empty string");
    assert_eq!(goal2, goal, "goal unchanged");

    // A handle to a missing id errors on root (QueryRow -> no rows).
    let missing = d.exploration(i64::MAX);
    assert!(
        missing.root().await.is_err(),
        "missing exploration root must error"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&id])
        .await
        .expect("cleanup");
}
