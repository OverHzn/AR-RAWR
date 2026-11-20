//! Differential tests for the exploration finding reads — fresh spec for the
//! `*ExplorationStore` finding methods in `db/exploration.go` (FindingLineage /
//! FindingIntents / FindingIntentsTerminal).
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
async fn exploration_finding_reads() {
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
        .create_exploration("desc", &format!("t-finding-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // Graph:
    //   goal --spawns--> intentA --yields--> finding1
    //                       intentA --yields--> finding2
    //   intentB --yields--> finding3 (intentB left open)
    //   intentC --yields--> finding4 (intentC done)
    let goal = store
        .add_goal(&json!({"text": "g"}), "planner")
        .await
        .expect("goal");
    let ia = store
        .add_intent(&json!({"summary": "a"}), 1, &[], "planner")
        .await
        .expect("ia");
    let ib = store
        .add_intent(&json!({"summary": "b"}), 1, &[], "planner")
        .await
        .expect("ib");
    let ic = store
        .add_intent(&json!({"summary": "c"}), 1, &[], "planner")
        .await
        .expect("ic");
    let f1 = store
        .add_node(
            "finding",
            &json!({"summary": "f1"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("f1");
    let f2 = store
        .add_node(
            "finding",
            &json!({"summary": "f2"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("f2");
    let f3 = store
        .add_node(
            "finding",
            &json!({"summary": "f3"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("f3");
    let f4 = store
        .add_node(
            "finding",
            &json!({"summary": "f4"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("f4");

    store.link(goal, "spawns", ia).await.expect("l-ga");
    store.link(ia, "yields", f1).await.expect("l-af1");
    store.link(ia, "yields", f2).await.expect("l-af2");
    store.link(ib, "yields", f3).await.expect("l-bf3");
    store.link(ic, "yields", f4).await.expect("l-cf4");

    // ---- FindingIntents: every finding -> producing intent.
    let m = store.finding_intents().await.expect("finding intents");
    assert_eq!(m.len(), 4);
    assert_eq!(m.get(&f1), Some(&ia));
    assert_eq!(m.get(&f2), Some(&ia));
    assert_eq!(m.get(&f3), Some(&ib));
    assert_eq!(m.get(&f4), Some(&ic));

    // ---- FindingIntentsTerminal: only findings whose producing intent is terminal.
    // None of the intents are terminal yet.
    assert!(store
        .finding_intents_terminal()
        .await
        .expect("terminal none")
        .is_empty());
    store.set_intent_state(ic, "done").await.expect("ic done");
    store
        .set_intent_state(ib, "blocked")
        .await
        .expect("ib blocked");
    let t = store
        .finding_intents_terminal()
        .await
        .expect("terminal some");
    assert_eq!(t.len(), 2, "only ic (done) + ib (blocked) count");
    assert_eq!(t.get(&f4), Some(&ic), "f4 from done intent");
    assert_eq!(t.get(&f3), Some(&ib), "f3 from blocked intent");
    assert!(!t.contains_key(&f1), "f1 from still-open intentA excluded");
    assert!(!t.contains_key(&f2));

    // ---- FindingLineage of a leaf finding: the finding + its producing intent +
    //      that intent's goal ancestor, and the internal edges only.
    let (nodes, edges) = store.finding_lineage(f1).await.expect("lineage f1");
    let ids: Vec<i64> = nodes.iter().map(|n| n.id).collect();
    assert!(
        ids.contains(&f1) && ids.contains(&ia) && ids.contains(&goal),
        "lineage reaches goal via intentA: {ids:?}"
    );
    // f2's branch is NOT an ancestor of f1 (siblings), so f2 is excluded.
    assert!(!ids.contains(&f2), "sibling fact f2 not in f1 lineage");
    assert!(
        !ids.contains(&ib) && !ids.contains(&ic),
        "other intents excluded"
    );
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "lineage nodes id-ordered"
    );
    // Internal edges: goal->ia, ia->f1, ia->f2? f2 is not in the ancestor set, so
    // only edges with BOTH ends in {goal, ia, f1} survive.
    for e in &edges {
        assert!(
            ids.contains(&e.from) && ids.contains(&e.to),
            "edge {:?} both ends in lineage",
            e
        );
    }
    assert!(
        edges
            .iter()
            .any(|e| e.from == goal && e.rel == "spawns" && e.to == ia),
        "goal->ia edge present"
    );
    assert!(
        edges
            .iter()
            .any(|e| e.from == ia && e.rel == "yields" && e.to == f1),
        "ia->f1 edge present"
    );
    assert!(
        !edges.iter().any(|e| e.to == f2),
        "ia->f2 edge excluded (f2 not an ancestor)"
    );

    // ---- FindingLineage of a node with no ancestors beyond itself.
    let (nodes, edges) = store.finding_lineage(goal).await.expect("lineage goal");
    assert_eq!(nodes.len(), 1, "goal is a root; lineage is just itself");
    assert_eq!(nodes[0].id, goal);
    assert!(edges.is_empty(), "no internal edges for a lone root");

    // ---- FindingLineage of a missing node -> empty.
    let (nodes, edges) = store.finding_lineage(999_999_999).await.expect("missing");
    assert!(nodes.is_empty(), "missing node -> no nodes");
    assert!(edges.is_empty(), "missing node -> no edges");

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
