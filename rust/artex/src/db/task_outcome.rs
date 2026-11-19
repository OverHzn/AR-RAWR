//! Per-task outcome card — port of `db/task_outcome.go`.
//!
//! The outcome card is the baseline layer for RSI comparisons: it consolidates what
//! a task produced and what it cost so a later harness/lesson change can be judged
//! against a number. One big read-only aggregate `SELECT` (LATERAL subqueries over
//! findings, finding_retests, activity, tool_usage, skill_usage); no writes.
//!
//! `sql.ErrNoRows` (task missing or soft-deleted) maps to a plain error with the
//! same message; no Rust caller checks it by identity yet.

use crate::db::error::{Error, Result};
use crate::db::DB;
use crate::llm::Context;

/// `TaskOutcomeFindings` — finding disposition and severity split.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskOutcomeFindings {
    pub total: i64,
    /// confirmed, resolved, or fixed
    pub confirmed: i64,
    /// pending or in_progress
    pub open: i64,
    pub false_positive: i64,
    pub duplicate: i64,
    /// confirmed only
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
}

/// `TaskOutcomeRetests` — retest verdict split for this task's findings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskOutcomeRetests {
    pub reproduced: i64,
    pub fixed: i64,
    pub inconclusive: i64,
}

/// `TaskOutcomeTokens` — the task's result-row token total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskOutcomeTokens {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

/// `TaskOutcome` — one task's production and cost baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    pub task_id: i64,
    pub status: String,
    pub category_id: i64,
    pub company_id: i64,
    pub findings: TaskOutcomeFindings,
    pub retests: TaskOutcomeRetests,
    pub tokens: TaskOutcomeTokens,
    pub tool_calls: i64,
    pub skill_misses: i64,
    pub wall_seconds: i64,
}

/// The outcome-card query, parameterised only by `$1 taskID`.
const TASK_OUTCOME_SQL: &str = r#"
SELECT t.id, t.status,
       COALESCE(t.category_id,0), COALESCE(t.company_id,0),
       COALESCE(f.total,0), COALESCE(f.confirmed,0), COALESCE(f.open,0),
       COALESCE(f.false_positive,0), COALESCE(f.duplicate,0),
       COALESCE(f.critical,0), COALESCE(f.high,0),
       COALESCE(f.medium,0), COALESCE(f.low,0),
       COALESCE(r.reproduced,0), COALESCE(r.fixed,0), COALESCE(r.inconclusive,0),
       COALESCE(a.input_tokens,0), COALESCE(a.output_tokens,0),
       COALESCE(a.cache_read_tokens,0), COALESCE(a.cache_write_tokens,0),
       COALESCE(tu.calls,0), COALESCE(su.misses,0),
       GREATEST(0, EXTRACT(EPOCH FROM
           (COALESCE(t.completed_at, now()) - COALESCE(t.first_run_at, t.created_at)))::bigint)
FROM tasks t
LEFT JOIN LATERAL (
    SELECT COUNT(*) AS total,
           COUNT(*) FILTER (WHERE status IN ('confirmed','resolved','fixed')) AS confirmed,
           COUNT(*) FILTER (WHERE status IN ('pending','in_progress'))       AS open,
           COUNT(*) FILTER (WHERE status='false_positive')                   AS false_positive,
           COUNT(*) FILTER (WHERE status='duplicate')                        AS duplicate,
           COUNT(*) FILTER (WHERE status IN ('confirmed','resolved','fixed') AND severity='critical') AS critical,
           COUNT(*) FILTER (WHERE status IN ('confirmed','resolved','fixed') AND severity='high')     AS high,
           COUNT(*) FILTER (WHERE status IN ('confirmed','resolved','fixed') AND severity='medium')   AS medium,
           COUNT(*) FILTER (WHERE status IN ('confirmed','resolved','fixed') AND severity='low')      AS low
    FROM findings
    WHERE task_id=t.id
) f ON true
LEFT JOIN LATERAL (
    SELECT COUNT(*) FILTER (WHERE fr.verdict='reproduced')   AS reproduced,
           COUNT(*) FILTER (WHERE fr.verdict='fixed')        AS fixed,
           COUNT(*) FILTER (WHERE fr.verdict='inconclusive') AS inconclusive
    FROM finding_retests fr
    JOIN findings f2 ON f2.id=fr.finding_id
    WHERE f2.task_id=t.id
) r ON true
LEFT JOIN LATERAL (
    SELECT SUM(input_tokens) AS input_tokens,
           SUM(output_tokens) AS output_tokens,
           SUM(cache_read_tokens) AS cache_read_tokens,
           SUM(cache_write_tokens) AS cache_write_tokens
    FROM activity
    WHERE exploration_id=t.exploration_id AND kind='result'
) a ON true
LEFT JOIN LATERAL (
    SELECT COUNT(*) AS calls FROM tool_usage WHERE task_id=t.id
) tu ON true
LEFT JOIN LATERAL (
    SELECT COUNT(*) AS misses FROM skill_usage WHERE task_id=t.id AND NOT found
) su ON true
WHERE t.id=$1 AND t.deleted_at IS NULL"#;

impl DB {
    /// `TaskOutcome`: the outcome card for one live task. A missing or soft-deleted
    /// task yields an error (Go's `sql.ErrNoRows`, same message).
    pub async fn task_outcome(&self, ctx: &Context, task_id: i64) -> Result<TaskOutcome> {
        if let Some(e) = ctx.err() {
            return Err(e.into());
        }
        // Bind the params slice to a local so it outlives the pinned query future.
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&task_id];
        let fut = self.query_opt(TASK_OUTCOME_SQL, &params);
        tokio::pin!(fut);
        let row = tokio::select! {
            biased;
            () = ctx.cancelled() => {
                return Err(ctx.err().unwrap_or(crate::llm::Error::Canceled).into());
            }
            row = fut => row?,
        };
        let row = row.ok_or_else(|| Error::msg("no rows in result set"))?;
        Ok(TaskOutcome {
            task_id: row.get(0),
            status: row.get(1),
            category_id: row.get(2),
            company_id: row.get(3),
            findings: TaskOutcomeFindings {
                total: row.get(4),
                confirmed: row.get(5),
                open: row.get(6),
                false_positive: row.get(7),
                duplicate: row.get(8),
                critical: row.get(9),
                high: row.get(10),
                medium: row.get(11),
                low: row.get(12),
            },
            retests: TaskOutcomeRetests {
                reproduced: row.get(13),
                fixed: row.get(14),
                inconclusive: row.get(15),
            },
            tokens: TaskOutcomeTokens {
                input_tokens: row.get(16),
                output_tokens: row.get(17),
                cache_read_tokens: row.get(18),
                cache_write_tokens: row.get(19),
            },
            tool_calls: row.get(20),
            skill_misses: row.get(21),
            wall_seconds: row.get(22),
        })
    }
}
