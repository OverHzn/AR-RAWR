//! Differential tests for `db/exploration_sources.go` — source-aware exploration
//! reads (direct sources only, terminal-intent-only inherited context).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::exploration::Activity;
use artex::db::tasks::TaskCreateOptions;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<artex::db::DB> {
    match artex::db::DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

fn opts(source_ids: &[i64]) -> TaskCreateOptions {
    TaskCreateOptions {
        source_task_ids: source_ids.to_vec(),
        ..TaskCreateOptions::default()
    }
}

#[tokio::test]
async fn direct_sources_and_inherited_reads() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // grand -> source -> current. Sources of a source are NEVER expanded.
    let grand = d
        .create_task_with_options("grand", "grand goal", opts(&[]))
        .await
        .unwrap();
    let source = d
        .create_task_with_options("source", "source goal", opts(&[grand.id]))
        .await
        .unwrap();
    let current = d
        .create_task_with_options("current", "current goal", opts(&[source.id]))
        .await
        .unwrap();

    let src_store = d.exploration(source.exploration_id);
    let cur_store = d.exploration(current.exploration_id);

    // Source graph: a finding node, a DONE intent with activities, and an OPEN intent.
    let src_finding = src_store
        .add_node(
            "finding",
            &serde_json::json!({"summary":"src finding"}),
            9,
            "confirmed",
            "tester",
            &[],
        )
        .await
        .unwrap();
    d.add_finding(
        source.id,
        src_finding,
        "src-vc",
        "",
        "critical",
        "s",
        "e",
        "w",
        &[],
    )
    .await
    .unwrap();
    let done_intent = src_store
        .add_intent(
            &serde_json::json!({"summary":"source work"}),
            1,
            &[],
            "planner",
        )
        .await
        .unwrap();
    src_store
        .set_intent_state(done_intent, "done")
        .await
        .unwrap();
    let open_intent = src_store
        .add_intent(
            &serde_json::json!({"summary":"still working"}),
            1,
            &[],
            "planner",
        )
        .await
        .unwrap();

    let mut act_ids = Vec::new();
    for (kind, summary) in [
        ("text", "shared text"),
        ("thinking", "private"),
        ("result", "shared result"),
    ] {
        let id = src_store
            .append_activity(Activity {
                node_id: Some(done_intent),
                worker: "worker".to_string(),
                kind: kind.to_string(),
                summary: summary.to_string(),
                detail: format!("{summary} detail"),
                ..Activity::default()
            })
            .await
            .unwrap();
        act_ids.push(id);
    }

    // Direct sources: exactly one (source), not grand (no transitive expansion).
    let sources = cur_store.direct_source_stores().await.unwrap();
    assert_eq!(sources.len(), 1, "one direct source");
    assert_eq!(sources[0].task.task_id, source.id);
    assert_eq!(cur_store.task_id().await.unwrap(), current.id);

    // ListByKindWithSources: local (none) + inherited finding, tagged.
    let findings = cur_store
        .list_by_kind_with_sources("finding", 20)
        .await
        .unwrap();
    assert_eq!(findings.len(), 1, "one inherited finding");
    assert_eq!(findings[0].id, src_finding);
    assert!(findings[0].inherited);
    assert_eq!(findings[0].source_task_id, source.id);

    // Non-terminal inherited intents excluded; terminal ones included.
    let intents = cur_store
        .list_by_kind_with_sources("intent", 20)
        .await
        .unwrap();
    assert!(
        intents.iter().any(|n| n.id == done_intent),
        "done intent inherited"
    );
    assert!(
        !intents.iter().any(|n| n.id == open_intent),
        "open intent excluded"
    );

    // GetNodeWithSources: inherited node found, tagged; open intent hidden.
    let got = cur_store
        .get_node_with_sources(src_finding)
        .await
        .unwrap()
        .unwrap();
    assert!(got.inherited && got.source_task_id == source.id);
    assert!(cur_store
        .get_node_with_sources(open_intent)
        .await
        .unwrap()
        .is_none());

    // Terminal-intent activity reads via sources: only shared text/result rows,
    // tagged inherited. thinking is filtered by the terminal boundary.
    let trace = cur_store
        .activity_trace_with_sources(done_intent, 10)
        .await
        .unwrap();
    assert_eq!(trace.len(), 2, "text + result only");
    assert!(trace
        .iter()
        .all(|a| a.inherited && a.source_task_id == source.id));
    let (list, _) = cur_store
        .activity_list_with_sources(done_intent, 0, 10)
        .await
        .unwrap();
    assert_eq!(list.len(), 2);
    let hits = cur_store
        .activity_trace_search_with_sources(done_intent, "shared", 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2, "search matches shared text/result");
    let by_ids = cur_store
        .activity_by_ids_with_sources(&act_ids)
        .await
        .unwrap();
    assert_eq!(by_ids.len(), 2, "detail boundary excludes thinking");
    // Shared text/result details resolve via the source; private thinking is filtered.
    for &id in &[act_ids[0], act_ids[2]] {
        let detail = cur_store.activity_detail_with_sources(id).await.unwrap();
        assert!(!detail.is_empty(), "shared detail resolved via source");
    }
    assert_eq!(
        cur_store
            .activity_detail_with_sources(act_ids[1])
            .await
            .unwrap(),
        "",
        "thinking filtered from inherited detail"
    );

    // Search-all includes source terminal intents, sorted + capped.
    let all = cur_store
        .activity_trace_search_all_with_sources(0, "shared", 10)
        .await
        .unwrap();
    assert!(!all.is_empty());

    // FindingIntentsWithSources: merges terminal lineage from the source.
    let _ = cur_store.finding_intents_with_sources().await.unwrap();

    // Cleanup: delete current/source/grand (cascade nodes, activities, relations).
    d.delete_task(current.id).await.unwrap();
    d.delete_task(source.id).await.unwrap();
    d.delete_task(grand.id).await.unwrap();
}
