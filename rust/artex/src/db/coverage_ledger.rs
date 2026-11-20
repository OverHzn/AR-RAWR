//! Pure coverage-ledger logic — port of the DB-free half of
//! `db/coverage_ledger.go`: `BuildCoverageLedger`, `CoverageGap.String`,
//! `normRiskArea`, `normalizeCoverageEntry` and the `CoverageEntry` /
//! `CoverageGap` / `CoverageCompleteness` / `CoverageLedger` types.
//!
//! The three gate-pinned helpers (`ValidCoverageOutcome`,
//! `NormalizeCoverageField`, `CoverageCompletenessFor`) are re-exported from
//! `db-rs::port` rather than re-implemented. The `AssetStore` DB methods
//! (`RecordCoverage` / `CoverageEntryCount` / `CoverageLedger`) are a separate
//! later unit (they need the assets group); this module is the pure ledger
//! document that both the assets group and the report layer build on.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::db::{Error, Result};

pub use db_rs::port::{
    coverage_completeness_for as coverage_completeness_for_value, normalize_coverage_field,
    valid_coverage_outcome,
};

// Ledger outcomes.
pub const OUTCOME_REPORTED: &str = "reported";
pub const OUTCOME_NO_ISSUE_FOUND: &str = "no_issue_found";
pub const OUTCOME_RULED_OUT: &str = "ruled_out";
pub const OUTCOME_NOT_APPLICABLE: &str = "not_applicable";
pub const OUTCOME_NEEDS_FOLLOW_UP: &str = "needs_follow_up";

// Gap kinds.
pub const GAP_NEEDS_FOLLOW_UP: &str = "needs_follow_up";
pub const GAP_UNRECORDED_RISK_CLASS: &str = "unrecorded_risk_class";
pub const GAP_MISSING_EVIDENCE: &str = "missing_evidence";
pub const GAP_MISSING_REASON: &str = "missing_reason";

// Ledger field limits (characters, after normalisation). Over a limit the write
// is refused, never truncated.
pub const MAX_COVERAGE_SURFACE: usize = 200;
pub const MAX_COVERAGE_RISK_AREA: usize = 64;
pub const MAX_COVERAGE_EVIDENCE: usize = 500;
pub const MAX_COVERAGE_REASON: usize = 500;
/// `MaxCoverageRowsPerTask`: the current-row cap of one task (history uncapped).
pub const MAX_COVERAGE_ROWS_PER_TASK: i64 = 2000;

/// `CoverageEntry`: one agent_reported row — surface + risk_area + outcome +
/// evidence + reason. `updated_at` is RFC3339 text (empty until read from PG).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub surface: String,
    pub risk_area: String,
    pub outcome: String,
    pub evidence: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worker: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub intent_id: i64,
    #[serde(default)]
    pub updated_at: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// `CoverageGap`: one open item — a follow-up, an unrecorded mandatory class, or
/// an entry missing its required evidence/reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageGap {
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub surface: String,
    pub risk_area: String,
}

impl std::fmt::Display for CoverageGap {
    /// `CoverageGap.String`: `kind risk_area`, or `kind surface risk_area` when a
    /// surface is set.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.surface.is_empty() {
            write!(f, "{} {}", self.kind, self.risk_area)
        } else {
            write!(f, "{} {} {}", self.kind, self.surface, self.risk_area)
        }
    }
}

/// `CoverageCompleteness`: whether the run ended as a complete scan. Caveat is
/// always set: an incomplete run is never reported silently.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageCompleteness {
    pub complete: bool,
    pub state: String,
    pub caveat: String,
}

/// `CoverageLedger`: the ledger document of one task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageLedger {
    pub entries: Vec<CoverageEntry>,
    pub gaps: Vec<CoverageGap>,
    /// Mandatory classes with no entry (never "clean").
    pub unexamined: Vec<String>,
    /// Machine-observed: findings per risk class.
    pub findings_by_class: HashMap<String, i64>,
    pub completeness: CoverageCompleteness,
}

/// `CoverageCompletenessFor`: wrap the gate-pinned `db-rs` helper into a typed
/// [`CoverageCompleteness`]. The helper is infallible and returns a well-formed
/// JSON object, so the unwrap here cannot fail.
pub fn coverage_completeness_for(
    status: &str,
    paused: bool,
    budget: bool,
    past_deadline: bool,
) -> CoverageCompleteness {
    let value =
        db_rs::port::coverage_completeness_for(status.to_string(), paused, budget, past_deadline)
            .expect("coverage_completeness_for is infallible");
    serde_json::from_value(value).expect("coverage_completeness_for returns a well-formed object")
}

/// `normRiskArea`: lower-case of [`normalize_coverage_field`].
fn norm_risk_area(s: &str) -> String {
    let normalized = normalize_coverage_field(s.to_string()).unwrap_or_default();
    gocompat::strings::go_to_lower(&normalized)
}

/// `normalizeCoverageEntry`: fold the agent-reported fields to one line and check
/// required fields, the outcome and the length limits. Over a limit is refused,
/// never truncated.
pub fn normalize_coverage_entry(mut e: CoverageEntry) -> Result<CoverageEntry> {
    e.surface = normalize_coverage_field(e.surface).map_err(Error::msg)?;
    e.risk_area = norm_risk_area(&e.risk_area);
    e.evidence = normalize_coverage_field(e.evidence).map_err(Error::msg)?;
    e.reason = normalize_coverage_field(e.reason).map_err(Error::msg)?;
    if e.surface.is_empty() || e.risk_area.is_empty() {
        return Err(Error::msg("task, surface and risk_area are required"));
    }
    if !valid_coverage_outcome(e.outcome.clone()).unwrap_or(false) {
        return Err(Error::msg(
            "outcome must be one of reported, no_issue_found, ruled_out, not_applicable, needs_follow_up",
        ));
    }
    for (name, v, max) in [
        ("surface", &e.surface, MAX_COVERAGE_SURFACE),
        ("risk_area", &e.risk_area, MAX_COVERAGE_RISK_AREA),
        ("evidence", &e.evidence, MAX_COVERAGE_EVIDENCE),
        ("reason", &e.reason, MAX_COVERAGE_REASON),
    ] {
        if v.chars().count() > max {
            return Err(Error::msg(format!("{name} too long (max {max})")));
        }
    }
    Ok(e)
}

/// `BuildCoverageLedger`: derive the gaps from the entries and the mandatory
/// classes. Gap sources: (a) needs_follow_up entries; (b) a mandatory class with
/// no entry at all (unrecorded_risk_class, also listed as unexamined); (c) an
/// entry missing its required evidence (no_issue_found) or reason (ruled_out,
/// not_applicable). Nothing here reads findings: an absent finding never closes a
/// class. The unrecorded-class gaps come first (the central invariant).
pub fn build_coverage_ledger(
    entries: Vec<CoverageEntry>,
    mandatory: Vec<String>,
    c: CoverageCompleteness,
) -> CoverageLedger {
    let mut l = CoverageLedger {
        entries,
        gaps: Vec::new(),
        unexamined: Vec::new(),
        findings_by_class: HashMap::new(),
        completeness: c,
    };

    let mut seen: HashSet<String> = HashSet::new();
    for e in &l.entries {
        seen.insert(norm_risk_area(&e.risk_area));
        if e.outcome == OUTCOME_NEEDS_FOLLOW_UP {
            l.gaps.push(CoverageGap {
                kind: GAP_NEEDS_FOLLOW_UP.to_string(),
                surface: e.surface.clone(),
                risk_area: e.risk_area.clone(),
            });
        } else if e.outcome == OUTCOME_NO_ISSUE_FOUND && e.evidence.trim().is_empty() {
            l.gaps.push(CoverageGap {
                kind: GAP_MISSING_EVIDENCE.to_string(),
                surface: e.surface.clone(),
                risk_area: e.risk_area.clone(),
            });
        } else if (e.outcome == OUTCOME_RULED_OUT || e.outcome == OUTCOME_NOT_APPLICABLE)
            && e.reason.trim().is_empty()
        {
            l.gaps.push(CoverageGap {
                kind: GAP_MISSING_REASON.to_string(),
                surface: e.surface.clone(),
                risk_area: e.risk_area.clone(),
            });
        }
    }

    let mut classes: HashSet<String> = HashSet::new();
    for m in &mandatory {
        let m = norm_risk_area(m);
        if !m.is_empty() && !seen.contains(&m) && !classes.contains(&m) {
            classes.insert(m.clone());
            l.unexamined.push(m);
        }
    }
    l.unexamined.sort();

    let mut unrecorded: Vec<CoverageGap> = l
        .unexamined
        .iter()
        .map(|m| CoverageGap {
            kind: GAP_UNRECORDED_RISK_CLASS.to_string(),
            surface: String::new(),
            risk_area: m.clone(),
        })
        .collect();
    unrecorded.append(&mut l.gaps); // the central invariant first
    l.gaps = unrecorded;
    l
}
