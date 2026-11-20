//! Differential tests for the exploration frontier + goal methods — fresh spec
//! for the `*ExplorationStore` frontier / goal methods in `db/exploration.go`
//! (Frontier / HasActiveIntent / HasOpenGoal / OpenGoalFingerprint /
//! UpdateGoalPayload / DeleteGoal).
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

#[tokio::test]
async fn exploration_frontier_and_goals() {
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
        .create_exploration("desc", &format!("t-frontier-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- Frontier on an empty exploration is empty; no goals yet.
    assert!(store.frontier(0).await.expect("frontier empty").is_empty());
    assert!(!store.has_active_intent().await.expect("no active"));
    assert!(!store.has_open_goal().await.expect("no goal"));
    assert_eq!(
        store.open_goal_fingerprint().await.expect("fp empty"),
        "",
        "no open goals -> empty fingerprint"
    );

    // ---- Two goals; one is later closed so only one stays open.
    let g1 = store
        .add_goal(&json!({"text": "goal one"}), "planner")
        .await
        .expect("g1");
    let g2 = store
        .add_goal(&json!({"text": "goal two"}), "planner")
        .await
        .expect("g2");
    assert!(store.has_open_goal().await.expect("open goal"));
    let fp = store.open_goal_fingerprint().await.expect("fp");
    assert_eq!(fp, format!("{g1},{g2}"), "sorted goal ids joined by comma");

    // ---- Intents with mixed priority; Frontier is priority DESC then id ASC.
    let lo = store
        .add_intent(&json!({"summary": "lo"}), 1, &[], "planner")
        .await
        .expect("lo");
    let hi_a = store
        .add_intent(&json!({"summary": "hi-a"}), 9, &[], "planner")
        .await
        .expect("hi_a");
    let hi_b = store
        .add_intent(&json!({"summary": "hi-b"}), 9, &[], "planner")
        .await
        .expect("hi_b");

    let frontier = store.frontier(0).await.expect("frontier");
    assert_eq!(frontier.len(), 3, "three open intents");
    // priority DESC: hi_a/hi_b (9) first (id ASC tie-break), then lo (1).
    assert_eq!(frontier[0].id, hi_a, "highest priority, lowest id first");
    assert_eq!(frontier[1].id, hi_b, "priority tie broken by id asc");
    assert_eq!(frontier[2].id, lo, "lowest priority last");
    assert!(store.has_active_intent().await.expect("active"));

    // ---- Frontier limit caps the result.
    let capped = store.frontier(1).await.expect("frontier capped");
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].id, hi_a);

    // ---- Closing the only open goal changes the fingerprint + has_open_goal.
    store.set_node_state(g1, "met").await.expect("close g1");
    assert!(store.has_open_goal().await.expect("g2 still open"));
    assert_eq!(
        store.open_goal_fingerprint().await.expect("fp g2"),
        g2.to_string(),
        "only the still-open goal remains"
    );

    // ---- UpdateGoalPayload replaces payload; vulnclass only when non-empty.
    store
        .update_goal_payload(g2, "rewritten goal", "sqli")
        .await
        .expect("update");
    let node = store.get_node(g2).await.expect("get").expect("g2 exists");
    assert_eq!(node.payload["text"], "rewritten goal");
    assert_eq!(node.payload["vulnclass"], "sqli");
    // Empty vulnclass omits the key.
    store
        .update_goal_payload(g2, "no class", "")
        .await
        .expect("update2");
    let node = store.get_node(g2).await.expect("get").expect("g2 exists");
    assert_eq!(node.payload["text"], "no class");
    assert!(
        node.payload.get("vulnclass").is_none(),
        "empty vulnclass omitted"
    );

    // ---- UpdateGoalPayload on a non-goal id errors.
    let err = store
        .update_goal_payload(lo, "x", "")
        .await
        .expect_err("intent is not a goal");
    assert!(
        err.to_string().contains("goal does not exist"),
        "msg: {err}"
    );

    // ---- DeleteGoal hard-deletes the goal, and only a goal (scoped to kind).
    store.delete_goal(g2).await.expect("delete g2");
    assert!(
        store.get_node(g2).await.expect("get g2").is_none(),
        "g2 gone"
    );
    assert!(
        !store.has_open_goal().await.expect("no open goal"),
        "g2 was last open"
    );
    // Deleting a non-goal id is rejected (kind guard) and leaves the intent.
    let err = store
        .delete_goal(lo)
        .await
        .expect_err("intent not deletable as goal");
    assert!(
        err.to_string().contains("goal does not exist"),
        "msg: {err}"
    );
    assert!(
        store.get_node(lo).await.expect("get lo").is_some(),
        "intent survives"
    );
    // Deleting an already-gone goal errors.
    let err = store.delete_goal(g2).await.expect_err("already gone");
    assert!(err.to_string().contains("goal does not exist"));

    // ---- has_active_intent reflects a running intent too (not only open).
    store
        .set_intent_state(lo, "running")
        .await
        .expect("lo running");
    assert!(store
        .has_active_intent()
        .await
        .expect("running counts active"));

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
