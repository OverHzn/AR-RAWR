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

use crate::db::error::Result;
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
