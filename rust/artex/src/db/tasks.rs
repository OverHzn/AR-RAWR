//! Task registry — port of `db/tasks.go`. This first slice is the foundation
//! (sentinels, limits, `IsTerminal`, `NormalizeTaskCompanyIDs`, the heartbeat
//! floor) plus the self-contained lifecycle mutators that act on the `tasks`
//! table by id: `SetParentRef`, `SetPaused`, `Enqueue`/`Dequeue`/`SetQueued`,
//! `SetStatus`, `SetTerminalStatusGuarded`, `ClearRunEndReason`, `StampFirstRun`.
//!
//! The `Task` row type + `CreateTask`/`CreateTaskWithOptions`/`GetTask`/
//! `ListTasks`/`UpdateTask` (which need the LLM-chain hydrate from
//! `db/task_context.go`) and `DeleteTaskCascade*` (which need target-pin + traffic
//! helpers) land in later slices on top of this one.

use std::time::SystemTime;

use crate::db::{Error, Result, DB};

/// `MinPlanHeartbeatSeconds`: floor of the planner heartbeat interval (default
/// 10min). Anything lower is raised to it to avoid hammering the planner.
pub const MIN_PLAN_HEARTBEAT_SECONDS: i32 = 600;
/// `MaxTaskSourceCount`: bound on inherited source tasks per task.
pub const MAX_TASK_SOURCE_COUNT: usize = 8;
/// `MaxTaskCompanyCount`: bound on company asset scopes attached to a task.
pub const MAX_TASK_COMPANY_COUNT: usize = 32;

/// `ErrTaskCompanyIDsInvalid`: the invalid-company-ids sentinel.
pub const ERR_TASK_COMPANY_IDS_INVALID: &str = "invalid task company ids";
/// `ErrTaskCompanyNotFound`: the missing-company sentinel.
pub const ERR_TASK_COMPANY_NOT_FOUND: &str = "task company not found";

/// `IsTerminal`: whether a task status is a terminal (finished) state.
pub fn is_terminal(status: &str) -> bool {
    status == "done" || status == "failed" || status == "timeout"
}

/// `normalizeHeartbeat`: raise a sub-floor heartbeat interval to the default.
pub fn normalize_heartbeat(sec: i32) -> i32 {
    if sec < MIN_PLAN_HEARTBEAT_SECONDS {
        MIN_PLAN_HEARTBEAT_SECONDS
    } else {
        sec
    }
}

/// `NormalizeTaskCompanyIDs`: validate ids and remove duplicates while retaining
/// first-seen order. Empty input returns an empty vec.
pub fn normalize_task_company_ids(ids: &[i64]) -> Result<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut normalized: Vec<i64> = Vec::new();
    for &id in ids {
        if id <= 0 {
            return Err(Error::sentinel(
                ERR_TASK_COMPANY_IDS_INVALID,
                format!("{ERR_TASK_COMPANY_IDS_INVALID}: company id must be positive"),
            ));
        }
        if !seen.insert(id) {
            continue;
        }
        normalized.push(id);
        if normalized.len() > MAX_TASK_COMPANY_COUNT {
            return Err(Error::sentinel(
                ERR_TASK_COMPANY_IDS_INVALID,
                format!(
                    "{ERR_TASK_COMPANY_IDS_INVALID}: got more than {MAX_TASK_COMPANY_COUNT} unique companies"
                ),
            ));
        }
    }
    Ok(normalized)
}

impl DB {
    /// `SetParentRef`: record a task's parent task id (orchestration spawn);
    /// empty clears it.
    pub async fn set_parent_ref(&self, id: i64, parent_ref: &str) -> Result<()> {
        self.execute(
            "UPDATE tasks SET parent_ref=NULLIF($2,'') WHERE id=$1",
            &[&id, &parent_ref],
        )
        .await?;
        Ok(())
    }

    /// `SetPaused`: persist a task's paused flag.
    pub async fn set_paused(&self, id: i64, paused: bool) -> Result<()> {
        self.execute("UPDATE tasks SET paused=$1 WHERE id=$2", &[&paused, &id])
            .await?;
        Ok(())
    }

    /// `Enqueue`: place a task at the tail of the persistent admission queue.
    /// Repeating while queued keeps its original FIFO position. `mode` must be
    /// `bootstrap` or `resume`.
    pub async fn enqueue(&self, id: i64, mode: &str) -> Result<()> {
        if mode != "bootstrap" && mode != "resume" {
            return Err(Error::msg(format!("invalid queue mode {mode:?}")));
        }
        self.execute(
            "UPDATE tasks \
         SET queued=true, \
             queued_at=CASE WHEN queued THEN COALESCE(queued_at, now()) ELSE now() END, \
             queue_mode=CASE \
                 WHEN queue_mode='bootstrap' OR $2='bootstrap' THEN 'bootstrap' \
                 ELSE 'resume' \
             END \
         WHERE id=$1",
            &[&id, &mode],
        )
        .await?;
        Ok(())
    }

    /// `Dequeue`: remove the concurrency hold. `clear_mode` is false when a user
    /// pauses a queued task, preserving whether its next admission must bootstrap
    /// or resume.
    pub async fn dequeue(&self, id: i64, clear_mode: bool) -> Result<()> {
        self.execute(
            "UPDATE tasks \
         SET queued=false, queued_at=NULL, \
             queue_mode=CASE WHEN $2 THEN '' ELSE queue_mode END \
         WHERE id=$1",
            &[&id, &clear_mode],
        )
        .await?;
        Ok(())
    }

    /// `SetQueued`: compatibility helper (older callers / tests). Queued uses
    /// bootstrap mode; not-queued clears the mode.
    pub async fn set_queued(&self, id: i64, queued: bool) -> Result<()> {
        if queued {
            self.enqueue(id, "bootstrap").await
        } else {
            self.dequeue(id, true).await
        }
    }

    /// `SetStatus`: update a task's lifecycle status. Entering a terminal state
    /// stamps `completed_at` once (COALESCE keeps the first stamp stable); moving
    /// back to a non-terminal state clears it (and clears any stale end_reason).
    pub async fn set_status(&self, id: i64, status: &str) -> Result<()> {
        self.execute(
            "UPDATE tasks \
         SET status = $1, \
             completed_at = CASE WHEN $1 IN ('done','failed','timeout') \
                                 THEN COALESCE(completed_at, now()) ELSE NULL END, \
             end_reason = CASE WHEN $1 IN ('done','failed','timeout') \
                               THEN end_reason ELSE '' END \
         WHERE id = $2",
            &[&status, &id],
        )
        .await?;
        Ok(())
    }

    /// `SetTerminalStatusGuarded`: set a terminal status only when the task is NOT
    /// already terminal, so a completed↔timeout race resolves to the first writer
    /// (returns `won=true`). A losing writer stores nothing and returns
    /// `won=false` (no error).
    pub async fn set_terminal_status_guarded(
        &self,
        id: i64,
        status: &str,
        end_reason: &str,
    ) -> Result<bool> {
        let n = self
            .execute(
                "UPDATE tasks \
             SET status = $1, completed_at = COALESCE(completed_at, now()), end_reason = $3 \
             WHERE id = $2 AND status NOT IN ('done','failed','timeout')",
                &[&status, &id, &end_reason],
            )
            .await?;
        Ok(n > 0)
    }

    /// `ClearRunEndReason`: drop the end_reason of a task that is running again (a
    /// revived run). A terminal task keeps its reason.
    pub async fn clear_run_end_reason(&self, id: i64) -> Result<()> {
        self.execute(
            "UPDATE tasks SET end_reason='' \
         WHERE id=$1 AND end_reason<>'' AND status NOT IN ('done','failed','timeout')",
            &[&id],
        )
        .await?;
        Ok(())
    }

    /// `StampFirstRun`: record a task's first-real-run moment and compute its
    /// absolute deadline (= now + timeoutSeconds). Idempotent: only stamps when
    /// `first_run_at` is still NULL. `timeout_seconds <= 0` leaves the deadline
    /// NULL (no limit). Returns the resulting deadline (None = unlimited/unchanged).
    pub async fn stamp_first_run(
        &self,
        id: i64,
        timeout_seconds: i32,
    ) -> Result<Option<SystemTime>> {
        let row = self
            .query_one(
                "UPDATE tasks \
             SET first_run_at = COALESCE(first_run_at, now()), \
                 deadline_at = CASE \
                     WHEN first_run_at IS NOT NULL THEN deadline_at \
                     WHEN $2::int > 0 THEN now() + ($2::int * INTERVAL '1 second') \
                     ELSE NULL END \
             WHERE id = $1 \
             RETURNING deadline_at",
                &[&id, &timeout_seconds],
            )
            .await?;
        Ok(row.get::<_, Option<SystemTime>>(0))
    }
}
