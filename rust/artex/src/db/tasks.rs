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

/// `Task` is a row in the task registry (1:1 with an exploration). The LLM-chain
/// fields (`llm_profile_id` / `active_llm_profile_id` / `llm_profile_ids` / ...),
/// `source_task_ids`, and `company_ids` are hydrated from the side tables by
/// `DB::hydrate_task_context` / `hydrate_tasks_context`; `created_at` / `pinned`
/// come straight from the `tasks` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    /// optional task name; empty=unnamed
    pub name: String,
    pub category_id: Option<i64>,
    pub category_name: String,
    /// derived from `pinned_at != NULL` at scan time.
    pub pinned: bool,
    pub pinned_at: Option<SystemTime>,
    pub description: String,
    pub goal: String,
    pub exploration_id: i64,
    pub status: String,
    pub paused: bool,
    pub queued: bool,
    pub queued_at: Option<SystemTime>,
    pub queue_mode: String,
    /// compatibility alias for `active_llm_profile_id` (kept for older clients).
    pub llm_profile_id: Option<i64>,
    /// the full ordered chain (all entries, ready or exhausted).
    pub llm_profile_ids: Vec<i64>,
    /// the ready subset of `llm_profile_ids`.
    pub ready_llm_profile_ids: Vec<i64>,
    pub active_llm_profile_id: Option<i64>,
    pub llm_chain_revision: i64,
    /// "default" (no chain), "ready" (active head usable), "chain_exhausted".
    pub llm_failover_state: String,
    pub llm_failover_reason: String,
    /// per-intent chain-head rotation switch (#85).
    pub llm_rotate: bool,
    pub source_task_ids: Vec<i64>,
    pub company_ids: Vec<i64>,
    /// parent task id (recorded by orchestration spawn; empty=top level).
    pub parent_ref: String,
    pub created_at: SystemTime,
    /// when it entered a terminal state (done/failed/timeout); None if non-terminal.
    pub completed_at: Option<SystemTime>,
    /// task-level wall-clock budget in seconds; 0=no time limit.
    pub timeout_seconds: i32,
    /// when it first actually started running (not created_at); None=not yet run.
    pub first_run_at: Option<SystemTime>,
    /// = first_run_at + timeout_seconds; None=unlimited or not yet run.
    pub deadline_at: Option<SystemTime>,
    /// planner heartbeat trigger interval (seconds).
    pub plan_heartbeat_seconds: i32,
    /// planner quiet-mode (#86): skip edge-triggered rounds while work is in flight.
    pub plan_quiet_mode: bool,
    /// per-task opt-in of the approval gate for asset targets.
    pub target_gate: bool,
    /// master switch of the "asset coverage" feature (default true).
    pub coverage_enabled: bool,
}

impl Default for Task {
    /// Zero-value `Task` (Go's `&Task{}`): epoch `created_at`, empty strings,
    /// zero numbers, empty slices. A construction convenience; the real fields are
    /// filled by a row scan + hydrate.
    fn default() -> Self {
        Task {
            id: 0,
            name: String::new(),
            category_id: None,
            category_name: String::new(),
            pinned: false,
            pinned_at: None,
            description: String::new(),
            goal: String::new(),
            exploration_id: 0,
            status: String::new(),
            paused: false,
            queued: false,
            queued_at: None,
            queue_mode: String::new(),
            llm_profile_id: None,
            llm_profile_ids: Vec::new(),
            ready_llm_profile_ids: Vec::new(),
            active_llm_profile_id: None,
            llm_chain_revision: 0,
            llm_failover_state: String::new(),
            llm_failover_reason: String::new(),
            llm_rotate: false,
            source_task_ids: Vec::new(),
            company_ids: Vec::new(),
            parent_ref: String::new(),
            created_at: SystemTime::UNIX_EPOCH,
            completed_at: None,
            timeout_seconds: 0,
            first_run_at: None,
            deadline_at: None,
            plan_heartbeat_seconds: 0,
            plan_quiet_mode: false,
            target_gate: false,
            coverage_enabled: false,
        }
    }
}

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

/// `taskCols`: the `SELECT` column list backing `scanTask` (and every full-row
/// read: `GetTask` / `ListTasks` / `UpdateTask`). COALESCE guards keep the
/// nullable columns at their Go zero values.
const TASK_COLS: &str = "id, COALESCE(name,''), category_id, \
COALESCE((SELECT category.name FROM task_categories category WHERE category.id=tasks.category_id),''), \
description, goal, exploration_id, status, paused, queued, queued_at, \
COALESCE(queue_mode,''), llm_profile_id, active_llm_profile_id, \
COALESCE(parent_ref,''), pinned_at, created_at, completed_at, \
COALESCE(timeout_seconds,0), COALESCE(plan_heartbeat_seconds,300), \
COALESCE(plan_quiet_mode,true), COALESCE(coverage_enabled,true), \
COALESCE(llm_rotate,false), first_run_at, deadline_at, target_gate";

/// `scanTask`: build a `Task` from a `TASK_COLS`-shaped row. `pinned` is derived
/// from `pinned_at != NULL` (there is no separate `pinned` column).
fn scan_task(row: &tokio_postgres::Row) -> Task {
    let pinned_at: Option<SystemTime> = row.get(15);
    Task {
        id: row.get(0),
        name: row.get(1),
        category_id: row.get(2),
        category_name: row.get(3),
        pinned: pinned_at.is_some(),
        pinned_at,
        description: row.get(4),
        goal: row.get(5),
        exploration_id: row.get(6),
        status: row.get(7),
        paused: row.get(8),
        queued: row.get(9),
        queued_at: row.get(10),
        queue_mode: row.get(11),
        llm_profile_id: row.get(12),
        active_llm_profile_id: row.get(13),
        llm_profile_ids: Vec::new(),
        ready_llm_profile_ids: Vec::new(),
        llm_chain_revision: 0,
        llm_failover_state: String::new(),
        llm_failover_reason: String::new(),
        llm_rotate: row.get(22),
        source_task_ids: Vec::new(),
        company_ids: Vec::new(),
        parent_ref: row.get(14),
        created_at: row.get(16),
        completed_at: row.get(17),
        timeout_seconds: row.get(18),
        first_run_at: row.get(23),
        deadline_at: row.get(24),
        plan_heartbeat_seconds: row.get(19),
        plan_quiet_mode: row.get(20),
        target_gate: row.get(25),
        coverage_enabled: row.get(21),
    }
}

/// `TaskPatch` updates task-list metadata without changing execution state. Each
/// `Option` is a tri-state: `None` = leave unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPatch {
    pub name: Option<String>,
    pub pinned: Option<bool>,
    /// planner quiet-mode (#86).
    pub plan_quiet_mode: Option<bool>,
    /// target-gate opt-in.
    pub target_gate: Option<bool>,
}

impl DB {
    /// `GetTask`: one alive task (None if not found / soft-deleted), hydrated with
    /// its LLM chain, source ids, and company scopes.
    pub async fn get_task(&self, id: i64) -> Result<Option<Task>> {
        let row = self
            .query_opt(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE id=$1 AND deleted_at IS NULL"),
                &[&id],
            )
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut t = scan_task(&row);
        self.hydrate_task_context(&mut t).await?;
        Ok(Some(t))
    }

    /// `ListTasks`: alive tasks, pinned first (by pinned_at desc), then newest id.
    /// Bulk-hydrated with three queries rather than 3N+1.
    pub async fn list_tasks(&self) -> Result<Vec<Task>> {
        let rows = self
            .query(
                &format!(
                    "SELECT {TASK_COLS} FROM tasks WHERE deleted_at IS NULL \
                     ORDER BY (pinned_at IS NOT NULL) DESC, pinned_at DESC NULLS LAST, id DESC"
                ),
                &[],
            )
            .await?;
        let mut out: Vec<Task> = rows.iter().map(scan_task).collect();
        self.hydrate_tasks_context(&mut out).await?;
        Ok(out)
    }

    /// `UpdateTask`: patch task-list metadata (name / pinned / quiet-mode /
    /// target-gate). Returns None when the task is missing or soft-deleted. The
    /// row is returned un-hydrated (the LLM chain is not re-read), matching Go.
    pub async fn update_task(&self, id: i64, patch: TaskPatch) -> Result<Option<Task>> {
        let name_present = patch.name.is_some();
        let quiet_present = patch.plan_quiet_mode.is_some();
        let gate_present = patch.target_gate.is_some();
        let row = self
            .query_opt(
                &format!(
                    "UPDATE tasks SET \
                     name = CASE WHEN $2::boolean THEN $3 ELSE name END, \
                     pinned_at = CASE \
                         WHEN $4::boolean IS NULL THEN pinned_at \
                         WHEN $4::boolean THEN COALESCE(pinned_at, now()) \
                         ELSE NULL \
                     END, \
                     plan_quiet_mode = CASE WHEN $5::boolean THEN $6 ELSE plan_quiet_mode END, \
                     target_gate = CASE WHEN $7::boolean THEN $8 ELSE target_gate END \
                     WHERE id=$1 AND deleted_at IS NULL \
                     RETURNING {TASK_COLS}",
                ),
                &[
                    &id,
                    &name_present,
                    &patch.name,
                    &patch.pinned,
                    &quiet_present,
                    &patch.plan_quiet_mode,
                    &gate_present,
                    &patch.target_gate,
                ],
            )
            .await?;
        Ok(row.as_ref().map(scan_task))
    }
}

/// `taskCompanyAssetSource`: the `task_asset_links.source` label for assets pulled
/// in through a company scope at task creation.
const TASK_COMPANY_ASSET_SOURCE: &str = "company";

/// `insertTaskRelations`: validate and insert the direct source-task relations.
/// Rejects non-positive / self / duplicate ids and missing (or soft-deleted) tasks.
async fn insert_task_relations(
    tx: &deadpool_postgres::Transaction<'_>,
    task_id: i64,
    source_ids: &[i64],
) -> Result<()> {
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for &source_id in source_ids {
        if source_id <= 0 || source_id == task_id || !seen.insert(source_id) {
            return Err(Error::msg(format!(
                "invalid or duplicate source task id {source_id}"
            )));
        }
        let n = tx
            .execute(
                "INSERT INTO task_relations(task_id, source_task_id) \
                 SELECT $1, id FROM tasks WHERE id=$2 AND deleted_at IS NULL",
                &[&task_id, &source_id],
            )
            .await?;
        if n != 1 {
            return Err(Error::msg(format!("source task {source_id} not found")));
        }
    }
    Ok(())
}

/// `insertTaskCompanies`: attach company asset scopes under the shared scope-mutation
/// lock (so the creation-time snapshot serialises with scope edits), snapshot the
/// currently-attributed assets into the task, and record provenance links. Errors
/// (`ErrTaskCompanyNotFound`) when any company is missing.
async fn insert_task_companies(
    tx: &deadpool_postgres::Transaction<'_>,
    task_id: i64,
    company_ids: &[i64],
) -> Result<()> {
    if company_ids.is_empty() {
        return Ok(());
    }
    // Serialise with company-scope edits so the task sees one committed attribution.
    tx.execute(
        "SELECT pg_advisory_xact_lock($1)",
        &[&crate::db::companies::COMPANY_SCOPE_MUTATION_LOCK],
    )
    .await?;

    let inserted: i64 = tx
        .query_one(
            "WITH requested(company_id, position) AS (\
                 SELECT company_id, position \
                 FROM unnest($2::bigint[]) WITH ORDINALITY AS requested(company_id, position)\
             ), inserted AS (\
                 INSERT INTO task_scope(task_id, kind, company_id, source, reason) \
                 SELECT $1, 'company', companies.id, 'manual', \
                        'associated with company at task creation' \
                 FROM requested \
                 JOIN companies ON companies.id=requested.company_id \
                 ORDER BY requested.position \
                 RETURNING company_id\
             ) \
             SELECT count(*) FROM inserted",
            &[&task_id, &company_ids],
        )
        .await?
        .get::<_, i64>(0);
    if inserted != company_ids.len() as i64 {
        return Err(Error::sentinel(
            ERR_TASK_COMPANY_NOT_FOUND,
            format!("{ERR_TASK_COMPANY_NOT_FOUND}: one or more companies do not exist"),
        ));
    }

    // `task_ids` update fires sync_task_asset_links (creating generic source rows),
    // so the provenance upsert must run afterwards to keep the company name/reason.
    tx.execute(
        "UPDATE assets \
         SET task_ids=CASE \
             WHEN $1=ANY(task_ids) THEN task_ids \
             ELSE array_append(task_ids, $1) \
         END \
         WHERE company_id=ANY($2::bigint[])",
        &[&task_id, &company_ids],
    )
    .await?;
    tx.execute(
        "INSERT INTO task_asset_links(task_id, asset_id, source, source_summary) \
         SELECT $1, asset.id, $3, \
                'associated with company at task creation: ' || company.name \
         FROM assets asset \
         JOIN companies company ON company.id=asset.company_id \
         WHERE asset.company_id=ANY($2::bigint[]) \
           AND $1=ANY(asset.task_ids) \
         ON CONFLICT (task_id, asset_id) DO UPDATE \
         SET source=EXCLUDED.source, \
             source_summary=EXCLUDED.source_summary, \
             source_node_id=NULL",
        &[&task_id, &company_ids, &TASK_COMPANY_ASSET_SOURCE],
    )
    .await?;
    Ok(())
}

/// `TaskCreateOptions`: the task data committed atomically with the task/exploration
/// row. `None` switch fields keep their defaults (quiet-mode / coverage on).
#[derive(Debug, Clone, Default)]
pub struct TaskCreateOptions {
    /// optional task name; empty=unnamed.
    pub name: String,
    pub category_id: Option<i64>,
    pub source_task_ids: Vec<i64>,
    pub company_ids: Vec<i64>,
    pub llm_profile_ids: Vec<i64>,
    pub timeout_seconds: i32,
    pub plan_heartbeat_seconds: i32,
    /// per-task quiet-mode switch; `None` = default on (#86).
    pub plan_quiet_mode: Option<bool>,
    /// "asset coverage" switch; `None` = default on (true).
    pub coverage_enabled: Option<bool>,
    /// task-level asset interception rules, written in the same tx.
    pub intercept_rules: Vec<crate::db::task_templates::TaskInterceptRuleInput>,
    /// per-intent chain-head rotation (#85); `None` = off.
    pub llm_rotate: Option<bool>,
    /// approval-gate opt-in; `None` = off.
    pub target_gate: Option<bool>,
}

impl DB {
    /// `CreateTask`: create an exploration + task in one tx. `llm_profile_id`, when
    /// present, becomes the single-entry chain. `timeout_seconds` is the wall-clock
    /// budget (0 = no limit); the deadline is stamped later at first real run.
    pub async fn create_task(
        &self,
        description: &str,
        goal: &str,
        llm_profile_id: Option<i64>,
        timeout_seconds: i32,
        plan_heartbeat_seconds: i32,
    ) -> Result<Task> {
        let mut llm_profile_ids = Vec::new();
        if let Some(id) = llm_profile_id {
            llm_profile_ids.push(id);
        }
        self.create_task_with_options(
            description,
            goal,
            TaskCreateOptions {
                llm_profile_ids,
                timeout_seconds,
                plan_heartbeat_seconds,
                ..TaskCreateOptions::default()
            },
        )
        .await
    }

    /// `CreateTaskWithOptions`: create an exploration (with an origin fact node),
    /// the task row, direct source relations, the ordered LLM chain, and task-level
    /// intercept rules in one transaction.
    pub async fn create_task_with_options(
        &self,
        description: &str,
        goal: &str,
        mut opts: TaskCreateOptions,
    ) -> Result<Task> {
        if opts.source_task_ids.len() > MAX_TASK_SOURCE_COUNT {
            return Err(Error::msg(format!(
                "too many source tasks: got {}, maximum is {MAX_TASK_SOURCE_COUNT}",
                opts.source_task_ids.len()
            )));
        }
        opts.company_ids = normalize_task_company_ids(&opts.company_ids)?;

        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        let exp_id: i64 = tx
            .query_one(
                "INSERT INTO explorations(description, goal) VALUES ($1,$2) RETURNING id",
                &[&description, &goal],
            )
            .await?
            .get::<_, i64>(0);

        // origin fact: the graph root (KindFact, state='origin') holding the original
        // description + goal; goals/intents/findings descend from it.
        let origin_payload = serde_json::json!({
            "summary": format!("Task origin: {description}; Goal: {goal}"),
            "description": description,
            "goal": goal,
        });
        tx.execute(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1, 'fact', $2, 0, 'origin', 'system')",
            &[&exp_id, &origin_payload],
        )
        .await?;

        let timeout_seconds = if opts.timeout_seconds < 0 {
            0
        } else {
            opts.timeout_seconds
        };
        let plan_heartbeat_seconds = normalize_heartbeat(opts.plan_heartbeat_seconds);
        let coverage_enabled = opts.coverage_enabled.unwrap_or(true);

        let mut category_name = String::new();
        if let Some(cid) = opts.category_id {
            if cid <= 0 {
                return Err(Error::sentinel(
                    crate::db::task_categories::ERR_TASK_CATEGORY_INVALID,
                    format!(
                        "{}: category id must be positive",
                        crate::db::task_categories::ERR_TASK_CATEGORY_INVALID
                    ),
                ));
            }
            let row = tx
                .query_opt("SELECT name FROM task_categories WHERE id=$1", &[&cid])
                .await?;
            let Some(row) = row else {
                return Err(Error::sentinel(
                    crate::db::task_categories::ERR_TASK_CATEGORY_NOT_FOUND,
                    crate::db::task_categories::ERR_TASK_CATEGORY_NOT_FOUND,
                ));
            };
            category_name = row.get::<_, String>(0);
        }

        let active: Option<i64> = opts.llm_profile_ids.first().copied();
        let quiet_mode = opts.plan_quiet_mode.unwrap_or(true);
        let rotate = opts.llm_rotate.unwrap_or(false);
        let target_gate = opts.target_gate.unwrap_or(false);

        let mut t = Task {
            name: opts.name.clone(),
            category_id: opts.category_id,
            category_name,
            description: description.to_string(),
            goal: goal.to_string(),
            exploration_id: exp_id,
            llm_profile_id: active,
            active_llm_profile_id: active,
            llm_profile_ids: opts.llm_profile_ids.clone(),
            source_task_ids: opts.source_task_ids.clone(),
            company_ids: opts.company_ids.clone(),
            timeout_seconds,
            plan_heartbeat_seconds,
            plan_quiet_mode: quiet_mode,
            coverage_enabled,
            llm_rotate: rotate,
            target_gate,
            ..Task::default()
        };

        let row = tx
            .query_one(
                "INSERT INTO tasks(name, category_id, description, goal, exploration_id, \
                 llm_profile_id, active_llm_profile_id, timeout_seconds, \
                 plan_heartbeat_seconds, plan_quiet_mode, coverage_enabled, llm_rotate, \
                 target_gate) \
                 VALUES ($1,$2,$3,$4,$5,$6,$6,$7,$8,$9,$10,$11,$12) \
                 RETURNING id, status, paused, created_at",
                &[
                    &opts.name,
                    &opts.category_id,
                    &description,
                    &goal,
                    &exp_id,
                    &active,
                    &timeout_seconds,
                    &plan_heartbeat_seconds,
                    &quiet_mode,
                    &coverage_enabled,
                    &rotate,
                    &target_gate,
                ],
            )
            .await?;
        t.id = row.get::<_, i64>(0);
        t.status = row.get(1);
        t.paused = row.get(2);
        t.created_at = row.get(3);

        insert_task_relations(&tx, t.id, &opts.source_task_ids).await?;
        insert_task_companies(&tx, t.id, &opts.company_ids).await?;
        DB::insert_task_llm_profiles(&tx, t.id, &opts.llm_profile_ids).await?;
        crate::db::task_intercept::insert_task_intercept_rules(&tx, t.id, &opts.intercept_rules)
            .await?;

        t.llm_failover_state = if opts.llm_profile_ids.is_empty() {
            "default".to_string()
        } else {
            "ready".to_string()
        };
        tx.commit().await?;
        Ok(t)
    }
}
