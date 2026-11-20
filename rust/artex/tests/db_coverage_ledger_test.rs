//! Differential tests for the pure coverage-ledger logic — port of the DB-free
//! tests in `db/coverage_ledger_test.go` (`TestBuildCoverageLedgerGaps`,
//! `TestCoverageCompletenessFor`, `TestNormalizeCoverageEntry`, and the
//! JSON-shape check from `TestCoverageJSONCarriesLedger`). The PG tests in that
//! file need the `AssetStore`/`tasks` layer and land with the assets group.
//!
//! These are pure functions: no Postgres, no shared lock.

use artex::db::coverage_ledger::{
    build_coverage_ledger, coverage_completeness_for, normalize_coverage_entry,
    CoverageCompleteness, CoverageEntry, OUTCOME_NEEDS_FOLLOW_UP, OUTCOME_NOT_APPLICABLE,
    OUTCOME_NO_ISSUE_FOUND, OUTCOME_REPORTED, OUTCOME_RULED_OUT,
};

fn e(surface: &str, risk: &str, outcome: &str, evidence: &str, reason: &str) -> CoverageEntry {
    CoverageEntry {
        surface: surface.to_string(),
        risk_area: risk.to_string(),
        outcome: outcome.to_string(),
        evidence: evidence.to_string(),
        reason: reason.to_string(),
        ..Default::default()
    }
}

/// One `BuildCoverageLedger` table row: (name, entries, mandatory, want gaps as
/// `CoverageGap.String()`, want unexamined).
type GapCase<'a> = (
    &'a str,
    Vec<CoverageEntry>,
    Vec<&'a str>,
    Vec<&'a str>,
    Vec<&'a str>,
);

#[test]
fn build_coverage_ledger_gaps() {
    let cases: Vec<GapCase> = vec![
        (
            "mandatory class with no entry is unexamined, never clean",
            vec![],
            vec!["SQLi"],
            vec!["unrecorded_risk_class sqli"],
            vec!["sqli"],
        ),
        (
            "needs_follow_up is a gap",
            vec![e("/login", "idor", OUTCOME_NEEDS_FOLLOW_UP, "", "")],
            vec![],
            vec!["needs_follow_up /login idor"],
            vec![],
        ),
        (
            "no_issue_found without evidence is a gap",
            vec![e("/q", "sqli", OUTCOME_NO_ISSUE_FOUND, " ", "")],
            vec!["sqli"],
            vec!["missing_evidence /q sqli"],
            vec![],
        ),
        (
            "no_issue_found with evidence closes the class",
            vec![e("/q", "sqli", OUTCOME_NO_ISSUE_FOUND, "fact 12", "")],
            vec!["SQLi "],
            vec![],
            vec![],
        ),
        (
            "ruled_out without reason is a gap",
            vec![e("/a", "xss", OUTCOME_RULED_OUT, "", "")],
            vec![],
            vec!["missing_reason /a xss"],
            vec![],
        ),
        (
            "not_applicable without reason is a gap",
            vec![e("/a", "ssrf", OUTCOME_NOT_APPLICABLE, "", "")],
            vec![],
            vec!["missing_reason /a ssrf"],
            vec![],
        ),
        (
            "all three sources, unrecorded first",
            vec![
                e("/b", "xss", OUTCOME_NEEDS_FOLLOW_UP, "", ""),
                e("/c", "rce", OUTCOME_NO_ISSUE_FOUND, "", ""),
            ],
            vec!["xss", "idor"],
            vec![
                "unrecorded_risk_class idor",
                "needs_follow_up /b xss",
                "missing_evidence /c rce",
            ],
            vec!["idor"],
        ),
        (
            "no goals and no entries: no obligation, no gap",
            vec![],
            vec![],
            vec![],
            vec![],
        ),
        (
            "reported closes the class",
            vec![e("/r", "sqli", OUTCOME_REPORTED, "", "")],
            vec!["sqli"],
            vec![],
            vec![],
        ),
    ];

    for (name, entries, mandatory, want, unexamined) in cases {
        let l = build_coverage_ledger(
            entries,
            mandatory.into_iter().map(str::to_string).collect(),
            CoverageCompleteness::default(),
        );
        let got: Vec<String> = l.gaps.iter().map(|g| g.to_string()).collect();
        let want: Vec<String> = want.into_iter().map(str::to_string).collect();
        let want_unexamined: Vec<String> = unexamined.into_iter().map(str::to_string).collect();
        assert_eq!(got, want, "gaps mismatch in case: {name}");
        assert_eq!(
            l.unexamined, want_unexamined,
            "unexamined mismatch in case: {name}"
        );
    }
}

#[test]
fn coverage_completeness_for_states() {
    // (status, paused, budget, past, want state)
    let cases = [
        ("done", false, false, false, "complete"),
        ("timeout", false, false, true, "timeout"),
        ("done", false, false, true, "timeout"), // goals met on the final timeout round
        ("timeout", false, true, false, "budget"),
        ("done", false, true, false, "budget"), // budget wrap-up that met the goals
        ("failed", false, false, false, "aborted"),
        ("running", true, false, false, "paused"),
        ("running", false, false, false, "in_progress"),
    ];
    for (status, paused, budget, past, want_state) in cases {
        let got = coverage_completeness_for(status, paused, budget, past);
        assert_eq!(
            got.state, want_state,
            "state for ({status},{paused},{budget},{past})"
        );
        assert_eq!(
            got.complete,
            want_state == "complete",
            "complete flag for {want_state}"
        );
        assert!(!got.caveat.is_empty(), "caveat always set");
    }
}

#[test]
fn coverage_json_carries_ledger() {
    let l = build_coverage_ledger(
        vec![],
        vec!["sqli".to_string()],
        coverage_completeness_for("timeout", false, false, true),
    );
    let v = serde_json::to_value(&l).expect("serialize ledger");
    let obj = v.as_object().expect("ledger is a JSON object");
    for k in [
        "entries",
        "gaps",
        "unexamined",
        "completeness",
        "findings_by_class",
    ] {
        assert!(obj.contains_key(k), "missing {k} in {v}");
    }
    assert!(
        v["gaps"].to_string().contains("unrecorded_risk_class"),
        "gaps={}",
        v["gaps"]
    );
}

#[test]
fn normalize_coverage_entry_rules() {
    // Fold a messy entry to one line, lower-case risk_area.
    let normalized = normalize_coverage_entry(CoverageEntry {
        surface: " /a\r\n\n\t[[x]]\u{0}\u{7f}\u{85} b  ".to_string(),
        risk_area: " SQLi\n".to_string(),
        outcome: OUTCOME_NO_ISSUE_FOUND.to_string(),
        evidence: "fact\n\n12".to_string(),
        reason: "\tr\u{1b}[31m".to_string(),
        ..Default::default()
    })
    .expect("normalize");
    assert_eq!(normalized.surface, "/a [[x]] b");
    assert_eq!(normalized.risk_area, "sqli");
    assert_eq!(normalized.evidence, "fact 12");
    assert_eq!(normalized.reason, "r [31m");

    // Length limits: at the limit is accepted, one over is refused by name.
    let ok = e("/s", "xss", OUTCOME_REPORTED, "", "");
    type LimitCase = (&'static str, fn(&mut CoverageEntry, usize), usize);
    let limits: Vec<LimitCase> = vec![
        ("surface", |e, n| e.surface = "s".repeat(n), 200),
        ("risk_area", |e, n| e.risk_area = "r".repeat(n), 64),
        ("evidence", |e, n| e.evidence = "é".repeat(n), 500),
        ("reason", |e, n| e.reason = "x".repeat(n), 500),
    ];
    for (name, edit, max) in limits {
        let mut at = ok.clone();
        edit(&mut at, max);
        assert!(
            normalize_coverage_entry(at).is_ok(),
            "{name} at the limit must be accepted"
        );
        let mut over = ok.clone();
        edit(&mut over, max + 1);
        let err = normalize_coverage_entry(over).expect_err("over limit refused");
        assert_eq!(
            err.to_string(),
            format!("{name} too long (max {max})"),
            "{name} over-limit message"
        );
    }

    // Blank surface is refused.
    assert!(
        normalize_coverage_entry(e("\n\t", "xss", OUTCOME_REPORTED, "", "")).is_err(),
        "blank surface accepted"
    );
    // Invalid outcome is refused.
    assert!(
        normalize_coverage_entry(e("/s", "xss", "clean", "", "")).is_err(),
        "invalid outcome accepted"
    );
}
