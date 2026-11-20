//! Differential tests for the exploration node + edge read methods — fresh spec
//! for the `*ExplorationStore` listing methods in `db/exploration.go` (Nodes /
//! ListByKind / ListByKindPage / NodesPage+NodeFilter / NodesByIDs / Edges /
//! EdgesTouching / FactsYielded).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::NodeFilter;
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
async fn exploration_node_and_edge_reads() {
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
        .create_exploration("desc", &format!("t-reads-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // Build a small graph: 1 goal, 2 intents, 2 facts yielded by intentA.
    let goal = store
        .add_goal(&json!({"text": "g"}), "planner")
        .await
        .expect("goal");
    let ia = store
        .add_intent(&json!({"summary": "plan one"}), 1, &[], "planner")
        .await
        .expect("ia");
    let ib = store
        .add_intent(&json!({"summary": "plan two"}), 1, &[], "planner")
        .await
        .expect("ib");
    let fa = store
        .add_node(
            "fact",
            &json!({"summary": "alpha finding"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("fa");
    let fb = store
        .add_node(
            "fact",
            &json!({"summary": "beta finding"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("fb");
    store.link(goal, "spawns", ia).await.expect("l1");
    store.link(goal, "spawns", ib).await.expect("l2");
    store.link(ia, "yields", fa).await.expect("l3");
    store.link(ia, "yields", fb).await.expect("l4");

    // ---- Nodes(limit): all nodes in id order; limit caps.
    let all = store.nodes(0).await.expect("nodes");
    assert_eq!(all.len(), 5, "goal + 2 intents + 2 facts = 5");
    let capped = store.nodes(2).await.expect("nodes capped");
    assert_eq!(capped.len(), 2, "limit caps the result");
    // id-ascending (creation order).
    assert!(
        all.windows(2).all(|w| w[0].id < w[1].id),
        "nodes id-ordered"
    );

    // ---- ListByKind: intents newest-first (id DESC).
    let intents = store.list_by_kind("intent", 0).await.expect("by kind");
    assert_eq!(intents.len(), 2);
    assert!(intents[0].id > intents[1].id, "newest first");
    assert!(intents.iter().all(|n| n.kind == "intent"));

    // ---- ListByKindPage: reverse pagination with hasMore.
    let (page1, more1) = store
        .list_by_kind_page("intent", 0, 1)
        .await
        .expect("page1");
    assert_eq!(page1.len(), 1);
    assert!(more1, "more older intents exist");
    assert_eq!(page1[0].id, intents[0].id, "newest intent first");
    let (page2, more2) = store
        .list_by_kind_page("intent", page1[0].id, 1)
        .await
        .expect("page2");
    assert_eq!(page2.len(), 1);
    assert!(!more2, "no older intents past the second");
    assert!(page2[0].id < page1[0].id, "older intent on the next page");

    // ---- NodesPage: filter by kind.
    let (facts, total) = store
        .nodes_page(
            NodeFilter {
                kinds: vec!["fact".to_string()],
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .expect("facts page");
    assert_eq!(total, 2, "two facts match");
    assert_eq!(facts.len(), 2);
    assert!(facts.iter().all(|n| n.kind == "fact"));

    // ---- NodesPage: keyword query (payload::text ILIKE).
    let (matched, total) = store
        .nodes_page(
            NodeFilter {
                query: "ALPHA".to_string(), // case-insensitive
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .expect("query page");
    assert_eq!(total, 1, "only the alpha fact matches");
    assert_eq!(matched[0].id, fa);

    // ---- NodesPage: "#id" exact-id jump (payload has no '#').
    let (jump, total) = store
        .nodes_page(
            NodeFilter {
                query: format!("#{fb}"),
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .expect("id jump");
    assert_eq!(total, 1, "the #id form is an exact id match");
    assert_eq!(jump[0].id, fb);

    // ---- NodesPage: state filter + asc ordering.
    store.set_intent_state(ia, "done").await.expect("ia done");
    let (asc, total) = store
        .nodes_page(
            NodeFilter {
                states: vec!["done".to_string()],
                asc: true,
                ..Default::default()
            },
            1,
            20,
        )
        .await
        .expect("asc page");
    assert_eq!(total, 1);
    assert_eq!(asc[0].id, ia);

    // ---- NodesPage: size clamping (size>200 -> 200) does not drop the total.
    let (_, total) = store
        .nodes_page(NodeFilter::default(), 1, 9999)
        .await
        .expect("clamp");
    assert_eq!(total, 5, "total is independent of page size");

    // ---- NodesByIDs: subset in id order; empty -> empty.
    let subset = store.nodes_by_ids(&[fb, goal, fa]).await.expect("by ids");
    let ids: Vec<i64> = subset.iter().map(|n| n.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "nodes_by_ids returns id order");
    assert_eq!(subset.len(), 3);
    assert!(store.nodes_by_ids(&[]).await.expect("empty").is_empty());

    // ---- FactsYielded: intentA produced fa, fb (oldest first).
    let yielded = store.facts_yielded(ia).await.expect("yielded");
    assert_eq!(yielded, vec![fa, fb]);
    assert!(store.facts_yielded(ib).await.expect("none").is_empty());

    // ---- Edges / EdgesTouching.
    let edges = store.edges(0).await.expect("edges");
    assert_eq!(edges.len(), 4, "4 links recorded");
    let touching_a = store.edges_touching(&[ia]).await.expect("touching");
    // ia --spawns--> ... no: goal->ia, ia->fa, ia->fb  => 3 edges touch ia.
    assert_eq!(touching_a.len(), 3, "three edges touch intentA");
    assert!(
        store.edges_touching(&[]).await.expect("empty").is_empty(),
        "empty ids -> empty"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
