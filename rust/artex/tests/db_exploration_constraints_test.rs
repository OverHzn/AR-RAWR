//! Differential tests for admission rollback + operator constraints — fresh spec
//! for the `*ExplorationStore` methods in `db/intent_admission.go`
//! (`DiscardOpenIntent`) and `db/constraints.go` (`ListConstraints` /
//! `AddConstraint` / `UpdateConstraint` / `DeleteConstraint`). Neither file ships
//! a Go `_test.go`, so this is a fresh spec against the real schema.
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
async fn exploration_constraints_and_admission_rollback() {
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
        .create_exploration("desc", &format!("t-constraint-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- AddConstraint: kind validation, default origin, return id.
    assert!(
        store.add_constraint("maybe", "x", "").await.is_err(),
        "invalid kind rejected"
    );
    let allow = store
        .add_constraint("allow", "port scanning", "")
        .await
        .expect("add allow");
    let deny = store
        .add_constraint("deny", "dos attacks", "human")
        .await
        .expect("add deny");

    // ---- ListConstraints: allow before deny, oldest first within each group.
    let list = store.list_constraints().await.expect("list");
    assert_eq!(list.len(), 2, "two constraints");
    assert_eq!(list[0].id, allow, "allow sorts before deny");
    assert_eq!(list[0].kind, "allow");
    assert_eq!(list[0].origin, "system", "empty origin -> system");
    assert_eq!(list[0].text, "port scanning");
    assert_eq!(list[1].id, deny);
    assert_eq!(list[1].kind, "deny");
    assert_eq!(list[1].origin, "human");

    // ---- UpdateConstraint: kind validation, rewrite, missing -> error.
    assert!(
        store.update_constraint(allow, "bad", "y").await.is_err(),
        "invalid kind rejected on update"
    );
    store
        .update_constraint(allow, "deny", "no scanning")
        .await
        .expect("update");
    let list = store.list_constraints().await.expect("list after update");
    let updated = list.iter().find(|c| c.id == allow).expect("still present");
    assert_eq!(updated.kind, "deny");
    assert_eq!(updated.text, "no scanning");
    assert!(
        store
            .update_constraint(9_999_999, "allow", "z")
            .await
            .is_err(),
        "update of missing constraint errors"
    );

    // ---- DeleteConstraint: missing -> error; then real delete.
    assert!(
        store.delete_constraint(9_999_999).await.is_err(),
        "delete of missing constraint errors"
    );
    store.delete_constraint(deny).await.expect("delete deny");
    let list = store.list_constraints().await.expect("list after delete");
    assert_eq!(list.len(), 1, "one constraint remains");
    assert_eq!(list[0].id, allow);

    // ---- DiscardOpenIntent: removes an open intent node (+ its activity).
    let intent = store
        .add_intent(&json!({"summary":"to discard"}), 1, &[], "planner")
        .await
        .expect("add intent");
    // Confirm the node exists.
    let before = store.list_by_kind("intent", 100).await.expect("intents");
    assert!(before.iter().any(|n| n.id == intent), "intent exists");
    store
        .discard_open_intent(intent)
        .await
        .expect("discard open intent");
    let after = store
        .list_by_kind("intent", 100)
        .await
        .expect("intents after");
    assert!(
        !after.iter().any(|n| n.id == intent),
        "intent removed by admission rollback"
    );
    // Discarding an already-gone (or non-open) intent errors (removed != 1).
    assert!(
        store.discard_open_intent(intent).await.is_err(),
        "second discard errors (removed != 1)"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
