//! Differential test for the per-task outcome card — fresh spec for
//! `db/task_outcome.go` (the Go package ships no `_test.go` for it).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Seeds an exploration + task and a spread of findings,
//! retests, activity token rows, tool/skill usage, then checks every counter of
//! the aggregate matches the seeded rows. Cleans up via cascade on the exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::DB;
use artex::llm::Context;

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

/// Seed an exploration + task; return (exploration_id, task_id).
async fn seed_task(d: &DB, stamp: u128) -> (i64, i64) {
    let goal = format!("t-outcome-goal-{stamp}");
    let exp: i64 = d
        .query_one(
            "INSERT INTO explorations(goal) VALUES ($1) RETURNING id",
            &[&goal],
        )
        .await
        .expect("insert exploration")
        .get(0);
    let task: i64 = d
        .query_one(
            "INSERT INTO tasks(description, goal, exploration_id) VALUES ($1, $1, $2) RETURNING id",
            &[&goal, &exp],
        )
        .await
        .expect("insert task")
        .get(0);
    (exp, task)
}

#[tokio::test]
async fn task_outcome_aggregates_seeded_rows() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let ctx = Context::background();
    let stamp = uniq();
    let (exp, task) = seed_task(&d, stamp).await;

    // Findings: 2 confirmed (critical + high), 1 open (pending), 1 false_positive,
    // 1 duplicate, 1 in_progress(low, open). Total 6.
    // confirmed severity split (confirmed/resolved/fixed only): critical 1, high 1.
    let mk_finding = |status: &str, severity: &str| {
        let d = d.clone();
        let status = status.to_string();
        let severity = severity.to_string();
        async move {
            d.query_one(
                "INSERT INTO findings(task_id, name, vulnclass, severity, status) VALUES ($1,$2,'xss',$3,$4) RETURNING id",
                &[&task, &format!("f-{status}-{severity}-{stamp}"), &severity, &status],
            )
            .await
            .expect("seed finding")
            .get::<_, i64>(0)
        }
    };
    let _f_crit = mk_finding("confirmed", "critical").await;
    let f_high = mk_finding("confirmed", "high").await;
    let f_open = mk_finding("pending", "critical").await; // open, not confirmed
    let f_fp = mk_finding("false_positive", "high").await;
    let _f_dup = mk_finding("duplicate", "low").await;
    let _f_prog = mk_finding("in_progress", "low").await;

    // One retest per finding (idx_finding_retests_active is UNIQUE on finding_id):
    // reproduced / fixed / inconclusive spread across three distinct findings.
    let snap = serde_json::json!({});
    for (fid, verdict) in [
        (f_high, "reproduced"),
        (f_open, "fixed"),
        (f_fp, "inconclusive"),
    ] {
        d.execute(
            "INSERT INTO finding_retests(finding_id, snapshot, verdict) VALUES ($1,$2,$3)",
            &[&fid, &snap, &verdict],
        )
        .await
        .expect("seed retest");
    }

    // Activity result row on this exploration's tokens (1 result row).
    d.execute(
        "INSERT INTO activity(exploration_id, kind, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) \
         VALUES ($1,'result',100,40,10,5)",
        &[&exp],
    )
    .await
    .expect("seed activity");

    // Tool usage: 3 calls for this task.
    for i in 0..3 {
        d.execute(
            "INSERT INTO tool_usage(tool_key, task_id) VALUES ($1,$2)",
            &[&format!("http_request-{i}"), &task],
        )
        .await
        .expect("seed tool_usage");
    }

    // Skill usage: 2 rows, 1 not found (a miss), 1 found.
    d.execute(
        "INSERT INTO skill_usage(skill, task_id, found) VALUES ('recon',$1,false)",
        &[&task],
    )
    .await
    .expect("seed skill miss");
    d.execute(
        "INSERT INTO skill_usage(skill, task_id, found) VALUES ('recon',$1,true)",
        &[&task],
    )
    .await
    .expect("seed skill hit");

    let o = d.task_outcome(&ctx, task).await.expect("task_outcome");
    assert_eq!(o.task_id, task);
    assert_eq!(o.category_id, 0, "null category_id coalesces to 0");
    assert_eq!(o.company_id, 0, "null company_id coalesces to 0");

    // Findings split.
    assert_eq!(o.findings.total, 6, "total findings");
    assert_eq!(o.findings.confirmed, 2, "confirmed+resolved+fixed");
    assert_eq!(o.findings.open, 2, "pending+in_progress");
    assert_eq!(o.findings.false_positive, 1, "false_positive");
    assert_eq!(o.findings.duplicate, 1, "duplicate");
    // Confirmed-only severity split: critical 1 (pending critical excluded), high 1.
    assert_eq!(o.findings.critical, 1, "confirmed critical only");
    assert_eq!(o.findings.high, 1, "confirmed high only");
    assert_eq!(o.findings.medium, 0);
    assert_eq!(
        o.findings.low, 0,
        "low confirmed only (in_progress low excluded)"
    );

    // Retests split.
    assert_eq!(o.retests.reproduced, 1);
    assert_eq!(o.retests.fixed, 1);
    assert_eq!(o.retests.inconclusive, 1);

    // Tokens (summed over result rows on the exploration).
    assert_eq!(o.tokens.input_tokens, 100);
    assert_eq!(o.tokens.output_tokens, 40);
    assert_eq!(o.tokens.cache_read_tokens, 10);
    assert_eq!(o.tokens.cache_write_tokens, 5);

    // Tool calls + skill misses.
    assert_eq!(o.tool_calls, 3, "tool_usage rows");
    assert_eq!(o.skill_misses, 1, "skill_usage rows with NOT found");

    // WallSeconds is >= 0 (completed_at null -> now() - first_run_at/created_at).
    assert!(
        o.wall_seconds >= 0,
        "wall seconds non-negative: {}",
        o.wall_seconds
    );

    // A missing task id errors (sql.ErrNoRows equivalent).
    let missing = d.task_outcome(&ctx, i64::MAX).await;
    assert!(missing.is_err(), "missing task must error");

    // A soft-deleted task is treated as absent.
    d.execute("UPDATE tasks SET deleted_at = now() WHERE id=$1", &[&task])
        .await
        .expect("soft delete");
    let deleted = d.task_outcome(&ctx, task).await;
    assert!(deleted.is_err(), "soft-deleted task must error");

    // Explicit cleanup (tasks.exploration_id is ON DELETE RESTRICT, findings.task_id
    // is ON DELETE SET NULL, tool/skill_usage outlive tasks): delete children first.
    d.execute("DELETE FROM findings WHERE task_id=$1", &[&task])
        .await
        .expect("clean findings");
    d.execute("DELETE FROM tool_usage WHERE task_id=$1", &[&task])
        .await
        .expect("clean tool_usage");
    d.execute("DELETE FROM skill_usage WHERE task_id=$1", &[&task])
        .await
        .expect("clean skill_usage");
    d.execute("DELETE FROM activity WHERE exploration_id=$1", &[&exp])
        .await
        .expect("clean activity");
    d.execute("DELETE FROM tasks WHERE id=$1", &[&task])
        .await
        .expect("clean task");
    d.execute(
        "DELETE FROM explorations WHERE goal=$1",
        &[&format!("t-outcome-goal-{stamp}")],
    )
    .await
    .expect("clean exploration");
}

/// A cancelled context short-circuits with the context error (QueryContext semantics).
#[tokio::test]
async fn task_outcome_cancelled_context() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let cancelled = Context::canceled();
    let err = d
        .task_outcome(&cancelled, 1)
        .await
        .expect_err("cancelled context must error");
    assert!(
        err.to_string().contains("context canceled"),
        "cancelled error = {err:?}"
    );
}
