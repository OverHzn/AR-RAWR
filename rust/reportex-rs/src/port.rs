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
