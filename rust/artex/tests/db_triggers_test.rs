//! Differential tests for `db/triggers.go` — trigger CRUD, scheduler watermarks,
//! and the cross-exploration event queries (finding / timeout / task / tool / goal).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::triggers::{AgentTrigger, TaskEvent};

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

fn sample_trigger(key: &str) -> AgentTrigger {
    AgentTrigger {
        agent_key: key.to_string(),
        enabled: true,
        interval_sec: 60,
        on_finding: true,
        on_tool_call: true,
        finding_message: "found".to_string(),
        tool_names: vec!["bash".to_string(), "http".to_string()],
        ..AgentTrigger::default()
    }
}

#[tokio::test]
async fn trigger_crud_and_sched_state() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let key = format!("t_trig_{}", std::process::id());
    let _ = d.delete_triggers_for_agent(&key).await;

    let mut t = sample_trigger(&key);
    let created = d.create_trigger(&t).await.unwrap();
    assert!(created.id > 0);
    assert_eq!(created.interval_sec, 60);
    assert_eq!(
        created.tool_names,
        vec!["bash".to_string(), "http".to_string()]
    );
    assert!(created.last_fire.is_none());
    t.id = created.id;

    // Empty tool names stores as "" and reads back empty vec.
    let mut other = sample_trigger(&key);
    other.tool_names = Vec::new();
    other.enabled = false;
    let other_created = d.create_trigger(&other).await.unwrap();
    assert!(other_created.tool_names.is_empty());

    let list = d.list_triggers_for(&key).await.unwrap();
    assert_eq!(list.len(), 2, "two triggers for the agent");
    let enabled = d.list_enabled_triggers().await.unwrap();
    assert!(
        enabled.iter().any(|x| x.id == created.id),
        "enabled trigger listed"
    );
    assert!(
        !enabled.iter().any(|x| x.id == other_created.id),
        "disabled not listed"
    );

    // Update (not last_fire).
    t.interval_sec = 120;
    t.finding_message = "updated".to_string();
    d.update_trigger(&t).await.unwrap();
    let got = d
        .list_triggers_for(&key)
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == t.id)
        .unwrap();
    assert_eq!(got.interval_sec, 120);
    assert_eq!(got.finding_message, "updated");

    // Touch fire sets last_fire.
    d.touch_trigger_fire(t.id).await.unwrap();
    let got = d
        .list_triggers_for(&key)
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.id == t.id)
        .unwrap();
    assert!(got.last_fire.is_some(), "last_fire recorded");

    // Delete one + delete-all-for-agent.
    d.delete_trigger(other_created.id).await.unwrap();
    assert_eq!(d.list_triggers_for(&key).await.unwrap().len(), 1);
    d.delete_triggers_for_agent(&key).await.unwrap();
    assert!(d.list_triggers_for(&key).await.unwrap().is_empty());

    // Scheduler watermark: absent -> "", set -> value, overwrite.
    let skey = format!("sched-{}", std::process::id());
    assert_eq!(d.get_sched_state(&skey).await.unwrap(), "");
    d.set_sched_state(&skey, "100").await.unwrap();
    assert_eq!(d.get_sched_state(&skey).await.unwrap(), "100");
    d.set_sched_state(&skey, "200").await.unwrap();
    assert_eq!(d.get_sched_state(&skey).await.unwrap(), "200");
    let _ = d
        .execute("DELETE FROM scheduler_state WHERE key=$1", &[&skey])
        .await;
}

#[tokio::test]
async fn event_queries_watermarks_and_payloads() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let baseline = d.new_tasks_since(0).await.unwrap().len() as i64;

    let t = d
        .create_task("trig-event", "the goal", None, 0, 0)
        .await
        .unwrap();
    let node_id = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1,'finding','{\"summary\":\"s\",\"vulnclass\":\"vc\",\"severity\":\"high\"}'::jsonb,9,'confirmed','tester') RETURNING id",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let goal_id = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1,'goal','{\"text\":\"goal met\"}'::jsonb,9,'met','tester') RETURNING id",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    // NewTasksSince sees our task with id > baseline-ish watermark (use t.id-1).
    let tasks: Vec<TaskEvent> = d.new_tasks_since(t.id - 1).await.unwrap();
    assert!(
        tasks.iter().any(|e| e.task_id == t.id),
        "task event present"
    );
    assert!(
        tasks.iter().any(|e| e.summary == "the goal"),
        "goal copied to summary"
    );
    let _ = baseline;

    // NewFindingsSince reads the finding payload fields.
    let findings = d.new_findings_since(node_id - 1).await.unwrap();
    let fe = findings
        .iter()
        .find(|e| e.node_id == node_id)
        .expect("finding event");
    assert_eq!(fe.summary, "s");
    assert_eq!(fe.vuln_class, "vc");
    assert_eq!(fe.severity, "high");
    assert!(
        d.new_findings_since(node_id)
            .await
            .unwrap()
            .iter()
            .all(|e| e.node_id != node_id),
        "watermark excludes the finding itself"
    );

    // MetGoals uses payload.text (fallback summary).
    let goals = d.met_goals().await.unwrap();
    let ge = goals
        .iter()
        .find(|e| e.node_id == goal_id)
        .expect("goal event");
    assert_eq!(ge.summary, "goal met");

    // TimedOutTasksSince: flip task to timeout.
    d.execute("UPDATE tasks SET status='timeout' WHERE id=$1", &[&t.id])
        .await
        .unwrap();
    let to = d.timed_out_tasks_since(t.id - 1).await.unwrap();
    assert!(
        to.iter().any(|e| e.task_id == t.id),
        "timeout event present"
    );

    // NewToolCallsSince: a tool_result activity with a paired tool_use.
    let store = d.exploration(t.exploration_id);
    let use_id = store
        .append_activity(artex::db::exploration::Activity {
            kind: "tool_use".to_string(),
            tool: "bash".to_string(),
            tool_use_id: "tu-1".to_string(),
            detail: "{\"cmd\":\"ls\"}".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();
    let result_id = store
        .append_activity(artex::db::exploration::Activity {
            kind: "tool_result".to_string(),
            tool: "bash".to_string(),
            tool_use_id: "tu-1".to_string(),
            detail: "ok".to_string(),
            is_error: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let calls = d.new_tool_calls_since(use_id - 1).await.unwrap();
    let ce = calls
        .iter()
        .find(|e| e.node_id == result_id)
        .expect("tool call event");
    assert_eq!(ce.tool, "bash");
    assert_eq!(ce.tool_input, "{\"cmd\":\"ls\"}");
    assert_eq!(ce.tool_output, "ok");
    assert!(!ce.tool_is_err);

    // Cleanup.
    d.delete_task(t.id).await.unwrap();
}
