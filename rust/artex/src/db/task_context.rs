//! Task context — port of `db/task_context.go`.
//!
//! The `Task` registry row plus the LLM failover chain that rides alongside it:
//! ordered per-task `task_llm_profiles` entries, the shared active-profile cursor,
//! inherited source-task relations, and company asset scopes. `hydrate*` fills a
//! `Task` with these side tables so one row scan yields the full context;
//! `ReplaceTaskLLMProfiles` / `MarkTaskLLMProfileQuotaExhausted*` mutate the chain
//! atomically (the chain lock order is shared with `DeleteProfile`).
//!
//! `CreateTask`/`GetTask`/`ListTasks` (which build a `Task` from SQL) land later;
//! this slice is the context layer those reads apply on top of.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::tasks::Task;
use crate::db::DB;
use crate::llm::Context;

/// `IntentBlockedLLMQuota`: the `blocked_reason` recorded on an intent whose LLM
/// quota is exhausted for its whole task chain.
pub const INTENT_BLOCKED_LLM_QUOTA: &str = "llm_quota_exhausted";

/// `TaskLLMProfile`: one ordered entry in a task's explicit failover chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLLMProfile {
    pub profile_id: i64,
    pub position: i32,
    pub status: String,
    pub last_error: String,
    pub exhausted_at: Option<SystemTime>,
}

/// `TaskSource`: one directly related task and its exploration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSource {
    pub task_id: i64,
    pub exploration_id: i64,
    pub description: String,
    pub goal: String,
    pub status: String,
}

/// `TaskLLMTransition`: the shared task-level result of marking one profile
/// quota-exhausted. `next_profile_id` is `None` when the explicit chain is
/// exhausted. `stale` means the chain was replaced after the failing call
/// selected its provider, so the caller must not persist a transition for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskLLMTransition {
    pub previous_profile_id: i64,
    pub next_profile_id: Option<i64>,
    pub chain_exhausted: bool,
    pub advanced: bool,
    pub stale: bool,
}

/// `taskBatchContext`: the per-task slice assembled by `hydrate_tasks_context`.
#[derive(Default)]
struct TaskBatchContext {
    legacy_profile_id: Option<i64>,
    active_profile_id: Option<i64>,
    revision: i64,
    chain: Vec<TaskLLMProfile>,
    source_ids: Vec<i64>,
    company_ids: Vec<i64>,
}

/// `truncateUTF8`: cap a string to at most `max_bytes` bytes, retreating to the
/// nearest char boundary so the result stays valid UTF-8. `<= 0` yields "".
fn truncate_utf8(value: &str, max_bytes: i32) -> String {
    if max_bytes <= 0 {
        return String::new();
    }
    if value.len() <= max_bytes as usize {
        return value.to_string();
    }
    let mut end = max_bytes as usize;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// `applyTaskLLMContext`: fold a task's LLM chain + cursor into the row fields.
/// A nil cursor on a non-empty chain is a persisted end-of-chain marker and is not
/// repaired from an earlier ready entry.
fn apply_task_llm_context(
    t: &mut Task,
    legacy_profile_id: Option<i64>,
    active_profile_id: Option<i64>,
    revision: i64,
    chain: &[TaskLLMProfile],
) {
    t.llm_profile_id = legacy_profile_id;
    t.active_llm_profile_id = active_profile_id;
    t.llm_chain_revision = revision;
    t.llm_profile_ids = Vec::with_capacity(chain.len());
    t.ready_llm_profile_ids = Vec::new();
    t.llm_failover_state = "default".to_string();
    t.llm_failover_reason = String::new();
    let mut latest: Option<&TaskLLMProfile> = None;
    let mut active_ready = false;
    for entry in chain {
        if entry.status == "ready" {
            t.ready_llm_profile_ids.push(entry.profile_id);
        }
        t.llm_profile_ids.push(entry.profile_id);
        if t.active_llm_profile_id == Some(entry.profile_id) && entry.status == "ready" {
            active_ready = true;
        }
        if let Some(ts) = entry.exhausted_at {
            if latest.is_none_or(|l| ts > l.exhausted_at.unwrap()) {
                latest = Some(entry);
            }
        }
    }
    if !chain.is_empty() {
        t.llm_failover_state = if active_ready {
            "ready".to_string()
        } else {
            "chain_exhausted".to_string()
        };
    }
    if let Some(l) = latest {
        t.llm_failover_reason = l.last_error.clone();
    }
}

impl DB {
    /// `taskLLMContext`: read the compatibility profile, current cursor, and
    /// ordered chain in one statement so hydration sees one matching snapshot.
    /// A missing task yields an error (Go's `sql.ErrNoRows`).
    async fn task_llm_context(
        &self,
        task_id: i64,
    ) -> Result<(Option<i64>, Option<i64>, i64, Vec<TaskLLMProfile>)> {
        let rows = self
            .query(
                "SELECT t.llm_profile_id, t.active_llm_profile_id, t.llm_chain_revision, \
                 p.profile_id, p.position, p.status, COALESCE(p.last_error,''), p.exhausted_at \
                 FROM tasks t \
                 LEFT JOIN task_llm_profiles p ON p.task_id=t.id \
                 WHERE t.id=$1 \
                 ORDER BY p.position NULLS LAST",
                &[&task_id],
            )
            .await?;
        let mut legacy_profile_id = None;
        let mut active_profile_id = None;
        let mut revision = 0_i64;
        let mut chain: Vec<TaskLLMProfile> = Vec::new();
        let mut found = false;
        for row in &rows {
            if !found {
                found = true;
                revision = row.get::<_, i64>(2);
                legacy_profile_id = row.get(0);
                active_profile_id = row.get(1);
            }
            let profile_id: Option<i64> = row.get(3);
            if let Some(pid) = profile_id {
                let position: Option<i32> = row.get(4);
                let status: Option<String> = row.get(5);
                let last_error: String = row.get(6);
                let exhausted_at: Option<SystemTime> = row.get(7);
                chain.push(TaskLLMProfile {
                    profile_id: pid,
                    position: position.unwrap_or(0),
                    status: status.unwrap_or_default(),
                    last_error,
                    exhausted_at,
                });
            }
        }
        if !found {
            return Err(Error::msg("no rows in result set"));
        }
        Ok((legacy_profile_id, active_profile_id, revision, chain))
    }

    /// `TaskSourceIDs`: the source task ids inherited by this task, ordered by the
    /// relation's creation time then id.
    pub async fn task_source_ids(&self, task_id: i64) -> Result<Vec<i64>> {
        let rows = self
            .query(
                "SELECT source_task_id FROM task_relations \
                 WHERE task_id=$1 ORDER BY created_at, source_task_id",
                &[&task_id],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get::<_, i64>(0)).collect())
    }

    /// `TaskCompanyIDs`: the companies whose asset scopes are available to the task
    /// (non-context form). `task_scope` rows remain the single source of truth.
    pub async fn task_company_ids(&self, task_id: i64) -> Result<Vec<i64>> {
        self.task_company_ids_context(&Context::background(), task_id)
            .await
    }

    /// `TaskCompanyIDsContext`: context-aware form of `TaskCompanyIDs`.
    pub async fn task_company_ids_context(&self, ctx: &Context, task_id: i64) -> Result<Vec<i64>> {
        if let Some(e) = ctx.err() {
            return Err(e.into());
        }
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&task_id];
        let fut = self.query(
            "SELECT company_id FROM task_scope \
             WHERE task_id=$1 AND kind='company' AND company_id IS NOT NULL \
             ORDER BY id",
            &params,
        );
        tokio::pin!(fut);
        let rows = tokio::select! {
            biased;
            () = ctx.cancelled() => {
                return Err(ctx.err().unwrap_or(crate::llm::Error::Canceled).into());
            }
            rows = fut => rows?,
        };
        Ok(rows.iter().map(|r| r.get::<_, i64>(0)).collect())
    }

    /// `TaskSources`: the full source-task rows (id, exploration, description, goal,
    /// status) for live (non-deleted) sources.
    pub async fn task_sources(&self, task_id: i64) -> Result<Vec<TaskSource>> {
        let rows = self
            .query(
                "SELECT t.id, t.exploration_id, t.description, t.goal, t.status \
                 FROM task_relations r \
                 JOIN tasks t ON t.id=r.source_task_id AND t.deleted_at IS NULL \
                 WHERE r.task_id=$1 \
                 ORDER BY r.created_at, r.source_task_id",
                &[&task_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TaskSource {
                task_id: r.get(0),
                exploration_id: r.get(1),
                description: r.get(2),
                goal: r.get(3),
                status: r.get(4),
            })
            .collect())
    }

    /// `TaskLLMProfiles`: the task's ordered chain as stored (no hydration fold).
    pub async fn task_llm_profiles(&self, task_id: i64) -> Result<Vec<TaskLLMProfile>> {
        let rows = self
            .query(
                "SELECT profile_id, position, status, COALESCE(last_error,''), exhausted_at \
                 FROM task_llm_profiles WHERE task_id=$1 ORDER BY position",
                &[&task_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TaskLLMProfile {
                profile_id: r.get(0),
                position: r.get(1),
                status: r.get(2),
                last_error: r.get(3),
                exhausted_at: r.get(4),
            })
            .collect())
    }

    /// `hydrateTaskContext`: fill a task's LLM chain, source ids, and company ids
    /// from the side tables. A nil cursor on a non-empty chain is kept as-is.
    pub async fn hydrate_task_context(&self, t: &mut Task) -> Result<()> {
        let (legacy, active, revision, chain) = self.task_llm_context(t.id).await?;
        t.source_task_ids = self.task_source_ids(t.id).await?;
        t.company_ids = self.task_company_ids(t.id).await?;
        apply_task_llm_context(t, legacy, active, revision, &chain);
        Ok(())
    }

    /// `hydrateTasksContext`: bulk hydrate many tasks with three queries instead
    /// of the 3N+1 per-task form `hydrateTaskContext` would issue.
    pub async fn hydrate_tasks_context(&self, tasks: &mut [Task]) -> Result<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        let mut contexts: HashMap<i64, TaskBatchContext> = HashMap::with_capacity(tasks.len());
        for t in tasks.iter() {
            contexts.insert(t.id, TaskBatchContext::default());
        }

        // LLM chains + cursor in one bulk LEFT JOIN.
        let rows = self
            .query(
                "SELECT t.id, t.llm_profile_id, t.active_llm_profile_id, t.llm_chain_revision, \
                 p.profile_id, p.position, p.status, COALESCE(p.last_error,''), p.exhausted_at \
                 FROM tasks t \
                 LEFT JOIN task_llm_profiles p ON p.task_id=t.id \
                 WHERE t.id=ANY($1::bigint[]) \
                 ORDER BY t.id, p.position NULLS LAST",
                &[&ids],
            )
            .await?;
        for row in &rows {
            let task_id: i64 = row.get(0);
            let Some(ctx) = contexts.get_mut(&task_id) else {
                continue;
            };
            ctx.revision = row.get(3);
            if let Some(legacy) = row.get::<_, Option<i64>>(1) {
                ctx.legacy_profile_id = Some(legacy);
            }
            if let Some(active) = row.get::<_, Option<i64>>(2) {
                ctx.active_profile_id = Some(active);
            }
            if let Some(pid) = row.get::<_, Option<i64>>(4) {
                let position: Option<i32> = row.get(5);
                let status: Option<String> = row.get(6);
                let last_error: String = row.get(7);
                let exhausted_at: Option<SystemTime> = row.get(8);
                ctx.chain.push(TaskLLMProfile {
                    profile_id: pid,
                    position: position.unwrap_or(0),
                    status: status.unwrap_or_default(),
                    last_error,
                    exhausted_at,
                });
            }
        }

        // Source-task relations.
        let rows = self
            .query(
                "SELECT task_id, source_task_id FROM task_relations \
                 WHERE task_id=ANY($1::bigint[]) \
                 ORDER BY task_id, created_at, source_task_id",
                &[&ids],
            )
            .await?;
        for row in &rows {
            let task_id: i64 = row.get(0);
            if let Some(ctx) = contexts.get_mut(&task_id) {
                ctx.source_ids.push(row.get(1));
            }
        }

        // Company scopes.
        let rows = self
            .query(
                "SELECT task_id, company_id FROM task_scope \
                 WHERE task_id=ANY($1::bigint[]) AND kind='company' AND company_id IS NOT NULL \
                 ORDER BY task_id, id",
                &[&ids],
            )
            .await?;
        for row in &rows {
            let task_id: i64 = row.get(0);
            if let Some(ctx) = contexts.get_mut(&task_id) {
                ctx.company_ids.push(row.get(1));
            }
        }

        for t in tasks.iter_mut() {
            if let Some(ctx) = contexts.get(&t.id) {
                t.source_task_ids = ctx.source_ids.clone();
                t.company_ids = ctx.company_ids.clone();
                apply_task_llm_context(
                    t,
                    ctx.legacy_profile_id,
                    ctx.active_profile_id,
                    ctx.revision,
                    &ctx.chain,
                );
            }
        }
        Ok(())
    }

    /// `insertTaskLLMProfiles`: write the ordered chain rows inside a tx. Rejects
    /// non-positive or duplicate ids (Go's per-position validation).
    async fn insert_task_llm_profiles(
        tx: &deadpool_postgres::Transaction<'_>,
        task_id: i64,
        profile_ids: &[i64],
    ) -> Result<()> {
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for (position, &profile_id) in profile_ids.iter().enumerate() {
            if profile_id <= 0 || !seen.insert(profile_id) {
                return Err(Error::msg(format!(
                    "invalid or duplicate LLM profile id {profile_id}"
                )));
            }
            tx.execute(
                "INSERT INTO task_llm_profiles(task_id, profile_id, position) VALUES ($1,$2,$3)",
                &[&task_id, &profile_id, &(position as i32)],
            )
            .await?;
        }
        Ok(())
    }

    /// `ReplaceTaskLLMProfiles`: atomically replace and reset the explicit chain.
    /// `active_profile_id=0` selects the first entry; an empty list restores the
    /// agent-binding/global fallback. `rotate=None` leaves `llm_rotate` unchanged.
    pub async fn replace_task_llm_profiles(
        &self,
        task_id: i64,
        profile_ids: &[i64],
        active_profile_id: i64,
        rotate: Option<bool>,
    ) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        // Keep the task -> profile lock order shared by all task LLM mutations and
        // DeleteProfile; inserts take KEY SHARE locks on llm_profiles via their FKs.
        let locked = tx
            .query_opt(
                "SELECT id FROM tasks WHERE id=$1 AND deleted_at IS NULL FOR UPDATE",
                &[&task_id],
            )
            .await?;
        if locked.is_none() {
            return Err(Error::msg(format!("task {task_id} not found")));
        }
        tx.execute(
            "DELETE FROM task_llm_profiles WHERE task_id=$1",
            &[&task_id],
        )
        .await?;
        Self::insert_task_llm_profiles(&tx, task_id, profile_ids).await?;

        let mut active = active_profile_id;
        if profile_ids.is_empty() {
            active = 0;
        } else if active == 0 {
            active = profile_ids[0];
        }
        if active > 0 && !profile_ids.contains(&active) {
            return Err(Error::msg(format!(
                "active LLM profile {active} is not in the task chain"
            )));
        }
        let active_param: Option<i64> = if active > 0 { Some(active) } else { None };
        tx.execute(
            "UPDATE tasks SET \
             active_llm_profile_id=$2, llm_profile_id=$2, \
             llm_chain_revision=llm_chain_revision+1, \
             llm_rotate=COALESCE($3::boolean,llm_rotate) \
             WHERE id=$1",
            &[&task_id, &active_param, &rotate],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// `MarkTaskLLMProfileQuotaExhausted`: advance the shared task cursor once. A
    /// late in-flight error from an older profile records that entry as exhausted
    /// but does not advance past the profile another call already selected.
    pub async fn mark_task_llm_profile_quota_exhausted(
        &self,
        task_id: i64,
        profile_id: i64,
        reason: &str,
    ) -> Result<TaskLLMTransition> {
        self.mark_task_llm_profile_quota_exhausted_inner(task_id, profile_id, 0, false, reason)
            .await
    }

    /// `MarkTaskLLMProfileQuotaExhaustedAtRevision`: apply a provider failure only
    /// when it belongs to the chain snapshot used to start that call.
    pub async fn mark_task_llm_profile_quota_exhausted_at_revision(
        &self,
        task_id: i64,
        profile_id: i64,
        revision: i64,
        reason: &str,
    ) -> Result<TaskLLMTransition> {
        self.mark_task_llm_profile_quota_exhausted_inner(
            task_id, profile_id, revision, true, reason,
        )
        .await
    }

    async fn mark_task_llm_profile_quota_exhausted_inner(
        &self,
        task_id: i64,
        profile_id: i64,
        revision: i64,
        check_revision: bool,
        reason: &str,
    ) -> Result<TaskLLMTransition> {
        let mut out = TaskLLMTransition::default();
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        let row = tx
            .query_one(
                "SELECT active_llm_profile_id, llm_chain_revision, llm_rotate \
                 FROM tasks WHERE id=$1 FOR UPDATE",
                &[&task_id],
            )
            .await?;
        let active: Option<i64> = row.get(0);
        let current_revision: i64 = row.get(1);
        out.previous_profile_id = profile_id;
        if check_revision && revision != current_revision {
            out.stale = true;
            tx.commit().await?;
            return Ok(out);
        }

        let entry = tx
            .query_opt(
                "SELECT position, status FROM task_llm_profiles \
                 WHERE task_id=$1 AND profile_id=$2",
                &[&task_id, &profile_id],
            )
            .await?;
        let Some(entry) = entry else {
            // The chain was edited while this request was in flight; its provider
            // error remains valid but must not mutate the new chain.
            tx.commit().await?;
            return Ok(out);
        };
        let position: i32 = entry.get(0);
        let entry_status: String = entry.get(1);

        // Once the cursor reached the end, late failures must be idempotent.
        let Some(active_id) = active else {
            out.chain_exhausted = true;
            tx.commit().await?;
            return Ok(out);
        };

        let reason = truncate_utf8(reason.trim(), 1000);
        if entry_status != "quota_exhausted" {
            tx.execute(
                "UPDATE task_llm_profiles \
                 SET status='quota_exhausted', last_error=$3, exhausted_at=now() \
                 WHERE task_id=$1 AND profile_id=$2",
                &[&task_id, &profile_id, &reason],
            )
            .await?;
        }

        // A late error from a profile that is no longer the cursor only records
        // the exhaustion; the cursor is left for the call that selected it.
        if active_id != profile_id {
            out.next_profile_id = Some(active_id);
            tx.commit().await?;
            return Ok(out);
        }

        let rotate = row.get::<_, bool>(2);
        let next = tx
            .query_opt(
                "SELECT profile_id FROM task_llm_profiles \
                 WHERE task_id=$1 AND ($3 OR position>$2) AND status='ready' \
                 ORDER BY position LIMIT 1",
                &[&task_id, &position, &rotate],
            )
            .await?;
        match next {
            Some(n) => {
                let next_id: i64 = n.get(0);
                out.advanced = true;
                out.next_profile_id = Some(next_id);
                tx.execute(
                    "UPDATE tasks SET \
                     active_llm_profile_id=$2, llm_profile_id=$2, \
                     llm_chain_revision=llm_chain_revision+1 \
                     WHERE id=$1",
                    &[&task_id, &next_id],
                )
                .await?;
            }
            None => {
                out.advanced = true;
                out.chain_exhausted = true;
                tx.execute(
                    "UPDATE tasks SET \
                     active_llm_profile_id=NULL, llm_profile_id=NULL, \
                     llm_chain_revision=llm_chain_revision+1 \
                     WHERE id=$1",
                    &[&task_id],
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(out)
    }
}
