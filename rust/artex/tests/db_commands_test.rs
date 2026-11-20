//! Differential tests for `db/commands.go` — command (tool_use/result) list + tally
//! and the LLM-record log (insert/list/tasks/delete/get).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::commands::{CommandRecord, LLMRecord};

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

#[tokio::test]
async fn command_list_and_tool_stats() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("commands", "goal", None, 0, 0).await.unwrap();
    let store = d.exploration(t.exploration_id);
    // Three tool_use rows, two with a paired tool_result (one error).
    for i in 0..3 {
        let tu = format!("tu-cmd-{}", std::process::id() + i);
        store
            .append_activity(artex::db::exploration::Activity {
                kind: "tool_use".to_string(),
                worker: "worker".to_string(),
                tool: if i == 2 { "http" } else { "bash" }.to_string(),
                tool_use_id: tu.clone(),
                detail: format!("{{\"cmd\":\"echo {i}\"}}"),
                ..Default::default()
            })
            .await
            .unwrap();
        if i < 2 {
            store
                .append_activity(artex::db::exploration::Activity {
                    kind: "tool_result".to_string(),
                    tool: if i == 2 { "http" } else { "bash" }.to_string(),
                    tool_use_id: tu,
                    detail: format!("output {i}"),
                    is_error: i == 1,
                    ..Default::default()
                })
                .await
                .unwrap();
        }
    }

    // list_commands for this exploration: 3 tool_use rows, newest first.
    let (all, total) = d
        .list_commands(Some(t.exploration_id), "", 0, 50)
        .await
        .unwrap();
    assert!(total >= 3, "at least our three tool uses");
    let ours: Vec<&CommandRecord> = all
        .iter()
        .filter(|c| c.exp_id == t.exploration_id)
        .collect();
    assert_eq!(ours.len(), 3);
    assert!(ours[0].id > ours[1].id, "newest first");

    // Paired output: the paired result's detail shows up (one error).
    assert!(ours.iter().any(|c| c.output == "output 0"));
    assert!(ours.iter().any(|c| c.is_error && c.output == "output 1"));

    // q filter matches tool name / input detail.
    let (filtered, filtered_total) = d
        .list_commands(Some(t.exploration_id), "echo 1", 0, 50)
        .await
        .unwrap();
    assert_eq!(filtered_total, 1, "one tool use with 'echo 1'");
    assert_eq!(filtered.len(), 1);

    // Pagination: size 2 page 1 -> second window.
    let (page1, _) = d
        .list_commands(Some(t.exploration_id), "", 1, 2)
        .await
        .unwrap();
    assert_eq!(page1.len(), 1, "page 1 (offset 2) of 3 rows -> 1 row");

    // ToolStats: bash total 2 errors 1, http total 1 errors 0.
    let stats = d.tool_stats(Some(t.exploration_id), "").await.unwrap();
    let bash = stats.iter().find(|s| s.tool == "bash").unwrap();
    assert_eq!(bash.total, 2);
    assert_eq!(bash.errors, 1);
    let http = stats.iter().find(|s| s.tool == "http").unwrap();
    assert_eq!(http.total, 1);
    assert_eq!(http.errors, 0);

    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn llm_records_insert_list_filter_get_delete() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    d.ensure_llm_records_table().await.unwrap();

    let tag = format!("cmds-{}", std::process::id());
    let mut ids = Vec::new();
    for i in 0..3 {
        let rec = LLMRecord {
            model: if i == 2 { "model-b" } else { "model-a" }.to_string(),
            profile_name: String::new(), // stored NULL
            session_id: format!("{tag}-session-{i}"),
            task_id: format!("{tag}-task"),
            worker: String::new(), // stored NULL
            latency_ms: 10 * i,
            input_tokens: 100 + i,
            output_tokens: 50 + i,
            cache_read: 5,
            cache_write: 2,
            status: "ok".to_string(),
            request_body: format!("req-{i}"),
            response_body: format!("resp-{i}"),
            raw_request: format!("rawreq-{i}"),
            raw_response: format!("rawresp-{i}"),
            ..LLMRecord::default()
        };
        d.insert_llm_record(&rec).await.unwrap();
        // Capture the last inserted id for cleanup.
        let id: i64 = d
            .query_one(
                "SELECT id FROM llm_records WHERE session_id=$1",
                &[&rec.session_id],
            )
            .await
            .unwrap()
            .get(0);
        ids.push(id);
    }

    // List by task filter (exact match on COALESCE(task_id,'')).
    let (rows, total) = d
        .list_llm_records("", "", &format!("{tag}-task"), 0, 50)
        .await
        .unwrap();
    assert_eq!(total, 3);
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|r| r.profile_name.is_empty()),
        "NULL profile -> ''"
    );
    assert!(
        rows.iter().all(|r| r.worker.is_empty()),
        "NULL worker -> ''"
    );

    // Model + session (ILIKE) filters.
    let (mrows, mtotal) = d.list_llm_records("model-b", "", "", 0, 50).await.unwrap();
    assert_eq!(mtotal, 1);
    assert!(mrows.iter().any(|r| r.model == "model-b"));
    let (_srows, stotal) = d
        .list_llm_records("", &format!("{tag}-session-1"), "", 0, 50)
        .await
        .unwrap();
    assert_eq!(stotal, 1);

    // Pagination.
    let (page, _) = d
        .list_llm_records("", "", &format!("{tag}-task"), 1, 2)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);

    // Get with full bodies.
    let got = d.get_llm_record(ids[0]).await.unwrap().unwrap();
    assert_eq!(got.request_body, "req-0");
    assert_eq!(got.raw_response, "rawresp-0");
    assert_eq!(got.input_tokens, 100);

    // LLMTasks includes our task, then delete removes all three.
    let tasks = d.llm_tasks().await.unwrap();
    let our = tasks
        .iter()
        .find(|t| t.task_id == format!("{tag}-task"))
        .unwrap();
    assert_eq!(our.count, 3);
    let deleted = d.delete_llm_records(&format!("{tag}-task")).await.unwrap();
    assert_eq!(deleted, 3);
    assert!(d.get_llm_record(ids[0]).await.unwrap().is_none());
}

#[test]
fn command_record_default_is_epoch() {
    let c = CommandRecord::default();
    assert_eq!(c.id, 0);
    assert_eq!(c.created_at, std::time::SystemTime::UNIX_EPOCH);
}
