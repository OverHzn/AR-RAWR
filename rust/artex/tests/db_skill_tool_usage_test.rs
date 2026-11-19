//! Differential tests for the metering ledger cluster — port of the Go specs
//! `db/skill_usage_test.go` (`TestSkillUsageLedger`) and `db/tool_usage_test.go`
//! (`TestToolUsageLedger`), plus an archive-merge case for
//! `mergeArchivedSkillStats`.
//!
//! Runs against `ARTEX_PG_DSN`, serialised behind one shared async lock, and skips
//! cleanly when the DSN is unset or unreachable. Rows are namespaced by a
//! process-unique skill/tool name so the shared dev DB stays usable and the test
//! cleans up after itself — the same isolation strategy as the Go tests.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::skill_usage::{SkillCall, SkillStat, SkillUsage};
use artex::db::task_archive_aggregate::{merge_archived_skill_stats, TaskArchiveAggregate};
use artex::db::tool_usage::ToolUsage;
use artex::db::DB;

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

/// `TestSkillUsageLedger`: writes a few rows and checks the three aggregates the
/// skills page relies on — per-skill totals (resolved calls only), the miss list,
/// the recent-call list, and the per-task rollup.
#[tokio::test]
async fn skill_usage_ledger() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let skill_a = format!("zz-test-skill-a-{stamp}");
    let missing = format!("zz-test-skill-missing-{stamp}");

    d.execute(
        "DELETE FROM skill_usage WHERE skill IN ($1,$2)",
        &[&skill_a, &missing],
    )
    .await
    .expect("pre-clean");

    let mk = |skill: &str,
              agent: &str,
              task: i64,
              expl: i64,
              intent: i64,
              sess: &str,
              args: i32,
              found: bool| SkillUsage {
        skill: skill.to_string(),
        agent_key: agent.to_string(),
        task_id: task,
        exploration_id: expl,
        intent_id: intent,
        session_id: sess.to_string(),
        args_len: args,
        found,
    };
    let rows = [
        mk(&skill_a, "worker", 991, 5, 7, "", 12, true),
        mk(&skill_a, "worker", 991, 5, 0, "", 0, true),
        mk(&skill_a, "planner", 992, 6, 0, "", 0, true),
        mk(&skill_a, "chatbot", 0, 0, 0, "conv-1", 0, true),
        mk(&missing, "worker", 991, 0, 0, "", 0, false),
        mk(&missing, "worker", 991, 0, 0, "", 0, false),
    ];
    for r in &rows {
        d.insert_skill_usage(r).await.expect("insert skill_usage");
    }

    // SkillStats: resolved calls only, per-skill totals + distinct agents.
    let stats = d.skill_stats().await.expect("skill_stats");
    assert!(
        !stats.iter().any(|s| s.skill == missing),
        "misses must not appear in skill_stats: {stats:?}"
    );
    let got = stats
        .iter()
        .find(|s| s.skill == skill_a)
        .expect("skill_a present in skill_stats");
    assert_eq!(got.calls, 4, "calls: {got:?}");
    // 991 + 992; the chat row has NULL task_id and COUNT(DISTINCT) skips NULLs.
    assert_eq!(got.tasks, 2, "tasks: {got:?}");
    assert_eq!(
        got.agents.len(),
        3,
        "agents: want 3 distinct, got {:?}",
        got.agents
    );
    assert!(got.last_used.is_some(), "last_used must be set");

    // MissingSkillStats: the "wished it existed" gap list.
    let miss = d
        .missing_skill_stats(200)
        .await
        .expect("missing_skill_stats");
    let miss_calls = miss
        .iter()
        .find(|m| m.skill == missing)
        .map(|m| m.calls)
        .unwrap_or(0);
    assert_eq!(miss_calls, 2, "missing calls: {miss_calls}");
    // A positive cap is honoured (the Go clamp path).
    let capped = d.missing_skill_stats(1).await.expect("missing cap");
    assert!(
        capped.len() <= 1,
        "limit=1 must cap rows, got {}",
        capped.len()
    );

    // RecentSkillCalls: 4 rows, newest first.
    let calls: Vec<SkillCall> = d
        .recent_skill_calls(&skill_a, 10)
        .await
        .expect("recent_skill_calls");
    assert_eq!(calls.len(), 4, "recent calls: want 4, got {}", calls.len());
    for i in 1..calls.len() {
        assert!(
            calls[i].ts <= calls[i - 1].ts,
            "recent calls not newest-first at {i}"
        );
    }

    // SkillCallsByTask: resolved calls for task 991.
    let by_task: Vec<SkillStat> = d
        .skill_calls_by_task(991)
        .await
        .expect("skill_calls_by_task");
    assert!(
        !by_task.iter().any(|s| s.skill == missing),
        "misses must not appear in skill_calls_by_task: {by_task:?}"
    );
    let task_calls = by_task
        .iter()
        .find(|s| s.skill == skill_a)
        .map(|s| s.calls)
        .unwrap_or(0);
    assert_eq!(task_calls, 2, "task 991 calls: {task_calls}");

    d.execute(
        "DELETE FROM skill_usage WHERE skill IN ($1,$2)",
        &[&skill_a, &missing],
    )
    .await
    .expect("cleanup");
}

/// `TestToolUsageLedger`: writes a few rows and checks the per-key invocation
/// totals.
#[tokio::test]
async fn tool_usage_ledger() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let tool_a = format!("zz_test_tool_usage_a_{stamp}");
    let tool_b = format!("zz_test_tool_usage_b_{stamp}");

    d.execute(
        "DELETE FROM tool_usage WHERE tool_key IN ($1,$2)",
        &[&tool_a, &tool_b],
    )
    .await
    .expect("pre-clean");

    let mk = |tool: &str, agent: &str, task: i64, expl: i64, intent: i64, sess: &str| ToolUsage {
        tool_key: tool.to_string(),
        agent_key: agent.to_string(),
        task_id: task,
        exploration_id: expl,
        intent_id: intent,
        session_id: sess.to_string(),
    };
    let rows = [
        mk(&tool_a, "worker", 991, 5, 7, ""),
        mk(&tool_a, "planner", 992, 6, 0, ""),
        mk(&tool_a, "chatbot", 0, 0, 0, "conv-1"),
        mk(&tool_b, "worker", 991, 0, 0, ""),
    ];
    for r in &rows {
        d.insert_tool_usage(r).await.expect("insert tool_usage");
    }

    let counts = d.tool_usage_counts().await.expect("tool_usage_counts");
    assert_eq!(
        counts.get(&tool_a).copied().unwrap_or(0),
        3,
        "{tool_a} calls"
    );
    assert_eq!(
        counts.get(&tool_b).copied().unwrap_or(0),
        1,
        "{tool_b} calls"
    );

    d.execute(
        "DELETE FROM tool_usage WHERE tool_key IN ($1,$2)",
        &[&tool_a, &tool_b],
    )
    .await
    .expect("cleanup");
}

/// Pure-logic case for `mergeArchivedSkillStats`: folds a cold summary into a live
/// one (calls/tasks sum, agents merge dedup-equal, last_used keeps the newer) and
/// honours the `missing` switch + the v1 call-count fallback. No DB needed.
#[test]
fn merge_archived_skill_stats_pure() {
    use std::time::{Duration, UNIX_EPOCH};

    let t0 = UNIX_EPOCH + Duration::from_secs(1_000);
    let t1 = UNIX_EPOCH + Duration::from_secs(2_000);

    // Resolved path: skill_stats present → summed, agents merged, newer last_used wins.
    let live = vec![SkillStat {
        skill: "scan".into(),
        calls: 3,
        tasks: 1,
        agents: vec!["worker".into()],
        last_used: Some(t0),
    }];
    let archived = vec![TaskArchiveAggregate {
        skill_stats: vec![SkillStat {
            skill: "scan".into(),
            calls: 2,
            tasks: 4,
            agents: vec!["worker".into(), "planner".into()],
            last_used: Some(t1),
        }],
        ..Default::default()
    }];
    let merged = merge_archived_skill_stats(live, &archived, false);
    assert_eq!(merged.len(), 1);
    let s = &merged[0];
    assert_eq!(s.calls, 5, "calls summed");
    assert_eq!(s.tasks, 5, "tasks summed");
    assert_eq!(
        s.agents,
        vec!["worker".to_string(), "planner".into()],
        "agents merged"
    );
    assert_eq!(s.last_used, Some(t1), "newer last_used wins");

    // An older cold last_used must NOT overwrite a newer live one.
    let live2 = vec![SkillStat {
        skill: "scan".into(),
        calls: 1,
        tasks: 1,
        agents: vec![],
        last_used: Some(t1),
    }];
    let archived2 = vec![TaskArchiveAggregate {
        skill_stats: vec![SkillStat {
            skill: "scan".into(),
            calls: 1,
            tasks: 0,
            agents: vec![],
            last_used: Some(t0),
        }],
        ..Default::default()
    }];
    let merged2 = merge_archived_skill_stats(live2, &archived2, false);
    assert_eq!(
        merged2[0].last_used,
        Some(t1),
        "older cold last_used ignored"
    );

    // v1 fallback: empty skill_stats → built from the skills call-count map (tasks=1).
    let archived_v1 = vec![TaskArchiveAggregate {
        skills: [("old".to_string(), 7i64)].into_iter().collect(),
        ..Default::default()
    }];
    let merged_v1 = merge_archived_skill_stats(Vec::new(), &archived_v1, false);
    let old = merged_v1
        .iter()
        .find(|s| s.skill == "old")
        .expect("v1 skill");
    assert_eq!(old.calls, 7);
    assert_eq!(old.tasks, 1, "v1 fallback tasks = 1");
    assert!(old.last_used.is_none(), "v1 has no timestamp");

    // missing=true reads missing_skill_stats, not skill_stats.
    let archived_miss = vec![TaskArchiveAggregate {
        missing_skill_stats: vec![SkillStat {
            skill: "ghost".into(),
            calls: 4,
            tasks: 0,
            agents: vec!["worker".into()],
            last_used: Some(t0),
        }],
        ..Default::default()
    }];
    let merged_miss = merge_archived_skill_stats(Vec::new(), &archived_miss, true);
    assert_eq!(merged_miss.len(), 1);
    assert_eq!(merged_miss[0].skill, "ghost");
    assert_eq!(merged_miss[0].calls, 4);

    // Sort order: calls desc, then skill asc.
    let live3 = vec![
        SkillStat {
            skill: "b".into(),
            calls: 2,
            ..Default::default()
        },
        SkillStat {
            skill: "a".into(),
            calls: 2,
            ..Default::default()
        },
        SkillStat {
            skill: "c".into(),
            calls: 5,
            ..Default::default()
        },
    ];
    let sorted = merge_archived_skill_stats(live3, &[], false);
    let order: Vec<&str> = sorted.iter().map(|s| s.skill.as_str()).collect();
    assert_eq!(order, vec!["c", "a", "b"], "calls desc then skill asc");
}
