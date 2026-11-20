//! Differential tests for the exploration stats + token aggregation — fresh spec
//! for the `*ExplorationStore` stat methods in `db/exploration.go` (Stats /
//! TokenTotal / TokenStatsByWorker / TokenStatsBySession).
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

/// Insert one `result` activity row with explicit token counts. `main_seg` is
/// `Option` (only main-agent rows carry a segment). Tokens bind as i32 (int4).
#[allow(clippy::too_many_arguments)]
async fn seed_result(
    d: &DB,
    exp_id: i64,
    worker: &str,
    node_id: Option<i64>,
    main_seg: Option<i32>,
    input: i32,
    output: i32,
    cache_read: i32,
    cache_write: i32,
) {
    d.execute(
        "INSERT INTO activity(exploration_id, node_id, worker, kind, summary, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, main_seg) \
         VALUES ($1,$2,$3,'result','seed',$4,$5,$6,$7,$8)",
        &[
            &exp_id,
            &node_id,
            &worker,
            &input,
            &output,
            &cache_read,
            &cache_write,
            &main_seg,
        ],
    )
    .await
    .expect("seed result");
}

#[tokio::test]
async fn exploration_stats_and_tokens() {
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
        .create_exploration("desc", &format!("t-stats-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

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
    let fact = store
        .add_node("fact", &json!({"n": 1}), 0, "confirmed", "w", &[])
        .await
        .expect("fact");

    // ---- Stats on a fresh exploration (1 goal + 2 intents + 1 fact).
    let stats = store.stats().await.expect("stats");
    assert_eq!(stats.get("goal"), Some(&1));
    assert_eq!(stats.get("intent"), Some(&2));
    assert_eq!(stats.get("fact"), Some(&1));
    assert_eq!(stats.get("begin"), None, "no begin nodes");

    // ---- Seed result activity rows.
    // mainagent, two segments.
    seed_result(&d, exp_id, "mainagent", None, Some(1), 100, 50, 10, 5).await;
    seed_result(&d, exp_id, "mainagent", None, Some(2), 200, 60, 20, 6).await;
    // planner (fixed key).
    seed_result(&d, exp_id, "planner", None, None, 10, 10, 1, 1).await;
    // two worker runs bound to intent nodes.
    seed_result(&d, exp_id, "w1", Some(ia), None, 7, 7, 7, 7).await;
    seed_result(&d, exp_id, "w2", Some(ib), None, 3, 3, 3, 3).await;
    // worker row bound to a non-intent node: counts toward worker/total, excluded
    // from session stats (the LEFT JOIN requires an intent node).
    seed_result(&d, exp_id, "w1", Some(fact), None, 7, 7, 7, 7).await;
    // A non-result row with tokens must be ignored by every token aggregate.
    d.execute(
        "INSERT INTO activity(exploration_id, worker, kind, summary, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) \
         VALUES ($1,'mainagent','note','x',999,999,999,999)",
        &[&exp_id],
    )
    .await
    .expect("seed note");

    // ---- TokenTotal: whole-task sum over kind='result' rows only.
    let total = store.token_total().await.expect("total");
    assert_eq!(total.input, 327, "100+200+10+7+3+7 (note excluded)");
    assert_eq!(total.output, 137);
    assert_eq!(total.cache_read, 48);
    assert_eq!(total.cache_write, 29);
    assert_eq!(total.worker, "", "ungrouped total has no worker");

    // ---- TokenStatsByWorker: grouped, ordered by worker.
    let by_worker = store.token_stats_by_worker().await.expect("by worker");
    let map: std::collections::HashMap<&str, (i64, i64, i64, i64)> = by_worker
        .iter()
        .map(|u| {
            (
                u.worker.as_str(),
                (u.input, u.output, u.cache_read, u.cache_write),
            )
        })
        .collect();
    assert_eq!(
        map.get("mainagent"),
        Some(&(300, 110, 30, 11)),
        "two main segs"
    );
    assert_eq!(map.get("planner"), Some(&(10, 10, 1, 1)));
    assert_eq!(
        map.get("w1"),
        Some(&(14, 14, 14, 14)),
        "w1: intent + fact rows"
    );
    assert_eq!(map.get("w2"), Some(&(3, 3, 3, 3)));
    // Ordered by worker ascending.
    let workers: Vec<&str> = by_worker.iter().map(|u| u.worker.as_str()).collect();
    let mut sorted = workers.clone();
    sorted.sort_unstable();
    assert_eq!(workers, sorted, "by_worker ordered by worker");

    // ---- TokenStatsBySession: session keys.
    let by_session = store.token_stats_by_session().await.expect("by session");
    let smap: std::collections::HashMap<&str, (i64, i64, i64, i64)> = by_session
        .iter()
        .map(|u| {
            (
                u.session.as_str(),
                (u.input, u.output, u.cache_read, u.cache_write),
            )
        })
        .collect();
    assert_eq!(smap.get("main:1"), Some(&(100, 50, 10, 5)));
    assert_eq!(smap.get("main:2"), Some(&(200, 60, 20, 6)));
    assert_eq!(smap.get("plan"), Some(&(10, 10, 1, 1)));
    assert_eq!(
        smap.get(&format!("intent:{ia}").as_str()),
        Some(&(7, 7, 7, 7))
    );
    assert_eq!(
        smap.get(&format!("intent:{ib}").as_str()),
        Some(&(3, 3, 3, 3))
    );
    // The fact-bound worker row is NOT a session (no intent node) -> excluded.
    assert_eq!(smap.len(), 5, "5 session buckets, fact row excluded");
    assert!(!smap.contains_key(""), "no empty session key");
    let sessions: Vec<&str> = by_session.iter().map(|u| u.session.as_str()).collect();
    let mut sorted = sessions.clone();
    sorted.sort_unstable();
    assert_eq!(sessions, sorted, "by_session ordered by session_key");

    // Sanity: stats unchanged by activity rows (they touch no nodes).
    let stats = store.stats().await.expect("stats2");
    assert_eq!(stats.get("intent"), Some(&2));
    let _ = goal; // silence unused

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
