//! Differential tests for `report/{report,quality,findings}.go` — all cases from
//! `report/report_test.go` ported verbatim. Pure — no Postgres needed.

use std::time::SystemTime;

use artex::db::coverage_ledger::{
    build_coverage_ledger, coverage_completeness_for, CoverageCompleteness, CoverageEntry,
};
use artex::db::exploration::Node;
use artex::db::findings::{DBFinding, FindingMeta, FindingQuality};
use artex::report::{
    detailed_report, detailed_report_full, finding_filename, findings_csv, findings_markdown,
    markdown, single_finding_markdown, Input,
};

fn epoch() -> SystemTime {
    SystemTime::UNIX_EPOCH
}

fn report_node(id: i64, severity: &str, name: &str) -> Node {
    Node {
        id,
        kind: "finding".to_string(),
        payload: serde_json::json!({
            "vulnclass": "cwe-602",
            "name": name,
            "severity": severity,
            "summary": format!("s{id}"),
            "evidence": { "poc": format!("proof-{id}") }
        }),
        ..Node {
            finding_id: 0,
            finding_node_id: 0,
            traffic_count: 0,
            id: 0,
            kind: String::new(),
            payload: serde_json::Value::Null,
            priority: 0,
            state: String::new(),
            origin: String::new(),
            owner: String::new(),
            blocked_reason: String::new(),
            delete_reason: String::new(),
            anchors: Vec::new(),
            created_at: epoch(),
            source_task_id: 0,
            inherited: false,
        }
    }
}

fn quality(id: i64, poe: i32, verification: &str) -> FindingQuality {
    FindingQuality {
        meta: FindingMeta {
            id,
            ..FindingMeta::default()
        },
        poe_level: poe,
        verification_status: verification.to_string(),
        ..FindingQuality::default()
    }
}

#[test]
fn markdown_coverage_gaps() {
    let l = build_coverage_ledger(
        vec![CoverageEntry {
            surface: "/a".to_string(),
            risk_area: "xss".to_string(),
            outcome: "needs_follow_up".to_string(),
            ..CoverageEntry::default()
        }],
        vec!["sqli".to_string()],
        coverage_completeness_for("timeout", false, false, true),
    );
    let md = markdown(&Input {
        title: "t".to_string(),
        coverage: Some(l),
        ..Input::default()
    });
    for w in [
        "## Coverage gaps",
        "timeout",
        "Incomplete scan",
        "open gaps: 2",
        "- unrecorded_risk_class sqli",
        "- needs_follow_up /a xss",
    ] {
        assert!(md.contains(w), "missing {w:?} in\n{md}");
    }
    // Coverage off -> no section.
    let md2 = markdown(&Input {
        title: "t".to_string(),
        ..Input::default()
    });
    assert!(!md2.contains("Coverage gaps"));
}

#[test]
fn markdown_coverage_gaps_cap() {
    let entries: Vec<CoverageEntry> = (0..230)
        .map(|_| CoverageEntry {
            surface: "/p".to_string(),
            risk_area: "xss".to_string(),
            outcome: "needs_follow_up".to_string(),
            ..CoverageEntry::default()
        })
        .collect();
    let md = markdown(&Input {
        title: "t".to_string(),
        coverage: Some(build_coverage_ledger(
            entries,
            Vec::new(),
            CoverageCompleteness::default(),
        )),
        ..Input::default()
    });
    assert_eq!(md.matches("\n- needs_follow_up /p xss").count(), 200);
    assert!(md.contains("- and 30 more (see the coverage API)"));
}

#[test]
fn markdown_finding_quality() {
    let mut row1 = quality(1, 3, "verified");
    row1.meta.traffic_count = 2;
    row1.confidence_rationale = "the 200 carries the victim's order body".to_string();
    row1.counterevidence = "PATCH on the same id returned 403".to_string();
    row1.report =
        "## Overview\n\nclient-supplied header\n\n## Fix\n\nbind role server-side".to_string();
    row1.evidence_version = 4;
    row1.report_evidence_version = 4;
    let row2 = quality(2, 0, "unverified");
    let mut rows = std::collections::HashMap::new();
    rows.insert(8, row1);
    rows.insert(9, row2);
    let md = markdown(&Input {
        title: "t".to_string(),
        generated_at: epoch(),
        findings: vec![
            report_node(8, "high", "BOLA"),
            report_node(9, "critical", "role header"),
        ],
        quality: Some(rows),
        ..Input::default()
    });
    for want in [
        "- **Finding record**: #1",
        "- **PoE level**: 3",
        "- **Verification**: Verified",
        "- **Proof**: 2 bound traffic exchange(s)",
        "**Confidence rationale**:\n\nthe 200 carries the victim's order body",
        "**Counterevidence**:\n\nPATCH on the same id returned 403",
        "**Detailed report:**\n\n## Overview\n\nclient-supplied header",
        "- **Finding record**: #2",
        "- **PoE level**: 0",
        "- **Verification**: Unverified",
        "- **Proof**: no bound traffic evidence",
    ] {
        assert!(md.contains(want), "missing {want:?} in\n{md}");
    }
    assert_eq!(md.matches("**Counterevidence**").count(), 1);
    assert_eq!(md.matches("**Detailed report:**").count(), 1);
    assert!(!md.contains("The evidence has changed"));
}

#[test]
fn markdown_finding_quality_no_row() {
    let mut empty = std::collections::HashMap::new();
    let md = markdown(&Input {
        title: "t".to_string(),
        findings: vec![report_node(3, "low", "x")],
        quality: Some(std::mem::take(&mut empty)),
        ..Input::default()
    });
    assert!(md.contains("- **Verification**: not assessed (no finding record"));
    assert!(!md.contains("- **PoE level**"));
}

#[test]
fn markdown_finding_quality_unavailable() {
    // quality = None -> unavailable (lookup failed).
    let md = markdown(&Input {
        title: "t".to_string(),
        findings: vec![report_node(3, "low", "x")],
        ..Input::default()
    });
    assert!(md.contains("- **Verification**: unknown (quality lookup failed"));
    assert!(!md.contains("no finding record"));
    assert!(!md.contains("- **PoE level**"));
    assert!(!md.contains("- **Proof**"));
}

fn stale_row() -> FindingQuality {
    let mut q = quality(5, 2, "verified");
    q.report = "## Overview\n\nstale body".to_string();
    q.evidence_version = 7;
    q.report_evidence_version = 6;
    q
}

#[test]
fn stale_report_warning_on_both_surfaces() {
    const WARNING: &str = "The evidence has changed; the detailed report needs updating.";
    let mut rows = std::collections::HashMap::new();
    rows.insert(4, stale_row());
    let md = markdown(&Input {
        title: "t".to_string(),
        findings: vec![report_node(4, "high", "stale one")],
        quality: Some(rows),
        ..Input::default()
    });
    assert_eq!(md.matches(WARNING).count(), 1);
    assert!(md.find(WARNING).unwrap() < md.find("**Detailed report:**").unwrap());
    let mut f = DBFinding {
        id: 5,
        vulnclass: "cwe-602".to_string(),
        severity: "high".to_string(),
        status: "fixed".to_string(),
        poe_level: 2,
        verification_status: "verified".to_string(),
        report: "## Overview\n\nstale body".to_string(),
        evidence_version: 7,
        report_evidence_version: 6,
        ..DBFinding::default()
    };
    for out in [
        findings_markdown(&[&f], epoch()),
        single_finding_markdown(&f, epoch()),
    ] {
        assert_eq!(out.matches(WARNING).count(), 1);
    }
    f.evidence_version = 6;
    assert!(!single_finding_markdown(&f, epoch()).contains(WARNING));
    assert!(!findings_markdown(&[&f], epoch()).contains(WARNING));
}

#[test]
fn markdown_detailed_report_fence_balance() {
    let unclosed = format!(
        "## Evidence\n\n```http\n{}",
        "GET /orders/7 HTTP/1.1\nHost: victim\n\n".repeat(400)
    );
    let cut = unclosed.trim();
    let cut = &cut[..8192];
    assert_eq!(
        cut.matches("```").count() % 2,
        1,
        "fixture cuts inside open fence"
    );
    let mut rows = std::collections::HashMap::new();
    rows.insert(1, quality(1, 3, "verified"));
    rows.get_mut(&1).unwrap().report = unclosed;
    rows.insert(2, quality(2, 1, "unverified"));
    let md = markdown(&Input {
        title: "t".to_string(),
        findings: vec![
            report_node(1, "high", "first"),
            report_node(2, "low", "second"),
        ],
        quality: Some(rows),
        ..Input::default()
    });
    assert_eq!(
        md.matches("```").count() % 2,
        0,
        "report left a code block open"
    );
    let start = md.find("**Detailed report:**").unwrap();
    let end = md.find("### 2.").unwrap();
    let block = &md[start..end];
    assert!(block.contains("_Detailed report truncated at 8 KiB"));
    assert!(block.find("\n```\n").unwrap() < block.find("_Detailed report truncated").unwrap());
    assert!(md.contains("### 2. [LOW] second"));
    assert!(md[md.find("### 2.").unwrap()..].contains("- **PoE level**: 1"));
    let plain = "word ".repeat(3000);
    assert!(!detailed_report(&plain, "**Detailed report:**").contains("```"));
    let closed = format!(
        "## Overview\n\n```\nstill readable\n```\n{}",
        "trailer line\n".repeat(700)
    );
    assert_eq!(
        detailed_report(&closed, "**Detailed report:**")
            .matches("```")
            .count()
            % 2,
        0
    );
}

#[test]
fn detailed_report_truncation() {
    let big = format!("{}€{}", "a".repeat(8192 - 2), "b".repeat(100));
    let got = detailed_report(&big, "**Detailed report:**");
    assert!(got.contains("_Detailed report truncated at 8 KiB"));
    assert!(got.is_char_boundary(got.len()));
    assert!(!got.contains('€'));
    assert!(!got.contains("bbbb"));
    let kept = detailed_report(&format!("€{}", "a".repeat(10)), "**Detailed report:**");
    assert!(kept.contains('€'));
    assert_eq!(detailed_report("   ", "**Detailed report:**"), "");
    assert!(detailed_report_full(&big, "## Detailed Report").contains(&"b".repeat(100)));
}

#[test]
fn findings_exports_carry_quality() {
    let f = DBFinding {
        id: 7,
        vulnclass: "cwe-602".to_string(),
        name: "role header".to_string(),
        severity: "critical".to_string(),
        status: "fixed".to_string(),
        summary: "sum".to_string(),
        evidence: "ev".to_string(),
        poe_level: 2,
        verification_status: "unverified".to_string(),
        verification_note: "no\nverbatim\nquote".to_string(),
        confidence_rationale: "reason".to_string(),
        counterevidence: "tried X".to_string(),
        report: "## Overview\n\nbody".to_string(),
        ..DBFinding::default()
    };
    for md in [
        findings_markdown(&[&f], epoch()),
        single_finding_markdown(&f, epoch()),
    ] {
        for want in [
            "- **Finding record**: #7",
            "- **PoE level**: 2",
            "- **Verification**: Unverified",
            "- **Verification note**: no verbatim quote",
            "**Confidence rationale**:\n\nreason",
            "**Counterevidence**:\n\ntried X",
        ] {
            assert!(md.contains(want), "missing {want:?} in\n{md}");
        }
        assert_eq!(md.matches("- **Proof**").count(), 1);
    }
    assert!(
        single_finding_markdown(&f, epoch()).contains("## Detailed Report\n\n## Overview\n\nbody")
    );
    assert!(
        findings_markdown(&[&f], epoch()).contains("**Detailed report:**\n\n## Overview\n\nbody")
    );
    // With a binding, the count is stated once in the traffic section, no Proof bullet.
    let mut bound = f.clone();
    bound.traffic_count = 2;
    bound
        .traffic_bindings
        .push(artex::db::finding_evidence::FindingTrafficBinding {
            id: 1,
            role: "proof".to_string(),
            ..artex::db::finding_evidence::FindingTrafficBinding::default()
        });
    let md = single_finding_markdown(&bound, epoch());
    assert!(!md.contains("- **Proof**"));
    assert!(md.contains("bindings: 1"));
}

#[test]
fn findings_csv_neutralizes_formulas() {
    let f = DBFinding {
        id: 7,
        name: "=HYPERLINK(\"http://evil.example/x\",\"x\")".to_string(),
        vulnclass: "+cmd".to_string(),
        severity: "high".to_string(),
        status: "pending".to_string(),
        report_state: "none".to_string(),
        task_description: "@SUM(1+1)".to_string(),
        summary: "-2+3".to_string(),
        ..DBFinding::default()
    };
    let out = findings_csv(&[&f]);
    assert!(out.starts_with(&[0xef, 0xbb, 0xbf]), "CSV lost BOM");
    let text = String::from_utf8_lossy(&out[3..]);
    let rows = parse_csv_rows(&text);
    assert_eq!(rows.len(), 2, "header + 1 row");
    let row = &rows[1];
    let want: &[(usize, &str)] = &[
        (1, "'=HYPERLINK(\"http://evil.example/x\",\"x\")"),
        (2, "'+cmd"),
        (6, "'@SUM(1+1)"),
        (8, "'-2+3"),
    ];
    for &(i, prefix) in want {
        assert_eq!(&row[i], prefix, "col {i}");
    }
    assert!(!row[3].starts_with('\''));
}

/// Minimal CSV reader (handles quoted fields with doubled quotes) for the test.
fn parse_csv_rows(s: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => quoted = true,
                ',' => {
                    row.push(std::mem::take(&mut field));
                }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                '\r' => {}
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[test]
fn filename_sanitization() {
    let f = DBFinding {
        id: 123,
        name: "SQL/injection: test".to_string(),
        severity: "critical".to_string(),
        ..DBFinding::default()
    };
    let name = finding_filename(&f);
    assert!(name.starts_with("critical_SQL_injection_test"));
    assert!(name.ends_with(".md"));
    assert!(!name.contains('/'));
}
