//! Standalone findings persistence — the foundation slice of `db/findings.go`.
//!
//! A `findings` row is the durable, task-independent record of a vulnerability
//! (it survives task deletion unless the caller explicitly cleans it up). This
//! module ports the self-contained core that has no dependency on the evidence /
//! traffic-binding cluster or the asset-scope resolver:
//!   - severity / status / report-state consts + validators,
//!   - the `DBFinding` row + `findingSelectCols` + `scan_findings`,
//!   - `AddFinding` / `GetFinding` / `ListFindings` / `DeleteFindingsByTask`,
//!   - the `setFindingCol` mirror-onto-node setters (`SetFindingStatus`,
//!     `SetFindingReportState`, `SetFindingSeverity`, `SetFindingName`,
//!     `SetFindingVulnClass`).
//!
//! The evidence-transaction delete (`DeleteFinding`), the bulk report-state batch
//! (`SetFindingReportStates`), the paginated filter / group / export / stats
//! cluster, and the `FindingTrafficBinding` export view land in a later slice
//! (they pull in `finding_traffic.go` + the asset-scope resolver).

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::exploration::{utf8_clean, Activity, ExplorationStore};
use crate::db::DB;

// Finding triage states (`findings.status`).
pub const FINDING_PENDING: &str = "pending";
pub const FINDING_IN_PROGRESS: &str = "in_progress";
pub const FINDING_CONFIRMED: &str = "confirmed";
pub const FINDING_RESOLVED: &str = "resolved";
pub const FINDING_FIXED: &str = "fixed";
pub const FINDING_FALSE_POSITIVE: &str = "false_positive";
pub const FINDING_IGNORED: &str = "ignored";
pub const FINDING_DUPLICATE: &str = "duplicate";
pub const FINDING_RISK_ACCEPTED: &str = "risk_accepted";

/// `ValidFindingStatus`: is `s` a known triage state?
pub fn valid_finding_status(s: &str) -> bool {
    matches!(
        s,
        FINDING_PENDING
            | FINDING_IN_PROGRESS
            | FINDING_CONFIRMED
            | FINDING_RESOLVED
            | FINDING_FIXED
            | FINDING_FALSE_POSITIVE
            | FINDING_IGNORED
            | FINDING_DUPLICATE
            | FINDING_RISK_ACCEPTED
    )
}

// Finding report stages (`findings.report_state`) — a workflow stage after triage,
// not a verdict; a finding can be confirmed and submitted at the same time.
pub const FINDING_REPORT_NONE: &str = "none";
pub const FINDING_REPORT_SUBMITTED: &str = "submitted";
pub const FINDING_REPORT_ACCEPTED: &str = "accepted";
pub const FINDING_REPORT_REJECTED: &str = "rejected";
pub const FINDING_REPORT_DUPLICATE: &str = "duplicate";

/// `ValidFindingReportState`: is `s` a known report stage?
pub fn valid_finding_report_state(s: &str) -> bool {
    matches!(
        s,
        FINDING_REPORT_NONE
            | FINDING_REPORT_SUBMITTED
            | FINDING_REPORT_ACCEPTED
            | FINDING_REPORT_REJECTED
            | FINDING_REPORT_DUPLICATE
    )
}

// Finding severity levels (`findings.severity`).
pub const SEVERITY_CRITICAL: &str = "critical";
pub const SEVERITY_HIGH: &str = "high";
pub const SEVERITY_MEDIUM: &str = "medium";
pub const SEVERITY_LOW: &str = "low";

/// `ValidSeverity`: is `s` a known severity level?
pub fn valid_severity(s: &str) -> bool {
    matches!(
        s,
        SEVERITY_CRITICAL | SEVERITY_HIGH | SEVERITY_MEDIUM | SEVERITY_LOW
    )
}

/// `FindingUnassignedTask`: the task filter sentinel for findings whose task is
/// absent (rows created without a task, or retained after their originating task
/// was deleted — the findings FK is `ON DELETE SET NULL`).
pub const FINDING_UNASSIGNED_TASK: &str = "__unassigned__";

/// `FindingUnassignedAsset`: the asset-scope sentinel for findings with "unlinked
/// assets" (empty asset_ids, or none of the ids still resolve in the assets table).
pub const FINDING_UNASSIGNED_ASSET: &str = "__none__";

/// `DBFinding`: one row in the standalone `findings` table. Persists across task
/// deletion unless the caller explicitly requests related finding cleanup.
#[derive(Debug, Clone, PartialEq)]
pub struct DBFinding {
    pub traffic_count: i64,
    pub evidence_version: i64,
    pub report_evidence_version: i64,
    pub id: i64,
    pub task_id: Option<i64>,
    pub node_id: Option<i64>,
    pub vulnclass: String,
    /// vulnerability name (readable title); empty → frontend falls back to vulnclass.
    pub name: String,
    pub severity: String,
    pub summary: String,
    pub evidence: String,
    pub worker: String,
    pub asset_ids: Vec<i64>,
    pub status: String,
    /// none / submitted / accepted / rejected / duplicate; separate from `status`.
    pub report_state: String,
    /// proof-of-exploitation level 0-4 (S1); 0 = none declared.
    pub poe_level: i32,
    pub verification_status: String,
    pub verification_note: String,
    /// detailed report (Markdown); only populated by `GetFinding`, not by list queries.
    pub report: String,
    pub created_at: SystemTime,
    /// populated via LEFT JOIN on tasks.
    pub task_description: String,
    /// what was tested and did not work; optional, "" = not given.
    pub counterevidence: String,
    /// why the conclusion holds; optional, "" = not given.
    pub confidence_rationale: String,
}

impl Default for DBFinding {
    fn default() -> Self {
        DBFinding {
            traffic_count: 0,
            evidence_version: 0,
            report_evidence_version: 0,
            id: 0,
            task_id: None,
            node_id: None,
            vulnclass: String::new(),
            name: String::new(),
            severity: String::new(),
            summary: String::new(),
            evidence: String::new(),
            worker: String::new(),
            asset_ids: Vec::new(),
            status: String::new(),
            report_state: String::new(),
            poe_level: 0,
            verification_status: String::new(),
            verification_note: String::new(),
            report: String::new(),
            created_at: SystemTime::UNIX_EPOCH,
            task_description: String::new(),
            counterevidence: String::new(),
            confidence_rationale: String::new(),
        }
    }
}

/// `findingSelectCols`: the column list every finding read selects (with the task
/// description join and the traffic-binding count subselect), so `scan_findings`
/// stays in sync across callers.
const FINDING_SELECT_COLS: &str = "f.id, f.task_id, f.node_id, f.vulnclass, COALESCE(f.name, ''), \
f.severity, f.summary, f.evidence, f.worker, f.asset_ids, COALESCE(f.status, 'pending'), \
COALESCE(f.report_state, 'none'), f.created_at, COALESCE(t.description, '') AS task_description, \
f.evidence_version, f.report_evidence_version, f.poe_level, \
COALESCE(f.verification_status, 'unverified'), COALESCE(f.verification_note, ''), \
f.counterevidence, f.confidence_rationale, \
(SELECT count(*) FROM finding_traffic_bindings b WHERE b.finding_id=f.id)";

/// `scanFindings` (single-row arm): build a `DBFinding` from a `FINDING_SELECT_COLS`
/// row (22 columns, no `report`). `asset_ids` is a JSONB int array.
fn scan_finding_row(row: &tokio_postgres::Row) -> DBFinding {
    let aids: serde_json::Value = row.get(9);
    DBFinding {
        id: row.get(0),
        task_id: row.get(1),
        node_id: row.get(2),
        vulnclass: row.get(3),
        name: row.get(4),
        severity: row.get(5),
        summary: row.get(6),
        evidence: row.get(7),
        worker: row.get(8),
        asset_ids: serde_json::from_value(aids).unwrap_or_default(),
        status: row.get(10),
        report_state: row.get(11),
        created_at: row.get(12),
        task_description: row.get(13),
        evidence_version: row.get(14),
        report_evidence_version: row.get(15),
        poe_level: row.get::<_, i16>(16) as i32,
        verification_status: row.get(17),
        verification_note: row.get(18),
        counterevidence: row.get(19),
        confidence_rationale: row.get(20),
        traffic_count: row.get(21),
        report: String::new(),
    }
}

impl DB {
    /// `GetFinding`: one finding (None if not found), including the `report` body.
    pub async fn get_finding(&self, id: i64) -> Result<Option<DBFinding>> {
        let row = self
            .query_opt(
                &format!(
                    "SELECT {FINDING_SELECT_COLS}, COALESCE(f.report, '') \
                     FROM findings f LEFT JOIN tasks t ON f.task_id = t.id WHERE f.id = $1"
                ),
                &[&id],
            )
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut f = scan_finding_row(&row);
        f.report = row.get(22);
        Ok(Some(f))
    }

    /// `ListFindings`: all findings newest-first (joined with task description).
    /// `limit <= 0` defaults to 500.
    pub async fn list_findings(&self, limit: i32) -> Result<Vec<DBFinding>> {
        let limit = if limit <= 0 { 500 } else { limit };
        let rows = self
            .query(
                &format!(
                    "SELECT {FINDING_SELECT_COLS} FROM findings f \
                     LEFT JOIN tasks t ON f.task_id = t.id \
                     ORDER BY f.created_at DESC LIMIT $1"
                ),
                &[&(limit as i64)],
            )
            .await?;
        Ok(rows.iter().map(scan_finding_row).collect())
    }

    /// `DeleteFindingsByTask`: remove all findings rows of a task. The originating
    /// exploration finding nodes cascade separately when the task's exploration
    /// subgraph is dropped. Returns the rows deleted.
    pub async fn delete_findings_by_task(&self, task_id: i64) -> Result<i64> {
        Ok(self
            .execute("DELETE FROM findings WHERE task_id=$1", &[&task_id])
            .await? as i64)
    }

    /// `setFindingCol`: update one text column on the finding row AND mirror the
    /// value into the originating exploration node payload under `json_key`, so the
    /// per-task Findings tab (which reads the node payload) stays in sync. Returns
    /// rows affected (0 when no finding has that id); the node sync is best-effort.
    /// `col` / `json_key` MUST be trusted constants (interpolated into SQL).
    async fn set_finding_col(&self, id: i64, col: &str, json_key: &str, val: &str) -> Result<i64> {
        let node_id = self
            .query_opt(
                &format!("UPDATE findings SET {col}=$1 WHERE id=$2 RETURNING node_id"),
                &[&val, &id],
            )
            .await?;
        let Some(row) = node_id else {
            return Ok(0);
        };
        let nid: Option<i64> = row.get(0);
        if let Some(nid) = nid {
            let _ = self
                .execute(
                    &format!(
                        "UPDATE exploration_nodes \
                         SET payload = jsonb_set(payload, '{{{json_key}}}', to_jsonb($1::text)) \
                         WHERE id = $2"
                    ),
                    &[&val, &nid],
                )
                .await;
        }
        Ok(1)
    }

    /// `SetFindingStatus`: update the triage state (+ node payload mirror). Returns
    /// rows affected. Low-level: no push event (production uses a notify variant).
    pub async fn set_finding_status(&self, id: i64, status: &str) -> Result<i64> {
        self.set_finding_col(id, "status", "status", status).await
    }

    /// `SetFindingReportState`: update the report stage (+ node payload mirror).
    /// Returns rows affected. No notification (report_state is a workflow stage).
    pub async fn set_finding_report_state(&self, id: i64, state: &str) -> Result<i64> {
        self.set_finding_col(id, "report_state", "report_state", state)
            .await
    }

    /// `SetFindingSeverity`: update severity (+ node payload mirror).
    pub async fn set_finding_severity(&self, id: i64, severity: &str) -> Result<i64> {
        self.set_finding_col(id, "severity", "severity", severity)
            .await
    }

    /// `SetFindingName`: update the vulnerability name (+ node payload mirror).
    /// Empty name is allowed (frontend falls back to the vuln class).
    pub async fn set_finding_name(&self, id: i64, name: &str) -> Result<i64> {
        self.set_finding_col(id, "name", "name", name).await
    }

    /// `SetFindingVulnClass`: update the vulnerability class (+ node payload mirror).
    pub async fn set_finding_vulnclass(&self, id: i64, vulnclass: &str) -> Result<i64> {
        self.set_finding_col(id, "vulnclass", "vulnclass", vulnclass)
            .await
    }
}

/// `FindingReportStateBatchResult`: one id's outcome in a bulk report-stage update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingReportStateBatchResult {
    pub id: i64,
    /// The prior report_state (`"none"` when NULL); empty when the id is unknown.
    pub from: String,
    pub found: bool,
    pub changed: bool,
}

impl DB {
    /// `DeleteFinding`: remove a finding entirely — the standalone `findings` row
    /// and its originating exploration node (`kind='finding'`), so it disappears
    /// from the list, the per-task Findings tab and the graph alike. Runs under the
    /// evidence lock; a missing finding is a no-op returning 0. Returns 1 when a
    /// row was deleted.
    pub async fn delete_finding(&self, id: i64) -> Result<i64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&crate::db::finding_evidence::FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        let result = async {
            match crate::db::finding_evidence::lock_finding_evidence_tx(&tx, id, None).await {
                Ok(()) => {}
                Err(e) if e.is(crate::db::finding_evidence::ERR_FINDING_NOT_FOUND) => {
                    return Ok(0);
                }
                Err(e) => return Err(e),
            }
            let node_id = tx
                .query_opt("DELETE FROM findings WHERE id=$1 RETURNING node_id", &[&id])
                .await?;
            if let Some(row) = node_id {
                let nid: Option<i64> = row.get(0);
                if let Some(nid) = nid {
                    tx.execute(
                        "DELETE FROM exploration_nodes WHERE id=$1 AND kind='finding'",
                        &[&nid],
                    )
                    .await?;
                }
            }
            Ok(1)
        }
        .await;
        match result {
            Ok(n) => {
                tx.commit().await?;
                Ok(n)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `setFindingReportStateTx`: set one finding's report_state (+ node payload
    /// mirror) inside a tx, under `FOR UPDATE`. Returns `(from, found, changed)`;
    /// `from` is the prior state (`"none"` when NULL). A missing id is `found=false`.
    pub(crate) async fn set_finding_report_state_tx(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        id: i64,
        state: &str,
    ) -> Result<(String, bool, bool)> {
        let row = tx
            .query_opt(
                "SELECT COALESCE(report_state,'none'), node_id FROM findings WHERE id=$1 FOR UPDATE",
                &[&id],
            )
            .await?;
        let Some(row) = row else {
            return Ok((String::new(), false, false));
        };
        let from: String = row.get(0);
        let node_id: Option<i64> = row.get(1);
        if from == state {
            return Ok((from, true, false));
        }
        tx.execute(
            "UPDATE findings SET report_state=$2 WHERE id=$1",
            &[&id, &state],
        )
        .await?;
        if let Some(nid) = node_id {
            tx.execute(
                "UPDATE exploration_nodes SET payload = jsonb_set(payload, '{report_state}', to_jsonb($1::text)) WHERE id = $2",
                &[&state, &nid],
            )
            .await?;
        }
        Ok((from, true, true))
    }

    /// `SetFindingReportStates`: update several findings to the same report stage in
    /// one evidence-locked transaction. Unknown ids come back `found=false` instead
    /// of hiding the others; a DB error rolls the whole batch back. No notification.
    pub async fn set_finding_report_states(
        &self,
        ids: &[i64],
        state: &str,
    ) -> Result<Vec<FindingReportStateBatchResult>> {
        let mut out: Vec<FindingReportStateBatchResult> = Vec::with_capacity(ids.len());
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&crate::db::finding_evidence::FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        let result = async {
            for &id in ids {
                let (from, found, changed) =
                    self.set_finding_report_state_tx(&tx, id, state).await?;
                out.push(FindingReportStateBatchResult {
                    id,
                    from,
                    found,
                    changed,
                });
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                tx.commit().await?;
                Ok(out)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `SetFindingReportByNodeID`: set the Markdown report on the finding row whose
    /// `node_id` matches, with no optimistic version guard. Returns rows affected
    /// (0 when no row). Thin wrapper over the versioned setter.
    pub async fn set_finding_report_by_node_id(&self, node_id: i64, report: &str) -> Result<i64> {
        self.set_finding_report_version_by_node_id(node_id, report, None)
            .await
    }
}

/// `FindingFilter`: narrows a paginated findings query. Empty-string fields mean
/// "no filter on that column". Sort is `"severity"` (severity desc, then newest)
/// or anything else (newest first). The `asset_*` fields are resolved from
/// `asset_scope` by `applyAssetScope` (not yet ported — see handoff); tests and
/// callers may set them directly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindingFilter {
    /// `high` | `medium` | `low` | `critical` (empty = any).
    pub severity: String,
    /// `pending` | `false_positive` | `ignored` | `resolved`.
    pub status: String,
    /// `none` | `submitted` | `accepted` | `rejected` | `duplicate`.
    pub report_state: String,
    /// `verified` | `unverified` | `blocked` | `not_assessed` | `false_positive`.
    pub verification_status: String,
    pub vulnclass: String,
    /// Task id in string form; empty/invalid = do not filter (sentinel = unassigned).
    pub task_id: String,
    /// Fuzzy keyword over name/class/summary/evidence/report body.
    pub query: String,
    /// `"severity"` | `"time"` (anything else = time).
    pub sort: String,
    /// A node key of the asset tree (`a:<id>` / `c:<id>` / `r:<domain>` /
    /// `__none__`); selecting a node selects its whole subtree. Empty = no filter.
    pub asset_scope: String,

    // Resolved from asset_scope by applyAssetScope; callers do not set them.
    /// All asset ids in the selected subtree.
    pub asset_ids: Vec<i64>,
    /// Only findings with "unlinked assets".
    pub asset_none: bool,
    /// The selected node does not exist under the current filter -> always empty.
    pub asset_miss: bool,
}

/// `FilterParam`: one owned positional argument produced by [`FindingFilter::where`],
/// converted to a `ToSql` reference at bind time.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterParam {
    /// A text equality or ILIKE pattern.
    Text(String),
    /// The task_id bigint.
    Bigint(i64),
    /// The `asset_ids @> ANY($n::jsonb[])` right-hand operand (one jsonb array per id).
    Containments(Vec<serde_json::Value>),
}

impl FilterParam {
    /// Borrow this parameter as a `ToSql` for binding.
    pub fn as_tosql(&self) -> &(dyn tokio_postgres::types::ToSql + Sync) {
        match self {
            FilterParam::Text(s) => s,
            FilterParam::Bigint(n) => n,
            FilterParam::Containments(v) => v,
        }
    }
}

/// `assetIDContainments`: turn asset ids into the right-hand operand set of a jsonb
/// containment check (used with the `idx_findings_asset_ids` GIN jsonb_path_ops
/// index). Each id becomes a one-element jsonb array string (`"[123]"`).
pub fn asset_id_containments(ids: &[i64]) -> Vec<String> {
    ids.iter().map(|id| format!("[{id}]")).collect()
}

impl FindingFilter {
    /// `where`: build the shared WHERE clause (used by the page + count queries)
    /// plus its positional args. All values are parameterized; `query` escapes
    /// ILIKE wildcards so user input is always matched literally. (Named
    /// `build_where` because `where` is a Rust keyword.)
    pub fn build_where(&self) -> (String, Vec<FilterParam>) {
        let mut conds: Vec<String> = Vec::new();
        let mut args: Vec<FilterParam> = Vec::new();

        // Simple text-equality columns (empty = no filter).
        for (col, val) in [
            ("severity", &self.severity),
            ("status", &self.status),
            ("report_state", &self.report_state),
            ("verification_status", &self.verification_status),
            ("vulnclass", &self.vulnclass),
        ] {
            if !val.is_empty() {
                args.push(FilterParam::Text(val.clone()));
                conds.push(format!("f.{col} = ${}", args.len()));
            }
        }

        // task_id is a bigint column, compared as an integer (cannot go through
        // the text path); empty/invalid values are ignored.
        if self.task_id == FINDING_UNASSIGNED_TASK {
            conds.push("(f.task_id IS NULL OR t.id IS NULL)".to_string());
        } else if let Ok(tid) = self.task_id.parse::<i64>() {
            if tid > 0 {
                args.push(FilterParam::Bigint(tid));
                conds.push(format!("f.task_id = ${}", args.len()));
            }
        }

        // Asset filter: asset_ids is a jsonb array; @> ANY(...) can use
        // idx_findings_asset_ids.
        if self.asset_miss {
            conds.push("FALSE".to_string());
        } else if self.asset_none {
            conds.push(
                "(jsonb_array_length(COALESCE(f.asset_ids, '[]'::jsonb)) = 0 \
                 OR NOT EXISTS (\
                     SELECT 1 FROM jsonb_array_elements_text(f.asset_ids) e(v) \
                     JOIN assets a ON a.id = e.v::bigint))"
                    .to_string(),
            );
        } else if !self.asset_ids.is_empty() {
            let containments: Vec<serde_json::Value> = asset_id_containments(&self.asset_ids)
                .iter()
                .map(|s| serde_json::from_str(s).expect("valid jsonb array literal"))
                .collect();
            args.push(FilterParam::Containments(containments));
            conds.push(format!("f.asset_ids @> ANY(${}::jsonb[])", args.len()));
        }

        // Fuzzy keyword search; escape ILIKE wildcards so input matches literally.
        let query = self.query.trim();
        if !query.is_empty() {
            let escaped = query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            args.push(FilterParam::Text(format!("%{escaped}%")));
            let p = format!("${}", args.len());
            conds.push(format!(
                "(COALESCE(f.name, '') ILIKE {p} ESCAPE '\\' OR \
                 f.vulnclass ILIKE {p} ESCAPE '\\' OR \
                 f.summary ILIKE {p} ESCAPE '\\' OR \
                 f.evidence ILIKE {p} ESCAPE '\\' OR \
                 COALESCE(f.report, '') ILIKE {p} ESCAPE '\\')"
            ));
        }

        if conds.is_empty() {
            return (String::new(), args);
        }
        (format!(" WHERE {}", conds.join(" AND ")), args)
    }
}

/// `FindingGroup`: one task-level bucket in the global findings view. `task_id` is
/// `None` for findings that never had a task and findings retained after their task
/// was deleted; those share one "unassigned/deleted" bucket.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingGroup {
    pub task_id: Option<i64>,
    pub task_name: String,
    pub task_description: String,
    pub task_status: String,
    pub count: i64,
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub last_found_at: SystemTime,
}

impl DB {
    /// `applyAssetScope`: resolve `asset_scope` into the concrete `asset_*` filter
    /// fields. Empty scope = no asset filtering; `__none__` = the unlinked-assets
    /// bucket. **Deferred:** a node key (`a:` / `c:` / `r:`) requires the asset
    /// tree (`buildFindingAssetTree`, not yet ported) to collect its subtree ids, so
    /// that case returns an explicit "not yet available" error rather than silently
    /// degrading into no filtering.
    pub(crate) fn apply_asset_scope(&self, mut f: FindingFilter) -> Result<FindingFilter> {
        let scope = f.asset_scope.trim();
        f.asset_ids = Vec::new();
        f.asset_none = false;
        f.asset_miss = false;
        if scope.is_empty() {
            return Ok(f);
        }
        if scope == FINDING_UNASSIGNED_ASSET {
            f.asset_none = true;
            return Ok(f);
        }
        Err(Error::msg(
            "asset-scope node resolution requires the asset tree (not yet ported)",
        ))
    }

    /// `ListFindingsPage`: one page of findings matching the filter, plus the total
    /// match count (for the pager). `page` is 1-based; `page_size <= 0` -> 20.
    pub async fn list_findings_page(
        &self,
        f: FindingFilter,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<DBFinding>, i64)> {
        let page = if page <= 0 { 1 } else { page };
        let page_size = if page_size <= 0 { 20 } else { page_size };
        let f = self.apply_asset_scope(f)?;
        let (where_sql, params) = f.build_where();

        let count_sql = format!(
            "SELECT COUNT(*) FROM findings f LEFT JOIN tasks t ON f.task_id = t.id{where_sql}"
        );
        let count_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_tosql()).collect();
        let total: i64 = self.query_one(&count_sql, &count_refs).await?.get(0);
        // Once the requested page is beyond the exact count, no data query is needed.
        let total_pages = (total - 1) / page_size as i64 + 1;
        if total == 0 || (page as i64) > total_pages {
            return Ok((Vec::new(), total));
        }

        let order = if f.sort == "severity" {
            "CASE f.severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2 WHEN 'low' THEN 1 ELSE 0 END DESC, f.created_at DESC, f.id DESC"
        } else {
            "f.created_at DESC, f.id DESC"
        };
        let limit = page_size as i64;
        let offset = (page - 1) as i64 * page_size as i64;
        let n = params.len();
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_tosql()).collect();
        refs.push(&limit);
        refs.push(&offset);
        let sql = format!(
            "SELECT {FINDING_SELECT_COLS} FROM findings f LEFT JOIN tasks t ON f.task_id = t.id{where_sql} \
             ORDER BY {order} LIMIT ${} OFFSET ${}",
            n + 1,
            n + 2
        );
        let rows = self.query(&sql, &refs).await?;
        Ok((rows.iter().map(scan_finding_row).collect(), total))
    }

    /// `ListFindingGroups`: a page of task-level groups matching the same filters.
    /// Returns `(groups, group_total, finding_total)` — the group and finding totals
    /// are independent so clients can page groups without losing the exact count.
    pub async fn list_finding_groups(
        &self,
        f: FindingFilter,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<FindingGroup>, i64, i64)> {
        let page = if page <= 0 { 1 } else { page };
        let page_size = if page_size <= 0 { 10 } else { page_size };
        let f = self.apply_asset_scope(f)?;
        let (where_sql, params) = f.build_where();
        let grouped = format!(
            " FROM findings f LEFT JOIN tasks t ON f.task_id=t.id{where_sql} \
             GROUP BY t.id, t.name, t.description, t.status, t.paused, t.queued"
        );

        let count_sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(finding_count),0)::bigint FROM (\
             SELECT COUNT(*) AS finding_count{grouped}) grouped_findings"
        );
        let count_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_tosql()).collect();
        let totals = self.query_one(&count_sql, &count_refs).await?;
        let group_total: i64 = totals.get(0);
        let finding_total: i64 = totals.get(1);
        let group_pages = (group_total - 1) / page_size as i64 + 1;
        if group_total == 0 || (page as i64) > group_pages {
            return Ok((Vec::new(), group_total, finding_total));
        }

        let order = if f.sort == "severity" {
            "MAX(CASE f.severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2 WHEN 'low' THEN 1 ELSE 0 END) DESC, MAX(f.created_at) DESC, t.id DESC NULLS LAST"
        } else {
            "MAX(f.created_at) DESC, t.id DESC NULLS LAST"
        };
        let limit = page_size as i64;
        let offset = (page - 1) as i64 * page_size as i64;
        let n = params.len();
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_tosql()).collect();
        refs.push(&limit);
        refs.push(&offset);
        let query = format!(
            "SELECT t.id, COALESCE(t.name,''), COALESCE(t.description,''), COALESCE(\
             CASE WHEN t.status IN ('done','failed','timeout') THEN t.status \
             WHEN t.queued THEN 'queued' WHEN t.paused THEN 'paused' ELSE t.status END, ''), \
             COUNT(*), \
             COUNT(*) FILTER (WHERE f.severity='critical'), \
             COUNT(*) FILTER (WHERE f.severity='high'), \
             COUNT(*) FILTER (WHERE f.severity='medium'), \
             COUNT(*) FILTER (WHERE f.severity='low'), \
             MAX(f.created_at){grouped} ORDER BY {order} LIMIT ${} OFFSET ${}",
            n + 1,
            n + 2
        );
        let rows = self.query(&query, &refs).await?;
        let mut groups = Vec::with_capacity(rows.len());
        for r in &rows {
            groups.push(FindingGroup {
                task_id: r.get::<_, Option<i64>>(0),
                task_name: r.get(1),
                task_description: r.get(2),
                task_status: r.get(3),
                count: r.get(4),
                critical: r.get(5),
                high: r.get(6),
                medium: r.get(7),
                low: r.get(8),
                last_found_at: r.get(9),
            });
        }
        Ok((groups, group_total, finding_total))
    }
}

/// `FindingMeta`: the standalone-row data (id, triage state, anchored assets) the
/// per-task view grafts onto its exploration-node findings. Deliberately lean —
/// the task list calls [`DB::finding_meta_by_node_id`] for up to a few hundred
/// nodes per page, so the multi-KB report body and the prose attestations are
/// NOT loaded here (see [`DB::finding_quality_by_node_id`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindingMeta {
    pub traffic_count: i64,
    pub id: i64,
    pub status: String,
    pub asset_ids: Vec<i64>,
}

/// `FindingQuality`: the report-only view of one standalone row — the triage meta
/// plus the proof-strength fields, the reporter-written body and the two evidence
/// versions that decide whether that body still describes the current evidence.
/// `verification_note`/`counterevidence`/`confidence_rationale` empty = "not
/// given"; `report` empty = the reporter never wrote one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindingQuality {
    pub meta: FindingMeta,
    pub poe_level: i32,
    pub verification_status: String,
    pub verification_note: String,
    pub counterevidence: String,
    pub confidence_rationale: String,
    pub report: String,
    pub evidence_version: i64,
    pub report_evidence_version: i64,
}

impl FindingQuality {
    /// `ReportStale`: the stored report predates the evidence it describes (written
    /// against an older evidence version), so it must be read as outdated rather
    /// than as the current claim.
    pub fn report_stale(&self) -> bool {
        !self.report.is_empty() && self.evidence_version != self.report_evidence_version
    }
}

/// `findingTrafficCountCol`: per-row bound-evidence count, spelled once so both
/// node-id lookups count the same way as `findingSelectCols`.
const FINDING_TRAFFIC_COUNT_COL: &str =
    "(SELECT count(*) FROM finding_traffic_bindings b WHERE b.finding_id=findings.id)";

/// `ListFindingsForExport` order: severity desc, then newest (the grouping order
/// of the "export summary report").
const FINDING_EXPORT_ORDER: &str = "CASE f.severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2 WHEN 'low' THEN 1 ELSE 0 END DESC, f.created_at DESC";

impl DB {
    /// `ListFindingsForExport`: findings for the export function, carrying the full
    /// `report` body and unpaginated. When `ids` is non-empty it exports exactly
    /// that set (ignoring the filter); otherwise it exports by filter. Ordered by
    /// severity desc, then time desc.
    pub async fn list_findings_for_export(
        &self,
        f: FindingFilter,
        ids: &[i64],
    ) -> Result<Vec<DBFinding>> {
        let rows = if !ids.is_empty() {
            let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("${}", i + 1)).collect();
            let sql = format!(
                "SELECT {FINDING_SELECT_COLS}, COALESCE(f.report, '') FROM findings f \
                 LEFT JOIN tasks t ON f.task_id = t.id \
                 WHERE f.id IN ({}) ORDER BY {FINDING_EXPORT_ORDER}",
                placeholders.join(",")
            );
            let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = ids
                .iter()
                .map(|id| id as &(dyn tokio_postgres::types::ToSql + Sync))
                .collect();
            self.query(&sql, &refs).await?
        } else {
            let scoped = self.apply_asset_scope(f)?;
            let (where_sql, params) = scoped.build_where();
            let sql = format!(
                "SELECT {FINDING_SELECT_COLS}, COALESCE(f.report, '') FROM findings f \
                 LEFT JOIN tasks t ON f.task_id = t.id{where_sql} ORDER BY {FINDING_EXPORT_ORDER}"
            );
            let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                params.iter().map(|p| p.as_tosql()).collect();
            self.query(&sql, &refs).await?
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut f = scan_finding_row(row);
            f.report = row.get(22);
            out.push(f);
        }
        Ok(out)
    }

    /// `FindingMetaByNodeID`: map a task's finding node ids to their standalone-row
    /// metadata (status + anchored assets + traffic count). `task_id <= 0` -> empty.
    pub async fn finding_meta_by_node_id(
        &self,
        task_id: i64,
    ) -> Result<std::collections::HashMap<i64, FindingMeta>> {
        let mut out = std::collections::HashMap::new();
        if task_id <= 0 {
            return Ok(out);
        }
        let rows = self
            .query(
                &format!(
                    "SELECT node_id, id, COALESCE(status,'pending'), asset_ids, {FINDING_TRAFFIC_COUNT_COL} \
                     FROM findings WHERE task_id=$1 AND node_id IS NOT NULL"
                ),
                &[&task_id],
            )
            .await?;
        for r in &rows {
            let nid: i64 = r.get(0);
            let aids: serde_json::Value = r.get(3);
            let m = FindingMeta {
                id: r.get(1),
                status: r.get(2),
                asset_ids: serde_json::from_value(aids).unwrap_or_default(),
                traffic_count: r.get(4),
            };
            out.insert(nid, m);
        }
        Ok(out)
    }

    /// `FindingQualityByNodeID`: map a task's finding node ids to everything the
    /// report renders (triage meta, poe level, verification, the two attestations,
    /// the reporter body and both evidence versions) in one query. `task_id <= 0`
    /// -> empty. The column order mirrors [`DB::finding_meta_by_node_id`] then the
    /// report-only columns.
    pub async fn finding_quality_by_node_id(
        &self,
        task_id: i64,
    ) -> Result<std::collections::HashMap<i64, FindingQuality>> {
        let mut out = std::collections::HashMap::new();
        if task_id <= 0 {
            return Ok(out);
        }
        let rows = self
            .query(
                &format!(
                    "SELECT node_id, id, COALESCE(status,'pending'), asset_ids, {FINDING_TRAFFIC_COUNT_COL}, \
                     COALESCE(poe_level,0)::int, COALESCE(verification_status,'unverified'), \
                     COALESCE(verification_note,''), COALESCE(counterevidence,''), COALESCE(confidence_rationale,''), \
                     evidence_version, report_evidence_version, COALESCE(report,'') \
                     FROM findings WHERE task_id=$1 AND node_id IS NOT NULL"
                ),
                &[&task_id],
            )
            .await?;
        for r in &rows {
            let nid: i64 = r.get(0);
            let aids: serde_json::Value = r.get(3);
            let q = FindingQuality {
                meta: FindingMeta {
                    id: r.get(1),
                    status: r.get(2),
                    asset_ids: serde_json::from_value(aids).unwrap_or_default(),
                    traffic_count: r.get(4),
                },
                poe_level: r.get(5),
                verification_status: r.get(6),
                verification_note: r.get(7),
                counterevidence: r.get(8),
                confidence_rationale: r.get(9),
                evidence_version: r.get(10),
                report_evidence_version: r.get(11),
                report: r.get(12),
            };
            out.insert(nid, q);
        }
        Ok(out)
    }
}

/// `ERR_FINDING_ORIGIN_UNAVAILABLE`: a retained finding no longer has a live owning
/// task + finding node from which a follow-up intent can be derived.
pub const ERR_FINDING_ORIGIN_UNAVAILABLE: &str = "finding origin is no longer available";

impl ExplorationStore {
    /// `AddFindingFollowUpIntent`: atomically create a priority-10 human intent from
    /// a LIVE finding node, copy that finding's asset anchors, record the
    /// finding --derived_from--> intent lineage edge, and persist the audit activity.
    /// The returned activity is the committed row (id + created_at filled) and can be
    /// broadcast as-is. A retained finding whose origin is gone is rejected with
    /// [`ERR_FINDING_ORIGIN_UNAVAILABLE`].
    #[allow(clippy::too_many_arguments)]
    pub async fn add_finding_follow_up_intent(
        &self,
        finding_id: i64,
        finding_node_id: i64,
        description: &str,
        mut audit: Activity,
    ) -> Result<(i64, Activity)> {
        let exp_id = self.id();
        let mut client = self.db().pool.get().await?;
        let tx = client.transaction().await?;
        let result = async {
            // Verify the finding's origin is still live and lock all three rows.
            let row = tx
                .query_opt(
                    "SELECT n.id FROM findings f \
                     JOIN tasks t ON t.id=f.task_id \
                     JOIN exploration_nodes n ON n.id=f.node_id AND n.exploration_id=t.exploration_id \
                     WHERE f.id=$1 AND f.node_id=$2 AND t.exploration_id=$3 AND n.kind='finding' \
                     FOR SHARE OF f, t, n",
                    &[&finding_id, &finding_node_id, &exp_id],
                )
                .await?;
            let Some(row) = row else {
                return Err(Error::sentinel(ERR_FINDING_ORIGIN_UNAVAILABLE, ERR_FINDING_ORIGIN_UNAVAILABLE));
            };
            let live_node_id: i64 = row.get(0);

            // Copy the finding's asset anchors into the payload + onto the new intent.
            let anchor_rows = tx
                .query(
                    "SELECT asset_id FROM exploration_anchors WHERE node_id=$1 ORDER BY asset_id",
                    &[&live_node_id],
                )
                .await?;
            let anchors: Vec<i64> = anchor_rows.iter().map(|r| r.get(0)).collect();

            let mut payload = serde_json::json!({
                "summary": description,
                "source_finding_id": finding_id,
                "source_finding_node_id": live_node_id,
            });
            if !anchors.is_empty() {
                payload["asset_ids"] = serde_json::json!(anchors);
            }

            // Insert the intent node.
            let intent_row = tx
                .query_one(
                    "INSERT INTO exploration_nodes(exploration_id,kind,payload,priority,state,origin) \
                     VALUES ($1,'intent',$2,10,'open','human') RETURNING id",
                    &[&exp_id, &payload],
                )
                .await?;
            let intent_id: i64 = intent_row.get(0);

            // Copy anchors and record the lineage edge.
            tx.execute(
                "INSERT INTO exploration_anchors(node_id,asset_id) \
                 SELECT $1, asset_id FROM exploration_anchors WHERE node_id=$2 \
                 ON CONFLICT DO NOTHING",
                &[&intent_id, &live_node_id],
            )
            .await?;
            tx.execute(
                "INSERT INTO exploration_edges(exploration_id,src_id,rel,dst_id) \
                 VALUES ($1,$2,$3,$4)",
                &[&exp_id, &live_node_id, &crate::db::constants::REL_DERIVED_FROM, &intent_id],
            )
            .await?;

            // Persist the audit activity inside the same transaction.
            audit.node_id = Some(intent_id);
            let summary = audit.summary.trim();
            audit.summary = if summary.is_empty() {
                String::new()
            } else {
                format!("{summary} #{intent_id}")
            };
            let metadata = if audit.metadata.is_null() {
                serde_json::json!({})
            } else {
                audit.metadata.clone()
            };
            let act_row = tx
                .query_one(
                    "INSERT INTO activity(exploration_id, node_id, worker, kind, tool, tool_use_id, \
                     is_error, summary, detail, metadata, input_tokens, output_tokens, \
                     cache_read_tokens, cache_write_tokens) \
                     VALUES ($1,$2,NULLIF($3,''),NULLIF($4,''),NULLIF($5,''),NULLIF($6,''),$7, \
                     NULLIF($8,''),NULLIF($9,''),$10,$11,$12,$13,$14) \
                     RETURNING id, created_at",
                    &[
                        &exp_id,
                        &audit.node_id,
                        &utf8_clean(&audit.worker),
                        &utf8_clean(&audit.kind),
                        &utf8_clean(&audit.tool),
                        &utf8_clean(&audit.tool_use_id),
                        &audit.is_error,
                        &utf8_clean(&audit.summary),
                        &utf8_clean(&audit.detail),
                        &metadata,
                        &audit.input_tokens,
                        &audit.output_tokens,
                        &audit.cache_read_tokens,
                        &audit.cache_write_tokens,
                    ],
                )
                .await?;
            audit.id = act_row.get(0);
            audit.created_at = act_row.get(1);
            audit.metadata = metadata;
            Ok((intent_id, audit))
        }
        .await;
        match result {
            Ok(v) => {
                tx.commit().await?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}
