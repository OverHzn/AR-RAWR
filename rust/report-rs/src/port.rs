//! The Rust port of `report/`.
//!
//! Faithful port of `Markdown(Input)` and the `quality.go` helpers: renders the
//! deliverable Markdown report from the exploration graph. `rust-anything diff`
//! checks behavior against what the source recorded.

#![forbid(unsafe_code)]

use gocompat::gotime::format_go_time;
use gocompat::strings::{go_fields_join, go_to_upper, go_trim_left, go_trim_space};
use serde_json::Value;
use std::collections::BTreeMap;

// ===========================================================================
// Constants
// ===========================================================================

const REPORT_MAX_GAPS: usize = 200;
const REPORT_MAX_REPORT_BYTES: usize = 8 << 10; // 8192

// db verification constants
const VERIFICATION_VERIFIED: &str = "verified";
const VERIFICATION_UNVERIFIED: &str = "unverified";
const VERIFICATION_BLOCKED: &str = "blocked";
const VERIFICATION_NOT_ASSESSED: &str = "not_assessed";
const VERIFICATION_FALSE_POSITIVE: &str = "false_positive";

// ===========================================================================
// Input / Quality types (mirror of the Go structs, JSON-decoded)
// ===========================================================================

#[derive(Debug, Clone, Default)]
struct Input {
    title: String,
    goal: String,
    generated_at: String, // RFC3339 string; formatted on render
    asset_counts: BTreeMap<String, i64>,
    findings: Vec<Node>,
    coverage: Option<CoverageLedger>,
    /// None = nil map (lookup failed / unavailable); Some(empty) = no rows (missing).
    quality: Option<BTreeMap<i64, FindingQuality>>,
}

#[derive(Debug, Clone, Default)]
struct Node {
    id: i64,
    #[allow(dead_code)]
    kind: String,
    payload: String,
}

#[derive(Debug, Clone, Default)]
struct FindingQuality {
    traffic_count: i64,
    id: i64,
    #[allow(dead_code)]
    status: String,
    poe_level: i64,
    verification_status: String,
    verification_note: String,
    counterevidence: String,
    confidence_rationale: String,
    report: String,
    evidence_version: i64,
    report_evidence_version: i64,
}

#[derive(Debug, Clone, Default)]
struct CoverageLedger {
    entries: Vec<CoverageEntry>,
    gaps: Vec<CoverageGap>,
    completeness: Option<CoverageCompleteness>,
}

#[derive(Debug, Clone, Default)]
struct CoverageEntry {
    #[allow(dead_code)]
    surface: String,
    #[allow(dead_code)]
    risk_area: String,
    #[allow(dead_code)]
    outcome: String,
}

#[derive(Debug, Clone, Default)]
struct CoverageGap {
    kind: String,
    #[allow(dead_code)]
    surface: String,
    risk_area: String,
}

#[derive(Debug, Clone, Default)]
struct CoverageCompleteness {
    #[allow(dead_code)]
    complete: bool,
    state: String,
    caveat: String,
}

// ===========================================================================
// Quality (proof-strength descriptor for one finding)
// ===========================================================================

#[derive(Debug, Clone, Default)]
struct Quality {
    poe: i64,
    verification: String,
    verification_note: String,
    confidence_rationale: String,
    counterevidence: String,
    traffic_count: i64,
    record_id: i64,
    report: String,
    evidence_version: i64,
    report_evidence_version: i64,
    missing: bool,
    unavailable: bool,
}

impl Quality {
    fn report_stale(&self) -> bool {
        report_stale(
            &self.report,
            self.evidence_version,
            self.report_evidence_version,
        )
    }
}

fn report_stale(report: &str, evidence_version: i64, report_evidence_version: i64) -> bool {
    !report.is_empty() && evidence_version != report_evidence_version
}

/// Build a Quality from the report's single quality lookup keyed by finding id.
/// None = lookup failed (unavailable); Some(empty) = no standalone rows (missing).
fn quality_from_rows(node_id: i64, rows: &Option<BTreeMap<i64, FindingQuality>>) -> Quality {
    let rows = match rows {
        None => {
            return Quality {
                unavailable: true,
                ..Default::default()
            };
        }
        Some(r) => r,
    };
    let q = match rows.get(&node_id) {
        None => {
            return Quality {
                missing: true,
                ..Default::default()
            };
        }
        Some(q) => q,
    };
    Quality {
        poe: q.poe_level,
        verification: q.verification_status.clone(),
        verification_note: q.verification_note.clone(),
        confidence_rationale: q.confidence_rationale.clone(),
        counterevidence: q.counterevidence.clone(),
        traffic_count: q.traffic_count,
        record_id: q.id,
        report: q.report.clone(),
        evidence_version: q.evidence_version,
        report_evidence_version: q.report_evidence_version,
        ..Default::default()
    }
}

// ===========================================================================
// quality.go helpers
// ===========================================================================

fn quality_bullets(q: &Quality, always_proof: bool) -> String {
    let mut b = String::new();
    if q.unavailable {
        b.push_str(
            "- **Verification**: unknown (quality lookup failed \u{2014} proof strength unavailable)\n\n",
        );
        return b;
    }
    if q.missing {
        b.push_str("- **Verification**: not assessed (no finding record \u{2014} quality fields unavailable)\n\n");
        return b;
    }
    if q.record_id > 0 {
        b.push_str(&format!("- **Finding record**: #{}\n", q.record_id));
    }
    b.push_str(&format!("- **PoE level**: {}\n", q.poe));
    b.push_str(&format!(
        "- **Verification**: {}\n",
        verification_label(&q.verification)
    ));
    if always_proof || q.traffic_count == 0 {
        if q.traffic_count > 0 {
            b.push_str(&format!(
                "- **Proof**: {} bound traffic exchange(s)\n",
                q.traffic_count
            ));
        } else {
            b.push_str("- **Proof**: no bound traffic evidence\n");
        }
    }
    let note = one_line(&q.verification_note);
    if !note.is_empty() {
        b.push_str(&format!("- **Verification note**: {}\n", note));
    }
    b.push('\n');
    b
}

fn quality_narrative(q: &Quality) -> String {
    if q.missing || q.unavailable {
        return String::new();
    }
    let mut b = String::new();
    let conf = go_trim_space(&q.confidence_rationale);
    if !conf.is_empty() {
        b.push_str(&format!("**Confidence rationale**:\n\n{}\n\n", conf));
    }
    let counter = go_trim_space(&q.counterevidence);
    if !counter.is_empty() {
        b.push_str(&format!("**Counterevidence**:\n\n{}\n\n", counter));
    }
    b
}

fn stale_report_notice(stale: bool) -> &'static str {
    if !stale {
        return "";
    }
    "The evidence has changed; the detailed report needs updating.\n\n"
}

/// detailedReportLimit: render the reporter-written body, capped at `limit` bytes.
fn detailed_report_limit(body: &str, heading: &str, limit: usize) -> String {
    let s = go_trim_space(body);
    if s.is_empty() {
        return String::new();
    }
    if limit > 0 && s.len() > limit {
        // Back off from `limit` to the nearest valid UTF-8 char boundary.
        // Rust's &str is always valid UTF-8, so we find the largest cut <= limit
        // that lands on a char boundary (Go's loop does `cut[:len(cut)-1]` until valid).
        let mut cut = limit;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        let cut = go_trim_space(&s[..cut]);
        let mut cut = cut.to_string();
        if unbalanced_fence(&cut) {
            cut.push_str("\n```");
        }
        return format!(
            "{}\n\n{}\n\n_Detailed report truncated at {} KiB; read the full text from the finding record._\n\n",
            heading,
            cut,
            limit >> 10
        );
    }
    format!("{}\n\n{}\n\n", heading, s)
}

fn detailed_report(body: &str, heading: &str) -> String {
    detailed_report_limit(body, heading, REPORT_MAX_REPORT_BYTES)
}

/// unbalancedFence: true if s ends inside an open fenced code block (odd ``` lines).
fn unbalanced_fence(s: &str) -> bool {
    let mut fences = 0;
    for line in s.split('\n') {
        let trimmed = go_trim_left(line, " \t");
        if trimmed.starts_with("```") {
            fences += 1;
        }
    }
    fences % 2 == 1
}

fn verification_label(s: &str) -> String {
    match s {
        "" | VERIFICATION_UNVERIFIED => "Unverified".to_string(),
        VERIFICATION_VERIFIED => "Verified".to_string(),
        VERIFICATION_BLOCKED => "Blocked".to_string(),
        VERIFICATION_NOT_ASSESSED => "Not assessed".to_string(),
        VERIFICATION_FALSE_POSITIVE => "False positive".to_string(),
        _ => s.to_string(),
    }
}

/// oneLine collapses whitespace runs onto a single space (Go `strings.Fields`).
fn one_line(s: &str) -> String {
    go_fields_join(s)
}

// ===========================================================================
// report.go: Markdown
// ===========================================================================

struct FindingView {
    vulnclass: String,
    name: String,
    severity: String,
    summary: String,
    poc: String,
    q: Quality,
}

fn parse_finding(n: &Node) -> FindingView {
    // Faithful JSON decode of the payload: vulnclass/name/severity/summary/evidence.poc.
    let mut vulnclass = String::new();
    let mut name = String::new();
    let mut severity = String::new();
    let mut summary = String::new();
    let mut poc = String::new();
    if let Ok(v) = serde_json::from_str::<Value>(&n.payload) {
        if let Some(s) = v.get("vulnclass").and_then(|x| x.as_str()) {
            vulnclass = s.to_string();
        }
        if let Some(s) = v.get("name").and_then(|x| x.as_str()) {
            name = s.to_string();
        }
        if let Some(s) = v.get("severity").and_then(|x| x.as_str()) {
            severity = s.to_string();
        }
        if let Some(s) = v.get("summary").and_then(|x| x.as_str()) {
            summary = s.to_string();
        }
        if let Some(s) = v
            .get("evidence")
            .and_then(|e| e.get("poc"))
            .and_then(|x| x.as_str())
        {
            poc = s.to_string();
        }
    }
    FindingView {
        vulnclass,
        name,
        severity,
        summary,
        poc,
        q: Quality::default(),
    }
}

/// Faithful Go map lookup: `sevRank[severity]` returns 0 for any key not present.
fn sev_rank_go(severity: &str) -> i64 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        "" => 4,
        _ => 0,
    }
}

pub fn markdown(r#in: Value) -> Result<String, String> {
    let input = parse_input(&r#in);
    let mut b = String::new();

    b.push_str(&format!(
        "# Penetration Test Report \u{2014} {}\n\n",
        nz(&input.title, "Unnamed task")
    ));
    b.push_str(&format!(
        "- **Task goal**: {}\n",
        nz(&input.goal, "(unspecified)")
    ));
    b.push_str(&format!(
        "- **Generated at**: {}\n\n",
        format_go_time(&input.generated_at)
    ));

    // summary
    b.push_str("## Summary\n\n");
    b.push_str(&format!(
        "- Confirmed findings: **{}**\n",
        input.findings.len()
    ));
    b.push_str("- Assets: ");
    let types: Vec<&String> = input.asset_counts.keys().collect();
    for (i, t) in types.iter().enumerate() {
        if i > 0 {
            b.push_str(", ");
        }
        b.push_str(&format!(
            "{} {}",
            t,
            input.asset_counts.get(*t).copied().unwrap_or(0)
        ));
    }
    b.push_str("\n\n");

    // findings
    b.push_str("## Findings\n\n");
    if input.findings.is_empty() {
        b.push_str("_No vulnerabilities confirmed this time._\n\n");
    } else {
        let mut fs: Vec<FindingView> = input
            .findings
            .iter()
            .map(|n| {
                let mut v = parse_finding(n);
                v.q = quality_from_rows(n.id, &input.quality);
                v
            })
            .collect();
        // sort.SliceStable by sev_rank
        fs.sort_by_key(|a| sev_rank_go(&a.severity));

        for (i, f) in fs.iter().enumerate() {
            b.push_str(&format!(
                "### {}. [{}] {}\n\n",
                i + 1,
                go_to_upper(nz(&f.severity, "info")),
                nz(&f.name, nz(&f.vulnclass, "Uncategorized"))
            ));
            b.push_str(&format!("{}\n\n", nz(&f.summary, "")));
            b.push_str(&quality_bullets(&f.q, true));
            if !f.poc.is_empty() {
                b.push_str(&format!("**PoC / Evidence:**\n\n```\n{}\n```\n\n", f.poc));
            }
            b.push_str(&quality_narrative(&f.q));
            b.push_str(stale_report_notice(f.q.report_stale()));
            b.push_str(&detailed_report(&f.q.report, "**Detailed report:**"));
        }
    }

    // coverage
    if let Some(c) = &input.coverage {
        b.push_str("## Coverage gaps\n\n");
        if let Some(comp) = &c.completeness {
            b.push_str(&format!(
                "- **Completeness**: {}. {}\n",
                comp.state, comp.caveat
            ));
        }
        b.push_str(&format!(
            "- Ledger entries: {}, open gaps: {}\n",
            c.entries.len(),
            c.gaps.len()
        ));
        for (i, g) in c.gaps.iter().enumerate() {
            if i == REPORT_MAX_GAPS {
                b.push_str(&format!(
                    "- and {} more (see the coverage API)\n",
                    c.gaps.len() - REPORT_MAX_GAPS
                ));
                break;
            }
            b.push_str(&format!("- {}\n", coverage_gap_string(g)));
        }
        b.push('\n');
    }

    Ok(b)
}

/// Render a CoverageGap the way Go's `CoverageGap.String()` does.
fn coverage_gap_string(g: &CoverageGap) -> String {
    if g.surface.is_empty() {
        format!("{} {}", g.kind, g.risk_area)
    } else {
        format!("{} {} {}", g.kind, g.surface, g.risk_area)
    }
}

// ===========================================================================
// Input parsing (JSON -> Input)
// ===========================================================================

fn parse_input(v: &Value) -> Input {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Input::default(),
    };
    Input {
        title: get_str(obj, "Title"),
        goal: get_str(obj, "Goal"),
        generated_at: get_str(obj, "GeneratedAt"),
        asset_counts: get_i64_map(obj, "AssetCounts"),
        findings: get_findings(obj, "Findings"),
        coverage: get_coverage(obj, "Coverage"),
        quality: get_quality(obj, "Quality"),
    }
}

fn get_str(obj: &serde_json::Map<String, Value>, k: &str) -> String {
    obj.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn get_i64_map(obj: &serde_json::Map<String, Value>, k: &str) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    if let Some(m) = obj.get(k).and_then(|x| x.as_object()) {
        for (key, val) in m {
            let n = val
                .as_i64()
                .or_else(|| val.as_f64().map(|f| f as i64))
                .unwrap_or(0);
            out.insert(key.clone(), n);
        }
    }
    out
}

fn get_findings(obj: &serde_json::Map<String, Value>, k: &str) -> Vec<Node> {
    let mut out = Vec::new();
    if let Some(arr) = obj.get(k).and_then(|x| x.as_array()) {
        for item in arr {
            let id = item.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
            let kind = item
                .get("kind")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            // payload arrives as a JSON string (json.RawMessage marshals to a string)
            let payload = match item.get("payload") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Object(_)) | Some(Value::Array(_)) => {
                    serde_json::to_string(item.get("payload").unwrap()).unwrap_or_default()
                }
                _ => String::new(),
            };
            out.push(Node { id, kind, payload });
        }
    }
    out
}

fn get_coverage(obj: &serde_json::Map<String, Value>, k: &str) -> Option<CoverageLedger> {
    match obj.get(k) {
        Some(Value::Null) | None => None,
        Some(v) => {
            let o = v.as_object()?;
            let entries = o
                .get("entries")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|e| CoverageEntry {
                            surface: e
                                .get("surface")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            risk_area: e
                                .get("risk_area")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            outcome: e
                                .get("outcome")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let gaps = o
                .get("gaps")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|g| CoverageGap {
                            kind: g
                                .get("kind")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            surface: g
                                .get("surface")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            risk_area: g
                                .get("risk_area")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let completeness = o.get("completeness").and_then(|c| {
                if c.is_null() {
                    None
                } else {
                    c.as_object().map(|co| CoverageCompleteness {
                        complete: co
                            .get("complete")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                        state: co
                            .get("state")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        caveat: co
                            .get("caveat")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                }
            });
            Some(CoverageLedger {
                entries,
                gaps,
                completeness,
            })
        }
    }
}

fn get_quality(
    obj: &serde_json::Map<String, Value>,
    k: &str,
) -> Option<BTreeMap<i64, FindingQuality>> {
    match obj.get(k) {
        None | Some(Value::Null) => None,
        Some(v) => {
            let m = v.as_object()?;
            let mut out = BTreeMap::new();
            for (key, val) in m {
                // Go map[int64]... keys are decimal strings in JSON
                let id: i64 = key.parse().unwrap_or(0);
                let o = match val.as_object() {
                    Some(o) => o,
                    None => continue,
                };
                out.insert(
                    id,
                    FindingQuality {
                        traffic_count: o.get("TrafficCount").and_then(|x| x.as_i64()).unwrap_or(0),
                        id: o.get("ID").and_then(|x| x.as_i64()).unwrap_or(0),
                        status: o
                            .get("Status")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        poe_level: o.get("PoeLevel").and_then(|x| x.as_i64()).unwrap_or(0),
                        verification_status: o
                            .get("VerificationStatus")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        verification_note: o
                            .get("VerificationNote")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        counterevidence: o
                            .get("Counterevidence")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        confidence_rationale: o
                            .get("ConfidenceRationale")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        report: o
                            .get("Report")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        evidence_version: o
                            .get("EvidenceVersion")
                            .and_then(|x| x.as_i64())
                            .unwrap_or(0),
                        report_evidence_version: o
                            .get("ReportEvidenceVersion")
                            .and_then(|x| x.as_i64())
                            .unwrap_or(0),
                    },
                );
            }
            Some(out)
        }
    }
}

// ===========================================================================
// Go stdlib helpers (faithful)
// ===========================================================================

/// Go `nz`: trim; empty -> default.
fn nz<'a>(s: &'a str, d: &'a str) -> &'a str {
    if go_trim_space(s).is_empty() { d } else { s }
}
