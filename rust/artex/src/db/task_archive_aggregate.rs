//! Archived-task aggregate reader — port of `db/task_archive_aggregate_stats.go`.
//!
//! `taskArchiveAggregate` is the compact, non-sensitive statistics section of a
//! cold archive manifest. Fields are additive so older archive formats remain
//! readable when new dashboard dimensions are introduced — so we model only the
//! sections the metering readers ([`DB::skill_stats`], [`DB::tool_usage_counts`])
//! consume, and ignore the rest (token/finding sections) via serde's default
//! unknown-field tolerance.
//!
//! [`merge_archived_skill_stats`] folds archived summaries into the live per-skill
//! totals so cold data does not disappear from historical dashboards. The pure
//! [`merge_agent_keys`] is reused from `dbx_rs::port` (gate-pinned).

use crate::db::error::Result;
use crate::db::skill_usage::SkillStat;
use crate::db::DB;

/// `taskArchiveAggregate` — the subset of the archive stats the metering cluster
/// reads. Other sections (token/daily/finding) are ignored on parse.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct TaskArchiveAggregate {
    #[serde(default)]
    pub skills: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub skill_stats: Vec<SkillStat>,
    #[serde(default)]
    pub missing_skill_stats: Vec<SkillStat>,
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, i64>,
}

impl DB {
    /// `ArchivedAggregateStats`: compact summaries used by global dashboards so
    /// cold data does not disappear from historical totals.
    pub async fn archived_aggregate_stats(&self) -> Result<Vec<String>> {
        let rows = self
            .query(
                "SELECT archive.aggregate_stats \
                 FROM task_archives archive \
                 JOIN tasks task ON task.id=archive.task_id \
                 WHERE task.archived_at IS NOT NULL",
                &[],
            )
            .await?;
        // `aggregate_stats` is JSONB; render each to its JSON text (the Go method
        // returns `json.RawMessage`).
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let v: serde_json::Value = row.get(0);
            out.push(v.to_string());
        }
        Ok(out)
    }
}

/// `archivedTaskAggregates`: deserialize every archive's `aggregate_stats`.
pub(crate) async fn archived_task_aggregates(d: &DB) -> Result<Vec<TaskArchiveAggregate>> {
    let raw_items = d.archived_aggregate_stats().await?;
    let mut out = Vec::with_capacity(raw_items.len());
    for raw in &raw_items {
        let item: TaskArchiveAggregate =
            serde_json::from_str(raw).map_err(|e| crate::db::error::Error::msg(e.to_string()))?;
        out.push(item);
    }
    Ok(out)
}

/// `mergeArchivedSkillStats`: fold archived summaries into the live per-skill
/// totals. `missing = true` merges the archived *missing*-skill stats, else the
/// resolved-call stats (falling back to the v1 call-count map when a cold row has
/// no `skill_stats`). The result is sorted by calls desc, then skill asc.
pub fn merge_archived_skill_stats(
    live: Vec<SkillStat>,
    archived: &[TaskArchiveAggregate],
    missing: bool,
) -> Vec<SkillStat> {
    use std::collections::BTreeMap;
    let mut by_name: BTreeMap<String, SkillStat> = BTreeMap::new();
    for current in live {
        by_name.insert(current.skill.clone(), current);
    }
    for aggregate in archived {
        let cold_stats: Vec<SkillStat> = if missing {
            aggregate.missing_skill_stats.clone()
        } else if !aggregate.skill_stats.is_empty() {
            aggregate.skill_stats.clone()
        } else {
            // Format v1 initially retained only the call-count map. Preserve those
            // summaries even though agent and timestamp dimensions are unavailable.
            aggregate
                .skills
                .iter()
                .map(|(skill, calls)| SkillStat {
                    skill: skill.clone(),
                    calls: *calls,
                    tasks: 1,
                    agents: Vec::new(),
                    last_used: None,
                })
                .collect()
        };
        for cold in cold_stats {
            let current = by_name.entry(cold.skill.clone()).or_default();
            current.skill = cold.skill;
            current.calls += cold.calls;
            current.tasks += cold.tasks;
            current.agents = dbx_rs::port::merge_agent_keys(&current.agents, &cold.agents);
            if let Some(cold_lu) = cold.last_used {
                if current.last_used.is_none_or(|cur| cold_lu > cur) {
                    current.last_used = Some(cold_lu);
                }
            }
        }
    }
    let mut out: Vec<SkillStat> = by_name.into_values().collect();
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.skill.cmp(&b.skill)));
    out
}
