//! Differential tests for the exploration task-list metrics + daily token rollup —
//! fresh spec for the `*DB` aggregate methods in `db/exploration.go`
//! (TokenDailyAll / TaskListMetrics / TaskListMetricsAll).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration
//! (the task row is removed first because tasks.exploration_id is RESTRICT).

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::Activity;
use artex::db::gotime::civil_date_from_days;
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

/// The UTC calendar day `days` before today, "YYYY-MM-DD" — the bucket key a
/// result row stamped now() lands in.
fn day_string(days_back: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    civil_date_from_days(now.div_euclid(86_400) - days_back)
}

#[tokio::test]
async fn exploration_task_metrics_token_daily() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let today = day_string(0);

    // ---- Baseline: token_daily_all before we add anything, so the global rollup
    // can be checked by delta (other tasks may exist in the shared DB).
    let base_daily = d.token_daily_all(30).await.expect("baseline daily");
    let base_today = base_daily
        .iter()
        .find(|b| b.day == today)
        .map(|b| (b.input, b.output, b.cache_read))
        .unwrap_or((0, 0, 0));

    let stamp = uniq();
    let exp_id = d
        .create_exploration("desc", &format!("t-metrics-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // A task row binds the exploration to the tasks table (RESTRICT FK). Metrics
    // are keyed by task; findings attach via task_id.
    let task_id = d
        .query_one(
            "INSERT INTO tasks(description, goal, exploration_id) \
             VALUES ('the task', 'the goal', $1) RETURNING id",
            &[&exp_id],
        )
        .await
        .expect("create task")
        .get::<_, i64>(0);

    // ---- Goals: 2 total, 1 met.
    let g1 = store
        .add_goal(&json!({"summary": "g1"}), "planner")
        .await
        .expect("g1");
    let _g2 = store
        .add_goal(&json!({"summary": "g2"}), "planner")
        .await
        .expect("g2");
    store.set_node_state(g1, "met").await.expect("g1 met");

    // ---- Intents: 2 running (kind='intent' state='running').
    let i1 = store
        .add_intent(&json!({"summary": "i1"}), 1, &[], "planner")
        .await
        .expect("i1");
    let i2 = store
        .add_intent(&json!({"summary": "i2"}), 1, &[], "planner")
        .await
        .expect("i2");
    store
        .set_intent_state(i1, "running")
        .await
        .expect("i1 running");
    store
        .set_intent_state(i2, "running")
        .await
        .expect("i2 running");

    // ---- Activity: two kind='result' rows carry tokens; thinking/usage do not.
    store
        .append_activity(Activity::new(
            Some(i1),
            "w1",
            "result",
            "",
            "",
            false,
            "r1",
            "",
            json!(null),
            Some(100),
            Some(50),
            Some(10),
            Some(5),
            None,
        ))
        .await
        .expect("result 1");
    store
        .append_activity(Activity::new(
            Some(i1),
            "w1",
            "result",
            "",
            "",
            false,
            "r2",
            "",
            json!(null),
            Some(200),
            Some(80),
            Some(20),
            Some(10),
            None,
        ))
        .await
        .expect("result 2");
    // Big token values on non-result kinds must be ignored by both aggregations.
    store
        .append_activity(Activity::new(
            Some(i1),
            "w1",
            "thinking",
            "",
            "",
            false,
            "think",
            "",
            json!(null),
            Some(9999),
            Some(9999),
            Some(9999),
            Some(9999),
            None,
        ))
        .await
        .expect("thinking");
    store
        .append_activity(Activity::new(
            Some(i1),
            "w1",
            "usage",
            "",
            "",
            false,
            "usage",
            "",
            json!(null),
            Some(9999),
            Some(9999),
            Some(9999),
            Some(9999),
            None,
        ))
        .await
        .expect("usage");

    // ---- Findings: 1 critical + 1 high (task-scoped), 1 medium on another task
    // (must not count). We add a low-severity one scoped to THIS task too.
    store
        .add_standalone_finding(task_id, 0, "c", "crit", "critical", "s", "e", "w", &[])
        .await
        .expect("finding critical");
    store
        .add_standalone_finding(task_id, 0, "h", "high", "high", "s", "e", "w", &[])
        .await
        .expect("finding high");
    store
        .add_standalone_finding(task_id, 0, "l", "low", "low", "s", "e", "w", &[])
        .await
        .expect("finding low");
    // A finding on a DIFFERENT task must not leak into this task's metrics.
    let other_exp = d
        .create_exploration("desc", &format!("t-metrics-other-{stamp}"))
        .await
        .expect("create other");
    let other_task = d
        .query_one(
            "INSERT INTO tasks(description, goal, exploration_id) \
             VALUES ('other', 'other goal', $1) RETURNING id",
            &[&other_exp],
        )
        .await
        .expect("create other task")
        .get::<_, i64>(0);
    d.add_finding(
        other_task,
        0,
        "c",
        "other crit",
        "critical",
        "s",
        "e",
        "w",
        &[],
    )
    .await
    .expect("other finding");

    // ---- TaskListMetrics (single): tokens from kind='result' only.
    let m = d.task_list_metrics(exp_id).await.expect("metrics");
    assert_eq!(m.tokens.input, 300, "sum of result input tokens");
    assert_eq!(m.tokens.output, 130);
    assert_eq!(m.tokens.cache_read, 30);
    assert_eq!(m.tokens.cache_write, 15);
    assert_eq!(m.tokens.worker, "", "rollup has no worker label");
    assert_eq!(m.goals.total, 2);
    assert_eq!(m.goals.met, 1);
    assert_eq!(m.running_intents, 2, "two running intents");
    assert!(m.last_activity > 0, "last activity epoch is set");
    assert_eq!(m.findings.critical, 1);
    assert_eq!(m.findings.high, 1);
    assert_eq!(m.findings.medium, 0);
    assert_eq!(m.findings.low, 1);

    // ---- TaskListMetricsAll: contains this exploration with the same metrics.
    let all = d.task_list_metrics_all().await.expect("metrics all");
    let ma = all.get(&exp_id).expect("exp in all-metrics map");
    assert_eq!(ma.tokens.input, 300);
    assert_eq!(ma.running_intents, 2);
    assert_eq!(ma.findings.high, 1);
    // The other task's critical finding is counted there, not here.
    let mo = all.get(&other_exp).expect("other exp in map");
    assert_eq!(mo.findings.critical, 1, "other task has its own critical");
    assert_eq!(mo.findings.low, 0, "this task's low does not leak to other");

    // ---- TaskListMetrics: missing exploration -> zero metrics.
    let missing = d
        .task_list_metrics(99_999_999)
        .await
        .expect("metrics missing");
    assert_eq!(missing, Default::default(), "no task -> zero metrics");

    // ---- Soft-deleted task drops out of both queries.
    d.execute(
        "UPDATE tasks SET deleted_at = now() WHERE id=$1",
        &[&task_id],
    )
    .await
    .expect("soft delete task");
    let after_del = d
        .task_list_metrics(exp_id)
        .await
        .expect("metrics after delete");
    assert_eq!(after_del, Default::default(), "soft-deleted task -> zero");
    let all_after = d.task_list_metrics_all().await.expect("all after delete");
    assert!(
        !all_after.contains_key(&exp_id),
        "soft-deleted task excluded from all"
    );

    // ---- TokenDailyAll: today's bucket grew by exactly our result tokens.
    let daily = d.token_daily_all(30).await.expect("daily after");
    let now_today = daily
        .iter()
        .find(|b| b.day == today)
        .map(|b| (b.input, b.output, b.cache_read))
        .unwrap_or((0, 0, 0));
    assert_eq!(now_today.0 - base_today.0, 300, "today input delta");
    assert_eq!(now_today.1 - base_today.1, 130, "today output delta");
    assert_eq!(now_today.2 - base_today.2, 30, "today cache_read delta");
    // days<=0 defaults to 30 (same window) — just confirm it returns rows.
    assert!(
        !d.token_daily_all(0)
            .await
            .expect("daily default")
            .is_empty()
            || base_daily.is_empty()
    );

    // Cleanup: task first (RESTRICT), then explorations cascade.
    d.execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await
        .expect("clean task");
    d.execute("DELETE FROM tasks WHERE id=$1", &[&other_task])
        .await
        .expect("clean other task");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&other_exp])
        .await
        .expect("clean other exp");
}
