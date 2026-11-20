//! Differential tests for the exploration activity trace + search — fresh spec for
//! the `*ExplorationStore` trace methods in `db/exploration.go` (ActivityTrace /
//! ActivityTraceForTerminalIntent / ActivityTraceSearch /
//! ActivityTraceSearchForTerminalIntent / ActivityTraceSearchTerminalIntents /
//! ActivityTraceSearchExcluding).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::Activity;
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

async fn append(
    store: &artex::db::exploration::ExplorationStore,
    node_id: Option<i64>,
    worker: &str,
    kind: &str,
    summary: &str,
    detail: &str,
) -> i64 {
    store
        .append_activity(Activity::new(
            node_id,
            worker,
            kind,
            "",
            "",
            false,
            summary,
            detail,
            json!(null),
            None,
            None,
            None,
            None,
            None,
        ))
        .await
        .expect("append")
}

fn ids(acts: &[Activity]) -> Vec<i64> {
    acts.iter().map(|a| a.id).collect()
}

#[tokio::test]
async fn exploration_activity_trace_search() {
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
        .create_exploration("desc", &format!("t-trace-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    let done = store
        .add_intent(&json!({"summary": "done"}), 1, &[], "planner")
        .await
        .expect("done");
    let live = store
        .add_intent(&json!({"summary": "live"}), 1, &[], "planner")
        .await
        .expect("live");
    store
        .set_intent_state(done, "done")
        .await
        .expect("done state");

    // done intent: a result + a thinking row (thinking excluded from trace).
    let r1 = append(
        &store,
        Some(done),
        "w1",
        "result",
        "found SQL injection",
        "detail one",
    )
    .await;
    let _think = append(
        &store,
        Some(done),
        "w1",
        "thinking",
        "internal reasoning",
        "",
    )
    .await;
    let r2 = append(
        &store,
        Some(done),
        "w1",
        "result",
        "confirmed SQL injection",
        "detail two",
    )
    .await;

    // live intent: a result (not terminal).
    let _r3 = append(
        &store,
        Some(live),
        "w2",
        "result",
        "live finding",
        "live detail",
    )
    .await;

    // node-less planner row (not a worker trace).
    let _plan = append(
        &store,
        None,
        "planner",
        "result",
        "plan step",
        "plan detail",
    )
    .await;

    // ---- ActivityTrace: done intent steps, no thinking, ascending.
    let trace = store.activity_trace(done, 0).await.expect("trace");
    assert_eq!(ids(&trace), vec![r1, r2], "no thinking, ascending");
    assert!(
        trace.iter().all(|a| a.summary.contains("SQL")),
        "summaries present"
    );
    // Trace rows have no metadata/detail (summary-only).
    assert!(trace[0].detail.is_empty(), "detail not loaded in trace");
    assert!(trace[0].metadata.is_null(), "metadata not loaded in trace");

    // ---- ActivityTrace limit caps.
    let capped = store.activity_trace(done, 1).await.expect("trace capped");
    assert_eq!(capped.len(), 1);

    // ---- ActivityTraceForTerminalIntent: only terminal, no thinking.
    let tt = store
        .activity_trace_for_terminal_intent(done, 0)
        .await
        .expect("term trace");
    assert_eq!(ids(&tt), vec![r1, r2], "terminal trace");
    let tt_live = store
        .activity_trace_for_terminal_intent(live, 0)
        .await
        .expect("live term trace");
    assert!(tt_live.is_empty(), "live intent not terminal -> empty");

    // ---- ActivityTraceSearch: scoped to done intent, summary OR detail match.
    let found = store
        .activity_trace_search(Some(done), "sql", 0)
        .await
        .expect("search sql");
    assert_eq!(ids(&found), vec![r1, r2], "both match 'sql' in summary");
    let found_detail = store
        .activity_trace_search(Some(done), "detail two", 0)
        .await
        .expect("search detail");
    assert_eq!(ids(&found_detail), vec![r2], "detail match on r2");
    // Case-insensitive.
    let ci = store
        .activity_trace_search(Some(done), "SQL", 0)
        .await
        .expect("search case-insensitive");
    assert_eq!(ids(&ci), vec![r1, r2], "case-insensitive");

    // ---- ActivityTraceSearch (all workers): node_id IS NOT NULL.
    let all = store
        .activity_trace_search(None, "finding", 0)
        .await
        .expect("search all");
    // r1/r2 don't contain "finding" in summary; r3 (live) has "live finding".
    assert_eq!(
        ids(&all),
        vec![_r3],
        "only live worker row matches 'finding'"
    );
    // Plan row (node-less) is excluded.
    assert!(!ids(&all).contains(&_plan), "node-less planner excluded");

    // ---- ActivityTraceSearchForTerminalIntent: scoped terminal.
    let tts = store
        .activity_trace_search_for_terminal_intent(done, "sql", 0)
        .await
        .expect("term search sql");
    assert_eq!(ids(&tts), vec![r1, r2], "terminal search");
    let tts_live = store
        .activity_trace_search_for_terminal_intent(live, "finding", 0)
        .await
        .expect("term search live");
    assert!(tts_live.is_empty(), "live intent not terminal -> empty");

    // ---- ActivityTraceSearchTerminalIntents: all terminal intents, no planner/main.
    let all_term = store
        .activity_trace_search_terminal_intents("sql", 0)
        .await
        .expect("search terminal all");
    assert_eq!(ids(&all_term), vec![r1, r2], "terminal worker traces only");
    // Live intent's result doesn't match "sql" anyway, but even if it did it'd be excluded.

    // ---- ActivityTraceSearchExcluding: exclude done intent's own trace.
    let excl = store
        .activity_trace_search_excluding(done, "finding", 0)
        .await
        .expect("search excluding");
    // Excludes done's rows; live's "live finding" matches.
    assert_eq!(ids(&excl), vec![_r3], "done excluded, live included");
    // exclude <= 0 -> no exclusion (searches all workers).
    let no_excl = store
        .activity_trace_search_excluding(0, "finding", 0)
        .await
        .expect("search no excl");
    assert_eq!(
        ids(&no_excl),
        vec![_r3],
        "no exclusion -> same as all-worker search"
    );

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
