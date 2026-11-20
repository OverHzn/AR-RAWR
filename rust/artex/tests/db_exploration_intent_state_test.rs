//! Differential tests for the exploration intent state-transition methods —
//! fresh spec for the `*ExplorationStore` intent lifecycle in
//! `db/exploration.go` (SetIntentState / CompareAndSetIntentState / Reopen* /
//! ResetRunningIntents / Count*).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::DB;
use serde_json::json;

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

/// Fetch a single column (as text) for a node, or None.
async fn node_state(d: &DB, id: i64) -> Option<String> {
    d.query_opt("SELECT state FROM exploration_nodes WHERE id=$1", &[&id])
        .await
        .expect("state query")
        .map(|r| r.get(0))
}

async fn completed_at_null(d: &DB, id: i64) -> bool {
    let row = d
        .query_one(
            "SELECT completed_at IS NULL FROM exploration_nodes WHERE id=$1",
            &[&id],
        )
        .await
        .expect("completed_at query");
    row.get(0)
}

#[tokio::test]
async fn intent_state_transitions() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let exp_id = d
        .create_exploration("desc", &format!("t-intent-state-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- SetIntentState: terminal stamps completed_at, non-terminal clears it.
    let i1 = store
        .add_intent(&json!({"goal": "a"}), 1, &[], "planner")
        .await
        .expect("add i1");
    assert!(
        completed_at_null(&d, i1).await,
        "fresh intent: no completed_at"
    );

    store.set_intent_state(i1, "running").await.expect("run");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("running"));
    assert!(
        completed_at_null(&d, i1).await,
        "running: completed_at NULL"
    );

    store.set_intent_state(i1, "done").await.expect("done");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("done"));
    assert!(
        !completed_at_null(&d, i1).await,
        "done: completed_at stamped"
    );

    // Reopening (open) clears completed_at again.
    store.set_intent_state(i1, "open").await.expect("reopen");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("open"));
    assert!(
        completed_at_null(&d, i1).await,
        "open: completed_at cleared"
    );

    // ---- CompareAndSetIntentState: succeeds only on a state match.
    // Wrong expected -> no change (false).
    let changed = store
        .compare_and_set_intent_state(i1, "done", "running")
        .await
        .expect("cas mismatch");
    assert!(!changed, "stale expected -> no change");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("open"));

    // Correct expected -> change (true).
    let changed = store
        .compare_and_set_intent_state(i1, "open", "running")
        .await
        .expect("cas match");
    assert!(changed, "matching expected -> change");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("running"));

    // ---- ReopenIntent: only blocked/exhausted/stopped flip to open.
    // running is left untouched -> false.
    let reopened = store.reopen_intent(i1).await.expect("reopen running");
    assert!(!reopened, "running intent is not reopened");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("running"));

    store.set_intent_state(i1, "blocked").await.expect("block");
    let reopened = store.reopen_intent(i1).await.expect("reopen blocked");
    assert!(reopened, "blocked intent reopens");
    assert_eq!(node_state(&d, i1).await.as_deref(), Some("open"));

    // ---- CountOpenIntents / CountFinishedIntents.
    // Current: i1 open. Build a known mix.
    let i2 = store
        .add_intent(&json!({"goal": "b"}), 1, &[], "planner")
        .await
        .expect("add i2"); // open
    let i3 = store
        .add_intent(&json!({"goal": "c"}), 1, &[], "planner")
        .await
        .expect("add i3");
    store.set_intent_state(i3, "done").await.expect("i3 done");
    let i4 = store
        .add_intent(&json!({"goal": "d"}), 1, &[], "planner")
        .await
        .expect("add i4");
    store
        .set_intent_state(i4, "exhausted")
        .await
        .expect("i4 exhausted");
    let i5 = store
        .add_intent(&json!({"goal": "e"}), 1, &[], "planner")
        .await
        .expect("add i5");
    store
        .set_intent_state(i5, "stopped")
        .await
        .expect("i5 stopped");

    // open: i1, i2 -> 2 ; finished(done/blocked/exhausted): i3, i4 -> 2 (stopped excluded).
    assert_eq!(store.count_open_intents().await.expect("open"), 2);
    assert_eq!(store.count_finished_intents().await.expect("fin"), 2);

    // ---- ReopenBlockedIntents: flip every blocked intent back to open.
    // Make i2, i3 blocked; reopen_blocked flips both; stopped i5 untouched.
    store
        .set_intent_state(i2, "blocked")
        .await
        .expect("i2 block");
    store
        .set_intent_state(i3, "blocked")
        .await
        .expect("i3 block");
    let n = store
        .reopen_blocked_intents()
        .await
        .expect("reopen blocked");
    assert_eq!(n, 2, "both blocked intents reopen");
    assert_eq!(node_state(&d, i2).await.as_deref(), Some("open"));
    assert_eq!(node_state(&d, i3).await.as_deref(), Some("open"));
    assert_eq!(node_state(&d, i5).await.as_deref(), Some("stopped"));

    // ---- ResetRunningIntents: take over expired/NULL-heartbeat running claims.
    // Put i1 back into running with an aged heartbeat -> taken over.
    store.set_intent_state(i1, "running").await.expect("i1 run");
    // Seed a fresh running intent with a *live* heartbeat -> left alone.
    let i6 = store
        .add_intent(&json!({"goal": "f"}), 1, &[], "planner")
        .await
        .expect("add i6");
    store.set_intent_state(i6, "running").await.expect("i6 run");
    d.execute(
        "UPDATE exploration_nodes SET last_heartbeat_at=now() WHERE id=$1",
        &[&i6],
    )
    .await
    .expect("fresh heartbeat");
    // Age i1's heartbeat far past the TTL to be unambiguous.
    d.execute(
        "UPDATE exploration_nodes SET last_heartbeat_at=now() - INTERVAL '1 hour' WHERE id=$1",
        &[&i1],
    )
    .await
    .expect("age i1");

    let taken = store.reset_running_intents().await.expect("reset");
    assert_eq!(
        taken, 1,
        "only the expired/NULL-heartbeat claim is taken over"
    );
    assert_eq!(
        node_state(&d, i1).await.as_deref(),
        Some("open"),
        "i1 reopened"
    );
    assert_eq!(
        node_state(&d, i6).await.as_deref(),
        Some("running"),
        "live claim untouched"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}

#[tokio::test]
async fn set_intent_state_scopes_to_intent_kind() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let exp_id = d
        .create_exploration("desc", &format!("t-intent-scope-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // A goal node: SetIntentState targets kind='intent' only, so it must not move.
    let goal_id = store
        .add_goal(&json!({"text": "g"}), "planner")
        .await
        .expect("goal");
    store
        .set_intent_state(goal_id, "done")
        .await
        .expect("set on a goal (no-op)");
    assert_eq!(
        node_state(&d, goal_id).await.as_deref(),
        Some("open"),
        "goal state unchanged (WHERE kind='intent')"
    );

    // An intent of a *different* exploration is not touched either.
    let other = d
        .create_exploration("d", &format!("t-intent-other-{stamp}"))
        .await
        .expect("other");
    let other_store = d.exploration(other);
    let foreign = other_store
        .add_intent(&json!({"goal": "x"}), 1, &[], "planner")
        .await
        .expect("foreign intent");
    store
        .set_intent_state(foreign, "done")
        .await
        .expect("set on foreign intent (no-op)");
    assert_eq!(
        node_state(&d, foreign).await.as_deref(),
        Some("open"),
        "foreign intent unchanged (WHERE exploration_id=...)"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&other])
        .await
        .expect("clean other");
}
