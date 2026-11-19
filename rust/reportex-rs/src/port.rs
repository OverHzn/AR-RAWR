//! Rust port of the unexported, pure string helpers in the Go `report/` package.
//!
//! All seven are deterministic and value-typed (strings, ints, bools); none touch
//! the network, regex, time, or the filesystem. Go-stdlib string semantics come
//! from `gocompat::strings` so edge cases (Unicode whitespace, tab/space trimming)
//! match byte-for-byte.

use gocompat::strings;

/// `nz` (`report/report.go:136`): return the default `d` when `s` is blank (only
/// whitespace), otherwise `s` unchanged.
pub fn nz(s: &str, d: &str) -> String {
    if strings::go_trim_space(s).is_empty() {
        d.to_string()
    } else {
        s.to_string()
    }
}

/// `oneLine` (`report/quality.go:247`): collapse every run of whitespace to a
/// single space and trim both ends — `strings.Join(strings.Fields(s), " ")`.
pub fn one_line(s: &str) -> String {
    strings::go_fields_join(s)
}

/// `unbalancedFence` (`report/quality.go:216`): true when an odd number of lines
/// begin (after leading spaces/tabs) with a triple-backtick fence.
pub fn unbalanced_fence(s: &str) -> bool {
    let mut fences = 0u32;
    for line in s.split('\n') {
        if strings::go_trim_left(line, " \t").starts_with("```") {
            fences += 1;
        }
    }
    fences % 2 == 1
}

/// `staleReportNotice` (`report/quality.go:161`): a fixed advisory paragraph when
/// the report is stale, otherwise the empty string.
pub fn stale_report_notice(stale: bool) -> String {
    if stale {
        "The evidence has changed; the detailed report needs updating.\n\n".to_string()
    } else {
        String::new()
    }
}

/// `reportStale` (`report/quality.go:54`): a non-empty report is stale when its
/// recorded evidence version differs from the current one.
pub fn report_stale(report: &str, evidence_version: i64, report_evidence_version: i64) -> bool {
    !report.is_empty() && evidence_version != report_evidence_version
}

/// `verificationLabel` (`report/quality.go:230`): human label for a verification
/// status (the `db.Verification*` string constants). Empty and `unverified` both
/// map to "Unverified"; unknown values pass through unchanged.
pub fn verification_label(s: &str) -> String {
    match s {
        "" | "unverified" => "Unverified",
        "verified" => "Verified",
        "blocked" => "Blocked",
        "not_assessed" => "Not assessed",
        "false_positive" => "False positive",
        _ => return s.to_string(),
    }
    .to_string()
}

/// `detailedReportLimit` (`report/quality.go:188`): render `body` under `heading`,
/// truncated to `limit` bytes when `limit > 0` and the body is longer. The cut is
/// moved back to a UTF-8 boundary, an open ``` fence is closed, and a KiB note is
/// appended; otherwise the full body is returned.
pub fn detailed_report_limit(body: &str, heading: &str, limit: i64) -> String {
    let s = strings::go_trim_space(body);
    if s.is_empty() {
        return String::new();
    }
    if limit > 0 && (s.len() as i64) > limit {
        let limit = limit as usize;
        let bytes = s.as_bytes();
        // Never split a rune: shrink the byte cut until it is valid UTF-8.
        let mut end = limit;
        while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
            end -= 1;
        }
        let mut cut =
            strings::go_trim_space(std::str::from_utf8(&bytes[..end]).unwrap_or("")).to_string();
        if unbalanced_fence(&cut) {
            cut.push_str("\n```");
        }
        let kib = limit >> 10;
        return format!(
            "{heading}\n\n{cut}\n\n_Detailed report truncated at {kib} KiB; read the full text from the finding record._\n\n"
        );
    }
    format!("{heading}\n\n{s}\n\n")
}

// ---------------------------------------------------------------------------
// Batch 9v: pure render helpers over the value-typed `Quality` record
// (`report/quality.go`). `qualityBullets` / `qualityNarrative` format a finding's
// quality fields into the report Markdown. They depend on `one_line` and
// `verification_label` (already ported above).
// ---------------------------------------------------------------------------

/// `report.Quality`: the value-typed quality record a finding node carries. The
/// `Missing` / `Unavailable` flags are three distinct states (see the Go source).
pub struct Quality {
    pub poe: i64,
    pub verification: String,
    pub verification_note: String,
    pub confidence_rationale: String,
    pub counterevidence: String,
    pub traffic_count: i64,
    pub record_id: i64,
    pub missing: bool,
    pub unavailable: bool,
}

/// `qualityBullets` (`report/quality.go`): render the quality bullets for a
/// finding. `always_proof` is true for the task report (no traffic section of its
/// own); the findings export passes false so a bound count is not repeated.
pub fn quality_bullets(q: &Quality, always_proof: bool) -> String {
    let mut b = String::new();
    // A failed lookup is not an absence: say the fields could not be read.
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
        b.push_str(&format!("- **Verification note**: {}\n", note));
    }
    b.push('\n');
    b
}

/// `qualityNarrative` (`report/quality.go`): render the confidence rationale and
/// counterevidence blocks (empty for a missing/unavailable record).
pub fn quality_narrative(q: &Quality) -> String {
    if q.missing || q.unavailable {
        return String::new();
    }
    let mut b = String::new();
    let s = strings::go_trim_space(&q.confidence_rationale);
    if !s.is_empty() {
        b.push_str(&format!("**Confidence rationale**:\n\n{}\n\n", s));
    }
    let s = strings::go_trim_space(&q.counterevidence);
    if !s.is_empty() {
        b.push_str(&format!("**Counterevidence**:\n\n{}\n\n", s));
    }
    b
}

// ---------------------------------------------------------------------------
// Batch 9x: `detailedReport` / `detailedReportFull` (`report/report.go`) — thin
// wrappers over `detailedReportLimit` with a fixed / no cap.
// ---------------------------------------------------------------------------

/// `reportMaxReportBytes` (`report/quality.go`): the default detailed-report cap,
/// `8 << 10` bytes (8 KiB).
pub const REPORT_MAX_REPORT_BYTES: i64 = 8 << 10;

/// `detailedReport` (`report/report.go`): `detailedReportLimit` with the default
/// 8 KiB cap.
pub fn detailed_report(body: &str, heading: &str) -> String {
    detailed_report_limit(body, heading, REPORT_MAX_REPORT_BYTES)
}

/// `detailedReportFull` (`report/report.go`): `detailedReportLimit` with no cap
/// (`limit == 0` renders the whole body).
pub fn detailed_report_full(body: &str, heading: &str) -> String {
    detailed_report_limit(body, heading, 0)
}

// ---------------------------------------------------------------------------
// Batch 9aq: `csvCell` (`report/findings.go`) — prefix cells that spreadsheet
// apps treat as formulas. Pure string predicate, value-typed.
// ---------------------------------------------------------------------------

/// `csvCell` (`report/findings.go`): prefix a cell with a single quote when it
/// begins with `=`, `+`, `-`, `@`, tab, or CR, so Excel/LibreOffice do not
/// evaluate it as a formula on open. The empty string is returned unchanged.
pub fn csv_cell(s: &str) -> String {
    match s.as_bytes().first() {
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r') => format!("'{s}"),
        _ => s.to_string(),
    }
}
