//! Tool-usage ledger — port of `db/tool_usage.go`.
//!
//! `ToolUsage` is one catalog tool invocation. It stores attribution dimensions
//! only — tool arguments and results are deliberately excluded from the ledger.
//! Runtime callers treat metering as best-effort so a statistics failure never
//! interrupts the tool itself.
//!
//! [`DB::tool_usage_counts`] merges the live per-key totals with archived-task
//! summaries (via [`archived_task_aggregates`](crate::db::task_archive_aggregate::archived_task_aggregates)).

use crate::db::error::Result;
use crate::db::skill_usage::null_if_zero;
use crate::db::task_archive_aggregate::archived_task_aggregates;
use crate::db::DB;

/// `ToolUsage` — one catalog tool invocation's attribution dimensions.
#[derive(Debug, Clone, Default)]
pub struct ToolUsage {
    pub tool_key: String,
    pub agent_key: String,
    /// 0 → SQL NULL (chat/non-task runs).
    pub task_id: i64,
    pub exploration_id: i64,
    pub intent_id: i64,
    pub session_id: String,
}

impl DB {
    /// `InsertToolUsage`: append one ledger row (best-effort at the call site).
    pub async fn insert_tool_usage(&self, u: &ToolUsage) -> Result<()> {
        self.execute(
            "INSERT INTO tool_usage(tool_key, agent_key, task_id, exploration_id, intent_id, session_id) \
             VALUES ($1, NULLIF($2,''), $3, $4, $5, NULLIF($6,''))",
            &[
                &u.tool_key,
                &u.agent_key,
                &null_if_zero(u.task_id),
                &null_if_zero(u.exploration_id),
                &null_if_zero(u.intent_id),
                &u.session_id,
            ],
        )
        .await?;
        Ok(())
    }

    /// `ToolUsageCounts`: invocation totals keyed by catalog tool key, merged with
    /// archived-task tool counts. Entries without calls are absent.
    pub async fn tool_usage_counts(&self) -> Result<std::collections::BTreeMap<String, i64>> {
        let rows = self
            .query(
                "SELECT tool_key, COUNT(*) FROM tool_usage GROUP BY tool_key",
                &[],
            )
            .await?;
        let mut out: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for row in &rows {
            let key: String = row.get(0);
            let calls: i64 = row.get(1);
            out.insert(key, calls);
        }
        let archived = archived_task_aggregates(self).await?;
        for aggregate in &archived {
            for (key, calls) in &aggregate.tools {
                *out.entry(key.clone()).or_insert(0) += calls;
            }
        }
        Ok(out)
    }
}
