//! Skill-usage ledger — port of `db/skill_usage.go`.
//!
//! One row per `Skill()` invocation (the always-on skill call ledger). Rows carry
//! only attribution dimensions (skill/agent/task/session + args length), never the
//! caller's args text, and deliberately outlive their task (`skill_usage` has no
//! foreign keys, so deleting a task keeps its skill statistics intact — same as
//! `llm_usage`).
//!
//! The aggregate readers ([`DB::skill_stats`], [`DB::missing_skill_stats`]) merge
//! live rows with archived-task summaries via
//! [`merge_archived_skill_stats`](crate::db::task_archive_aggregate::merge_archived_skill_stats).

use crate::db::error::Result;
use crate::db::task_archive_aggregate::{archived_task_aggregates, merge_archived_skill_stats};
use crate::db::DB;
use std::time::SystemTime;

/// `SkillUsage` — one Skill() invocation's attribution dimensions.
#[derive(Debug, Clone, Default)]
pub struct SkillUsage {
    /// Skill directory name (matches `agent_skill_visibility.skill_name`).
    pub skill: String,
    /// worker / planner / mainagent / custom agent key.
    pub agent_key: String,
    /// 0 for non-task runs (chat sessions).
    pub task_id: i64,
    /// 0 when unknown.
    pub exploration_id: i64,
    /// worker's intent node; 0 for planner/mainagent/chat.
    pub intent_id: i64,
    /// chat conversation id; empty for task runs.
    pub session_id: String,
    pub args_len: i32,
    /// false = the model named a skill that does not exist (a miss).
    pub found: bool,
}

/// `nullIfZero`: `> 0` passes through, else SQL `NULL` (binds as `Option`).
pub(crate) fn null_if_zero(v: i64) -> Option<i64> {
    if v > 0 {
        Some(v)
    } else {
        None
    }
}

/// `SkillStat` — one skill's aggregate usage for the skills page. Serializes with
/// Go's `time.Time` RFC3339 shape so it round-trips through the archive manifest.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillStat {
    pub skill: String,
    pub calls: i64,
    /// Distinct tasks that loaded it (chat runs excluded).
    pub tasks: i64,
    /// Agent keys that loaded it, most-used first.
    #[serde(default)]
    pub agents: Vec<String>,
    /// nil when never called.
    #[serde(default, with = "crate::db::gotime_serde::opt_system_time_rfc3339")]
    pub last_used: Option<SystemTime>,
}

/// `SkillCall` — one row of a skill's recent-call list (detail panel).
#[derive(Debug, Clone)]
pub struct SkillCall {
    pub ts: SystemTime,
    pub agent_key: String,
    pub task_id: i64,
    pub session_id: String,
    pub args_len: i32,
}

impl DB {
    /// `InsertSkillUsage`: append one ledger row (best-effort at the call site).
    pub async fn insert_skill_usage(&self, u: &SkillUsage) -> Result<()> {
        self.execute(
            "INSERT INTO skill_usage(skill, agent_key, task_id, exploration_id, intent_id, session_id, args_len, found) \
             VALUES ($1, NULLIF($2,''), $3, $4, $5, NULLIF($6,''), $7, $8)",
            &[
                &u.skill,
                &u.agent_key,
                &null_if_zero(u.task_id),
                &null_if_zero(u.exploration_id),
                &null_if_zero(u.intent_id),
                &u.session_id,
                &u.args_len,
                &u.found,
            ],
        )
        .await?;
        Ok(())
    }

    /// `SkillStats`: aggregate the whole ledger grouped by skill, most-used first,
    /// merged with archived summaries. Only resolved calls (`found`) count.
    pub async fn skill_stats(&self) -> Result<Vec<SkillStat>> {
        let rows = self
            .query(
                "SELECT skill, COUNT(*) AS calls, \
                        COUNT(DISTINCT task_id) AS tasks, \
                        COALESCE(STRING_AGG(DISTINCT agent_key, ','), '') AS agents, \
                        MAX(ts) AS last_used \
                 FROM skill_usage \
                 WHERE found \
                 GROUP BY skill \
                 ORDER BY COUNT(*) DESC, skill",
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let agents_csv: String = row.get(3);
            out.push(SkillStat {
                skill: row.get(0),
                calls: row.get(1),
                tasks: row.get(2),
                agents: split_csv(&agents_csv),
                last_used: row.get(4),
            });
        }
        let archived = archived_task_aggregates(self).await?;
        Ok(merge_archived_skill_stats(out, &archived, false))
    }

    /// `MissingSkillStats`: skill names agents asked for that do not exist, most-
    /// requested first, merged with archived summaries and capped at `limit`
    /// (default 20 for `limit <= 0 || limit > 200`).
    pub async fn missing_skill_stats(&self, limit: i64) -> Result<Vec<SkillStat>> {
        let limit = if limit <= 0 || limit > 200 { 20 } else { limit };
        let rows = self
            .query(
                "SELECT skill, COUNT(*) AS calls, \
                        COALESCE(STRING_AGG(DISTINCT agent_key, ','), '') AS agents, \
                        MAX(ts) AS last_used \
                 FROM skill_usage \
                 WHERE NOT found \
                 GROUP BY skill \
                 ORDER BY COUNT(*) DESC, skill",
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let agents_csv: String = row.get(2);
            out.push(SkillStat {
                skill: row.get(0),
                calls: row.get(1),
                tasks: 0,
                agents: split_csv(&agents_csv),
                last_used: row.get(3),
            });
        }
        let archived = archived_task_aggregates(self).await?;
        let mut out = merge_archived_skill_stats(out, &archived, true);
        if out.len() as i64 > limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    /// `RecentSkillCalls`: the most recent invocations of one skill, newest first
    /// (default 50 for `limit <= 0 || limit > 200`).
    pub async fn recent_skill_calls(&self, skill: &str, limit: i64) -> Result<Vec<SkillCall>> {
        let limit = if limit <= 0 || limit > 200 { 50 } else { limit };
        let rows = self
            .query(
                "SELECT ts, COALESCE(agent_key,''), COALESCE(task_id,0), COALESCE(session_id,''), args_len \
                 FROM skill_usage \
                 WHERE skill = $1 AND found \
                 ORDER BY ts DESC \
                 LIMIT $2",
                &[&skill, &limit],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(SkillCall {
                ts: row.get(0),
                agent_key: row.get(1),
                task_id: row.get(2),
                session_id: row.get(3),
                args_len: row.get(4),
            });
        }
        Ok(out)
    }

    /// `SkillCallsByTask`: count a task's skill loads, most-used first (resolved
    /// calls only).
    pub async fn skill_calls_by_task(&self, task_id: i64) -> Result<Vec<SkillStat>> {
        let rows = self
            .query(
                "SELECT skill, COUNT(*) AS calls, MAX(ts) AS last_used \
                 FROM skill_usage \
                 WHERE task_id = $1 AND found \
                 GROUP BY skill \
                 ORDER BY COUNT(*) DESC, skill",
                &[&task_id],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(SkillStat {
                skill: row.get(0),
                calls: row.get(1),
                tasks: 0,
                agents: Vec::new(),
                last_used: row.get(2),
            });
        }
        Ok(out)
    }
}

/// Split a comma-joined `STRING_AGG` value ("" → empty vec).
fn split_csv(csv: &str) -> Vec<String> {
    if csv.is_empty() {
        Vec::new()
    } else {
        csv.split(',').map(|s| s.to_string()).collect()
    }
}
