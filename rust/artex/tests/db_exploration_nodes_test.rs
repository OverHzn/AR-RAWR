//! Differential tests for the exploration graph node methods — fresh spec for the
//! `*ExplorationStore` node methods in `db/exploration.go` (the Go package's node
//! tests live elsewhere; this covers the core AddNode/GetNode/state/anchor/link
//! surface directly).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Cleans up via cascade on the seeded exploration.

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
async fn exploration_nodes_crud() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let goal = format!("t-node-goal-{stamp}");
    let exp_id = d
        .create_exploration("desc", &goal)
        .await
        .expect("create exploration");
    let store = d.exploration(exp_id);

    // AddGoal: a goal node, state open, priority 0.
    let goal_id = store
        .add_goal(&json!({"text": "find bugs"}), "planner")
        .await
        .expect("add_goal");
    let goal_node = store
        .get_node(goal_id)
        .await
        .expect("get")
        .expect("goal present");
    assert_eq!(goal_node.kind, "goal");
    assert_eq!(goal_node.state, "open");
    assert_eq!(goal_node.priority, 0);
    assert_eq!(goal_node.origin, "planner");
    assert_eq!(
        goal_node.payload["text"], "find bugs",
        "payload round-trips"
    );
    assert!(goal_node.anchors.is_empty(), "no anchors");

    // AddIntent: open intent; empty origin defaults to "planner".
    let intent_id = store
        .add_intent(&json!({"goal": "x"}), 5, &[], "")
        .await
        .expect("add_intent");
    let intent = store
        .get_node(intent_id)
        .await
        .expect("get")
        .expect("intent present");
    assert_eq!(intent.kind, "intent");
    assert_eq!(intent.state, "open");
    assert_eq!(intent.priority, 5);
    assert_eq!(intent.origin, "planner", "empty origin -> planner");

    // exploration_anchors.asset_id FKs assets(id): anchors must be real assets.
    let asset_tag = format!("t-node-asset-{stamp}");
    let asset_id: i64 = d
        .query_one(
            "INSERT INTO assets(type, domain) VALUES ('subdomain',$1) RETURNING id",
            &[&asset_tag],
        )
        .await
        .expect("seed asset")
        .get(0);

    // AddNode with a non-fact kind + anchors: anchors recorded, no existence check
    // (the FK still requires the asset to exist, which it does).
    let hint_id = store
        .add_node("hint", &json!({"n": 1}), 0, "active", "system", &[asset_id])
        .await
        .expect("add hint with anchors");
    let hint = store
        .get_node(hint_id)
        .await
        .expect("get")
        .expect("hint present");
    assert_eq!(hint.kind, "hint");
    assert_eq!(hint.state, "active");

    // Anchor() is idempotent and ignores non-positive ids.
    store.anchor(hint_id, asset_id).await.expect("anchor");
    store
        .anchor(hint_id, asset_id)
        .await
        .expect("anchor again (idempotent)");
    store
        .anchor(0, asset_id)
        .await
        .expect("non-positive node id is a no-op");
    store
        .anchor(hint_id, -1)
        .await
        .expect("non-positive asset id is a no-op");

    // Link() adds a typed edge (idempotent).
    store
        .link(goal_id, "spawns", intent_id)
        .await
        .expect("link");
    store
        .link(goal_id, "spawns", intent_id)
        .await
        .expect("link again (idempotent)");

    // SetNodeState flips state and bumps content_version.
    store
        .set_node_state(intent_id, "running")
        .await
        .expect("set state");
    let intent2 = store
        .get_node(intent_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(intent2.state, "running");

    // OriginFactID: none yet (no fact/origin node).
    assert_eq!(
        store.origin_fact_id().await.expect("origin"),
        0,
        "no origin fact yet"
    );

    // Add a fact/origin node (no anchors -> no asset check needed).
    let fact_id = store
        .add_node("fact", &json!({"root": true}), 0, "origin", "system", &[])
        .await
        .expect("add origin fact");
    assert_eq!(
        store.origin_fact_id().await.expect("origin"),
        fact_id,
        "origin fact found"
    );

    // GetNode is scoped to this exploration: a foreign exploration can't see it.
    let other = d.create_exploration("d", "other").await.expect("other exp");
    let other_store = d.exploration(other);
    assert!(
        other_store.get_node(goal_id).await.expect("get").is_none(),
        "cross-exploration node hidden"
    );

    // Cleanup (cascade from the two explorations).
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&other])
        .await
        .expect("clean other");
    d.execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await
        .expect("clean asset");
}

#[tokio::test]
async fn fact_anchor_validates_assets() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let goal = format!("t-fact-goal-{stamp}");
    let exp_id = d.create_exploration("desc", &goal).await.expect("create");
    let store = d.exploration(exp_id);

    // Seed a real asset (subdomain) to anchor to.
    let asset_tag = format!("t-fact-asset-{stamp}");
    let asset_id: i64 = d
        .query_one(
            "INSERT INTO assets(type, domain) VALUES ('subdomain',$1) RETURNING id",
            &[&asset_tag],
        )
        .await
        .expect("seed asset")
        .get(0);

    // A fact with an existing anchor asset succeeds.
    let fact_id = store
        .add_node("fact", &json!({}), 0, "confirmed", "system", &[asset_id])
        .await
        .expect("fact with valid anchor");
    assert!(fact_id > 0);

    // A fact anchored to a non-existent asset errors (and inserts nothing).
    let before: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_nodes WHERE exploration_id=$1",
            &[&exp_id],
        )
        .await
        .expect("count")
        .get(0);
    let missing = 1_i64 << 60;
    let err = store
        .add_node("fact", &json!({}), 0, "confirmed", "system", &[missing])
        .await
        .expect_err("fact with missing anchor must error");
    assert!(
        err.to_string().contains("asset IDs do not exist"),
        "{err:?}"
    );
    let after: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_nodes WHERE exploration_id=$1",
            &[&exp_id],
        )
        .await
        .expect("count")
        .get(0);
    assert_eq!(
        before, after,
        "failed fact insert left no node (tx rolled back)"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
    d.execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await
        .expect("clean asset");
}
