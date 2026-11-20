//! Differential test for `ExplorationStore::SoftDeleteIntent` — fresh spec for
//! the soft-delete path in `db/exploration.go`. Verifies state='deleted' +
//! delete_reason, summary extraction, the deletable-state guard
//! (ERR_INTENT_STATE_CONFLICT), and side-question-session cleanup.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::ERR_INTENT_STATE_CONFLICT;
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

#[tokio::test]
async fn soft_delete_intent() {
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
        .create_exploration("desc", &format!("t-softdel-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- Deletes an open intent: state='deleted', delete_reason set, summary
    // returned. Side-question sessions for the intent are removed.
    let intent_id = store
        .add_intent(
            &json!({"goal": "g", "summary": "the prior conclusion"}),
            3,
            &[],
            "planner",
        )
        .await
        .expect("add intent");
    // Seed a side_question_session bound to this intent. The table's XOR CHECK
    // requires (task_id AND exploration_id) NOT NULL with conversation_id NULL;
    // intent_id rides alongside them, exactly as the Go snapshot writer sets it.
    let task_id: i64 = d
        .query_one(
            "INSERT INTO tasks(description, goal, exploration_id) VALUES ('d','g',$1) RETURNING id",
            &[&exp_id],
        )
        .await
        .expect("seed task")
        .get(0);
    let n_sessions = d
        .execute(
            "INSERT INTO side_question_sessions(session_key, task_id, exploration_id, intent_id, run_id, version, snapshot) \
         VALUES ($1, $2, $3, $4, 1, 1, '{}'::jsonb)",
            &[&format!("sqs-{stamp}"), &task_id, &exp_id, &intent_id],
        )
        .await
        .expect("seed session");
    assert_eq!(n_sessions, 1);

    let summary = store
        .soft_delete_intent(intent_id, "user changed their mind")
        .await
        .expect("soft delete");
    assert_eq!(
        summary, "the prior conclusion",
        "summary from before deletion"
    );

    // Node still exists, now soft-deleted, with the reason + stamped completed_at.
    let node = store
        .get_node(intent_id)
        .await
        .expect("get")
        .expect("node kept after soft delete");
    assert_eq!(node.kind, "intent");
    assert_eq!(node.state, "deleted");
    assert_eq!(node.delete_reason, "user changed their mind");
    let completed: bool = d
        .query_one(
            "SELECT completed_at IS NOT NULL FROM exploration_nodes WHERE id=$1",
            &[&intent_id],
        )
        .await
        .expect("completed_at")
        .get(0);
    assert!(completed, "soft delete stamps completed_at");

    // Side-question session was cleaned up with the intent.
    let left: i64 = d
        .query_one(
            "SELECT COUNT(*) FROM side_question_sessions WHERE intent_id=$1",
            &[&intent_id],
        )
        .await
        .expect("count sessions")
        .get(0);
    assert_eq!(
        left, 0,
        "side-question sessions removed with the deleted intent"
    );

    // ---- A non-deletable state (done) errors with the conflict sentinel and
    // leaves the node untouched.
    let done_id = store
        .add_intent(&json!({"goal": "h"}), 1, &[], "planner")
        .await
        .expect("add done intent");
    store
        .set_intent_state(done_id, "done")
        .await
        .expect("to done");
    let err = store
        .soft_delete_intent(done_id, "nope")
        .await
        .expect_err("done intent cannot be soft-deleted");
    assert!(
        err.is(ERR_INTENT_STATE_CONFLICT),
        "conflict sentinel, got: {err:?}"
    );
    let still = store
        .get_node(done_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(
        still.state, "done",
        "failed soft delete left state unchanged"
    );
    assert!(
        still.delete_reason.is_empty(),
        "no delete_reason on a failed delete"
    );

    // ---- A paused intent is deletable (running/paused/open all allowed).
    let paused_id = store
        .add_intent(&json!({"goal": "p"}), 1, &[], "planner")
        .await
        .expect("add paused intent");
    store
        .set_intent_state(paused_id, "paused")
        .await
        .expect("to paused");
    store
        .soft_delete_intent(paused_id, "cancel paused")
        .await
        .expect("paused is deletable");
    assert_eq!(
        store
            .get_node(paused_id)
            .await
            .expect("get")
            .expect("present")
            .state,
        "deleted"
    );

    // ---- A missing intent id errors "intent not found" (plain, no sentinel).
    let missing = 1_i64 << 60;
    let err = store
        .soft_delete_intent(missing, "x")
        .await
        .expect_err("missing intent errors");
    assert!(
        err.to_string().contains("intent not found"),
        "not-found message, got: {err:?}"
    );
    assert!(
        !err.is(ERR_INTENT_STATE_CONFLICT),
        "not-found is not a state conflict"
    );

    // Cleanup: the task first (tasks.exploration_id is ON DELETE RESTRICT);
    // deleting it cascades its side-question sessions, then drop the exploration.
    d.execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await
        .expect("clean task");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
