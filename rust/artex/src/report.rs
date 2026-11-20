//! Deliverable report rendering — port of `report/{report,quality,findings}.go`.
//!
//! Pure rendering over the already-ported `db` value types (`DBFinding`,
//! `FindingQuality`, `CoverageLedger`, `Node`). No DB IO here.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::db::coverage_ledger::{CoverageGap, CoverageLedger};
use crate::db::exploration::Node;
use crate::db::finding_evidence::{FindingTrafficBinding, TrafficEvidenceSnapshot};
use crate::db::findings::{DBFinding, FindingQuality};

const VERIFICATION_UNVERIFIED: &str = "unverified";
const VERIFICATION_VERIFIED: &str = "verified";
const VERIFICATION_BLOCKED: &str = "blocked";
const VERIFICATION_NOT_ASSESSED: &str = "not_assessed";
const VERIFICATION_FALSE_POSITIVE: &str = "false_positive";

const REPORT_MAX_GAPS: usize = 200;
const REPORT_MAX_REPORT_BYTES: usize = 8 << 10;

/// `Input`: everything the task report needs.
pub struct Input {
    pub title: String,
    pub goal: String,
    pub generated_at: SystemTime,
    pub asset_counts: BTreeMap<String, i64>,
    pub findings: Vec<Node>,
    /// `None` when the task's coverage feature is off.
    pub coverage: Option<CoverageLedger>,
    /// Quality keyed by finding node id. `None` map = lookup failed (unavailable);
    /// empty map = no standalone rows (nothing assessed).
    pub quality: Option<std::collections::HashMap<i64, FindingQuality>>,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            title: String::new(),
            goal: String::new(),
            generated_at: SystemTime::UNIX_EPOCH,
            asset_counts: BTreeMap::new(),
            findings: Vec::new(),
            coverage: None,
            quality: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct FindingView {
    vuln_class: String,
    name: String,
    severity: String,
    summary: String,
    poc: String,
    q: Quality,
}

/// `Quality`: proof-strength descriptor of one finding, shared by the task report
/// and the findings exports so one finding never says two different things.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quality {
    pub poe: i32,
    pub verification: String,
    pub verification_note: String,
    pub confidence_rationale: String,
    pub counterevidence: String,
    pub traffic_count: i64,
    pub record_id: i64,
    pub report: String,
    pub evidence_version: i64,
    pub report_evidence_version: i64,
    pub missing: bool,
    pub unavailable: bool,
}

impl Quality {
    pub fn report_stale(&self) -> bool {
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

/// `qualityFromRows`: nil map = lookup failed; missing key = no standalone row.
fn quality_from_rows(
    node_id: i64,
    rows: &Option<std::collections::HashMap<i64, FindingQuality>>,
) -> Quality {
    let Some(rows) = rows else {
        return Quality {
            unavailable: true,
            ..Quality::default()
        };
    };
    let Some(q) = rows.get(&node_id) else {
        return Quality {
            missing: true,
            ..Quality::default()
        };
    };
    Quality {
        poe: q.poe_level,
        verification: q.verification_status.clone(),
        verification_note: q.verification_note.clone(),
        confidence_rationale: q.confidence_rationale.clone(),
        counterevidence: q.counterevidence.clone(),
        traffic_count: q.meta.traffic_count,
        record_id: q.meta.id,
        report: q.report.clone(),
        evidence_version: q.evidence_version,
        report_evidence_version: q.report_evidence_version,
        ..Quality::default()
    }
}

fn quality_from_db_finding(f: &DBFinding) -> Quality {
    Quality {
        poe: f.poe_level,
        verification: f.verification_status.clone(),
        verification_note: f.verification_note.clone(),
        confidence_rationale: f.confidence_rationale.clone(),
        counterevidence: f.counterevidence.clone(),
        traffic_count: f.traffic_count,
        record_id: f.id,
        report: f.report.clone(),
        evidence_version: f.evidence_version,
        report_evidence_version: f.report_evidence_version,
        ..Quality::default()
    }
}

fn nz<'a>(s: &'a str, default: &'a str) -> &'a str {
    if s.trim().is_empty() {
        default
    } else {
        s
    }
}

fn one_line(s: &str) -> String {
    gocompat::strings::go_fields_join(s)
}

/// Go `time.Format("2006-01-02 15:04:05")` in UTC.
pub fn format_go_time(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}:{:02}",
        crate::db::gotime::civil_date_from_days(days),
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

fn verification_label(s: &str) -> &str {
    match s {
        "" | VERIFICATION_UNVERIFIED => "Unverified",
        VERIFICATION_VERIFIED => "Verified",
        VERIFICATION_BLOCKED => "Blocked",
        VERIFICATION_NOT_ASSESSED => "Not assessed",
        VERIFICATION_FALSE_POSITIVE => "False positive",
        other => other,
    }
}

fn quality_bullets(q: &Quality, always_proof: bool) -> String {
    let mut b = String::new();
    if q.unavailable {
        b.push_str(
            "- **Verification**: unknown (quality lookup failed — proof strength unavailable)\n\n",
        );
        return b;
    }
    if q.missing {
        b.push_str(
            "- **Verification**: not assessed (no finding record — quality fields unavailable)\n\n",
        );
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
        b.push_str(&format!("- **Verification note**: {note}\n"));
    }
    b.push('\n');
    b
}

fn quality_narrative(q: &Quality) -> String {
    if q.missing || q.unavailable {
        return String::new();
    }
    let mut b = String::new();
    let rationale = q.confidence_rationale.trim();
    if !rationale.is_empty() {
        b.push_str(&format!("**Confidence rationale**:\n\n{rationale}\n\n"));
    }
    let counter = q.counterevidence.trim();
    if !counter.is_empty() {
        b.push_str(&format!("**Counterevidence**:\n\n{counter}\n\n"));
    }
    b
}

fn stale_report_notice(stale: bool) -> &'static str {
    if stale {
        "The evidence has changed; the detailed report needs updating.\n\n"
    } else {
        ""
    }
}

fn unbalanced_fence(s: &str) -> bool {
    let fences = s
        .split('\n')
        .filter(|line| line.trim_start_matches([' ', '\t']).starts_with("```"))
        .count();
    fences % 2 == 1
}

fn detailed_report_limit(body: &str, heading: &str, limit: usize) -> String {
    let s = body.trim();
    if s.is_empty() {
        return String::new();
    }
    if limit > 0 && s.len() > limit {
        let mut end = limit;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut cut = s[..end].trim().to_string();
        if unbalanced_fence(&cut) {
            cut.push_str("\n```");
        }
        return format!(
            "{heading}\n\n{cut}\n\n_Detailed report truncated at {} KiB; read the full text from the finding record._\n\n",
            limit >> 10
        );
    }
    format!("{heading}\n\n{s}\n\n")
}

pub fn detailed_report(body: &str, heading: &str) -> String {
    detailed_report_limit(body, heading, REPORT_MAX_REPORT_BYTES)
}

pub fn detailed_report_full(body: &str, heading: &str) -> String {
    detailed_report_limit(body, heading, 0)
}

fn sev_rank(s: &str) -> i32 {
    match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        "" => 4,
        _ => 0,
    }
}

fn parse_finding(n: &Node) -> FindingView {
    let p: &serde_json::Value = &n.payload;
    let gs = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let poc = p
        .get("evidence")
        .and_then(|e| e.get("poc"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    FindingView {
        vuln_class: gs("vulnclass"),
        name: gs("name"),
        severity: gs("severity"),
        summary: gs("summary"),
        poc,
        q: Quality::default(),
    }
}

fn gap_string(g: &CoverageGap) -> String {
    if g.surface.is_empty() {
        format!("{} {}", g.kind, g.risk_area)
    } else {
        format!("{} {} {}", g.kind, g.surface, g.risk_area)
    }
}

/// `Markdown`: render the task report.
pub fn markdown(in_: &Input) -> String {
    let mut b = String::new();
    b.push_str(&format!(
        "# Penetration Test Report — {}\n\n",
        nz(&in_.title, "Unnamed task")
    ));
    b.push_str(&format!(
        "- **Task goal**: {}\n",
        nz(&in_.goal, "(unspecified)")
    ));
    b.push_str(&format!(
        "- **Generated at**: {}\n\n",
        format_go_time(in_.generated_at)
    ));

    b.push_str("## Summary\n\n");
    b.push_str(&format!(
        "- Confirmed findings: **{}**\n",
        in_.findings.len()
    ));
    b.push_str("- Assets: ");
    let types: Vec<&String> = in_.asset_counts.keys().collect();
    for (i, t) in types.iter().enumerate() {
        if i > 0 {
            b.push_str(", ");
        }
        b.push_str(&format!("{} {}", t, in_.asset_counts[*t]));
    }
    b.push_str("\n\n");

    b.push_str("## Findings\n\n");
    if in_.findings.is_empty() {
        b.push_str("_No vulnerabilities confirmed this time._\n\n");
    } else {
        let mut fs: Vec<FindingView> = in_
            .findings
            .iter()
            .map(|n| {
                let mut v = parse_finding(n);
                v.q = quality_from_rows(n.id, &in_.quality);
                v
            })
            .collect();
        fs.sort_by_key(|f| sev_rank(&f.severity));
        for (i, f) in fs.iter().enumerate() {
            b.push_str(&format!(
                "### {}. [{}] {}\n\n",
                i + 1,
                nz(&f.severity, "info").to_uppercase(),
                nz(&f.name, nz(&f.vuln_class, "Uncategorized"))
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

    if let Some(c) = &in_.coverage {
        b.push_str("## Coverage gaps\n\n");
        b.push_str(&format!(
            "- **Completeness**: {}. {}\n",
            c.completeness.state, c.completeness.caveat
        ));
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
            b.push_str(&format!("- {}\n", gap_string(g)));
        }
        b.push('\n');
    }
    b
}

fn sort_findings_for_export(fs: &mut [&DBFinding]) {
    fs.sort_by(|a, z| {
        let ra = sev_rank(&a.severity);
        let rz = sev_rank(&z.severity);
        if ra != rz {
            ra.cmp(&rz)
        } else {
            z.created_at.cmp(&a.created_at)
        }
    });
}

fn finding_title(f: &DBFinding) -> &str {
    nz(&f.name, nz(&f.vulnclass, "Uncategorized"))
}

fn finding_traffic_ids(f: &DBFinding) -> String {
    f.traffic_bindings
        .iter()
        .map(|b| b.id.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn finding_traffic_markdown(f: &DBFinding, attachments: bool) -> String {
    let stale = report_stale(&f.report, f.evidence_version, f.report_evidence_version);
    if f.traffic_bindings.is_empty() && !stale {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("\n## Related Traffic Evidence\n\n");
    out.push_str(&format!(
        "Evidence version: {}; bindings: {}.\n\n",
        f.evidence_version,
        f.traffic_bindings.len()
    ));
    out.push_str(stale_report_notice(stale));
    for (i, b) in f.traffic_bindings.iter().enumerate() {
        out.push_str(&format!(
            "{}. **Evidence #{} · {}** — `{} {}`, status {}\n",
            i + 1,
            b.id,
            b.role,
            b.snapshot.method,
            b.snapshot.url.replace('`', "%60"),
            b.snapshot.status
        ));
        if !b.note.is_empty() {
            out.push_str(&format!("   {}\n", b.note.replace('\n', "\n   ")));
        }
        if attachments {
            out.push_str(&format!("   [Request message](evidence/{}/{}/request.http) · [Response message](evidence/{}/{}/response.http)\n", f.id, b.id, f.id, b.id));
        }
    }
    out.push('\n');
    out
}

/// `FindingsMarkdown`: batch findings → summary Markdown.
pub fn findings_markdown(findings: &[&DBFinding], generated_at: SystemTime) -> String {
    let mut items: Vec<&DBFinding> = findings.to_vec();
    sort_findings_for_export(&mut items);
    let mut b = String::new();
    b.push_str("# Vulnerability Findings Summary Report\n\n");
    b.push_str(&format!(
        "- **Generated at**: {}\n",
        format_go_time(generated_at)
    ));
    b.push_str(&format!("- **Total findings**: {}\n\n", items.len()));
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for f in &items {
        *counts.entry(f.severity.as_str()).or_default() += 1;
    }
    b.push_str("## Summary\n\n| Severity | Count |\n| --- | --- |\n");
    for (key, label) in [
        ("critical", "Critical"),
        ("high", "High"),
        ("medium", "Medium"),
        ("low", "Low"),
    ] {
        b.push_str(&format!(
            "| {label} | {} |\n",
            counts.get(key).copied().unwrap_or(0)
        ));
    }
    b.push('\n');
    if items.is_empty() {
        b.push_str("_No matching vulnerabilities._\n");
        return b;
    }
    b.push_str("## Vulnerability Details\n\n");
    for (i, f) in items.iter().enumerate() {
        b.push_str(&format!(
            "### {}. [{}] {}\n\n",
            i + 1,
            nz(&f.severity, "info").to_uppercase(),
            finding_title(f)
        ));
        if !f.vulnclass.is_empty() {
            b.push_str(&format!("- **Class**: {}\n", f.vulnclass));
        }
        b.push_str(&format!("- **Status**: {}\n", nz(&f.status, "pending")));
        b.push_str(&format!(
            "- **Report stage**: {}\n",
            nz(&f.report_state, "none")
        ));
        let desc = f.task_description.trim();
        if !desc.is_empty() {
            b.push_str(&format!("- **Task**: {desc}\n"));
        }
        b.push_str(&format!(
            "- **Found at**: {}\n",
            format_go_time(f.created_at)
        ));
        let q = quality_from_db_finding(f);
        b.push_str(&quality_bullets(&q, false));
        let summary = f.summary.trim();
        if !summary.is_empty() {
            b.push_str(&format!("{summary}\n\n"));
        }
        let evidence = f.evidence.trim();
        if !evidence.is_empty() {
            b.push_str(&format!("**Evidence:**\n\n```\n{evidence}\n```\n\n"));
        }
        b.push_str(&quality_narrative(&q));
        b.push_str(&detailed_report_full(&f.report, "**Detailed report:**"));
        b.push_str(&finding_traffic_markdown(f, false));
        b.push_str("---\n\n");
    }
    b
}

/// `SingleFindingMarkdown`: one finding as a standalone Markdown document.
pub fn single_finding_markdown(f: &DBFinding, generated_at: SystemTime) -> String {
    let mut b = String::new();
    b.push_str(&format!(
        "# [{}] {}\n\n",
        nz(&f.severity, "info").to_uppercase(),
        finding_title(f)
    ));
    if !f.vulnclass.is_empty() {
        b.push_str(&format!("- **Class**: {}\n", f.vulnclass));
    }
    b.push_str(&format!("- **Severity**: {}\n", nz(&f.severity, "info")));
    b.push_str(&format!("- **Status**: {}\n", nz(&f.status, "pending")));
    b.push_str(&format!(
        "- **Report stage**: {}\n",
        nz(&f.report_state, "none")
    ));
    let desc = f.task_description.trim();
    if !desc.is_empty() {
        b.push_str(&format!("- **Task**: {desc}\n"));
    }
    b.push_str(&format!(
        "- **Found at**: {}\n",
        format_go_time(f.created_at)
    ));
    b.push_str(&format!(
        "- **Generated at**: {}\n",
        format_go_time(generated_at)
    ));
    let q = quality_from_db_finding(f);
    b.push_str(&quality_bullets(&q, false));
    let summary = f.summary.trim();
    if !summary.is_empty() {
        b.push_str(&format!("## Summary\n\n{summary}\n\n"));
    }
    let evidence = f.evidence.trim();
    if !evidence.is_empty() {
        b.push_str(&format!("## Evidence\n\n```\n{evidence}\n```\n\n"));
    }
    b.push_str(&quality_narrative(&q));
    b.push_str(&detailed_report_full(&f.report, "## Detailed Report"));
    b.push_str(&finding_traffic_markdown(f, true));
    b
}

/// `FindingFilename`: a safe `.md` filename for "one file per vulnerability".
pub fn finding_filename(f: &DBFinding) -> String {
    let sev = nz(&f.severity, "info");
    let title = finding_title(f);
    let raw = format!("{sev}_{title}_#{}", f.id);
    let mut name: String = String::new();
    let mut pending_underscore = false;
    for c in raw.chars() {
        if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
            if pending_underscore && !name.is_empty() {
                name.push('_');
            }
            pending_underscore = false;
            name.push(c);
        } else if !name.is_empty() {
            pending_underscore = true;
        }
    }
    let name = name.trim_matches(|c| c == '.' || c == '_').to_string();
    let mut name = if name.is_empty() {
        format!("finding_{}", f.id)
    } else {
        name
    };
    // Defensive: strip one more path layer (no-op after sanitization, but matches Go).
    if let Some(slash) = name.rfind('/') {
        name = name[slash + 1..].to_string();
    }
    if name.len() > 120 {
        let mut end = 120;
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    format!("{name}.md")
}

fn csv_cell(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let first = s.as_bytes()[0];
    if matches!(first, b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r') {
        format!("'{s}")
    } else {
        s.to_string()
    }
}

fn csv_quote(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// `FindingsCSV`: batch findings → CSV bytes (with a UTF-8 BOM).
pub fn findings_csv(findings: &[&DBFinding]) -> Vec<u8> {
    let mut items: Vec<&DBFinding> = findings.to_vec();
    sort_findings_for_export(&mut items);
    let mut out = String::from("\u{feff}");
    let mut row = |cells: &[String]| {
        out.push_str(
            &cells
                .iter()
                .map(|c| csv_quote(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    };
    row(&[
        "ID".into(),
        "Name".into(),
        "Class".into(),
        "Severity".into(),
        "Status".into(),
        "Report Stage".into(),
        "Task".into(),
        "Found At".into(),
        "Summary".into(),
        "Traffic Evidence Count".into(),
        "Traffic Evidence IDs".into(),
    ]);
    for f in &items {
        row(&[
            f.id.to_string(),
            csv_cell(finding_title(f)),
            csv_cell(&f.vulnclass),
            csv_cell(nz(&f.severity, "info")),
            csv_cell(nz(&f.status, "pending")),
            csv_cell(nz(&f.report_state, "none")),
            csv_cell(&f.task_description),
            format_go_time(f.created_at),
            csv_cell(f.summary.trim()),
            f.traffic_bindings.len().to_string(),
            csv_cell(&finding_traffic_ids(f)),
        ]);
    }
    out.into_bytes()
}

// Keep imports used by the rendering helpers honest (bindings are referenced via
// their fields in `finding_traffic_markdown`).
#[allow(dead_code)]
fn _types(_: &FindingTrafficBinding, _: &TrafficEvidenceSnapshot) {}
