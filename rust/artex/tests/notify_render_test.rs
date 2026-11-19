//! Rust port of the pure `notify/render_test.go` specs. These exercise the
//! truncation / label helpers that `artex::notify` **re-exports** from the
//! oracle-parity `notify-rs` + `notifyx-rs` crates, proving the re-export surface
//! behaves identically to the real Go `notify.TruncateBytes` / `TruncateRunes` /
//! `OneLine` / `TruncateHTML` / `assetLine` / `AtLeast` / `StatusLabel`.

use artex::notify::{
    asset_line, at_least, one_line, severity_label, status_label, truncate_bytes, truncate_html,
    truncate_runes,
};

const ELLIPSIS: &str = "\u{2026}"; // "…"

/// `TestTruncateBytesKeepsValidUTF8`: the most critical invariant — a multi-byte
/// char must never be cut in half, or the platform rejects the message.
#[test]
fn truncate_bytes_keeps_valid_utf8() {
    let inputs = [
        "éééééé".to_string(),
        "mixed éé content content".to_string(),
        "aébécédé".to_string(),
        "🔴🟠🟡🔵".to_string(), // 4-byte emoji: a wrong cut is obvious
        "éé".repeat(100),
    ];
    for input in &inputs {
        for max in 1..=(input.len() as i64 + 2) {
            let got = truncate_bytes(input.clone(), max).unwrap();
            assert!(
                got.is_char_boundary(got.len()),
                "input {input:?} max={max}: invalid UTF-8 {got:?}"
            );
            assert!(
                got.len() as i64 <= max,
                "input {input:?} max={max}: {} bytes exceeds limit",
                got.len()
            );
            if (input.len() as i64) <= max {
                assert_eq!(
                    got, *input,
                    "input {input:?} max={max}: modified within limit"
                );
            }
        }
    }
}

/// `TestTruncateBytesZeroMeansUnlimited`.
#[test]
fn truncate_bytes_zero_means_unlimited() {
    let long = "x".repeat(10_000);
    assert_eq!(truncate_bytes(long.clone(), 0).unwrap(), long);
    assert_eq!(truncate_bytes(long.clone(), -5).unwrap(), long);
}

/// `TestTruncateBytesEllipsisBudget`: when max < the ellipsis, the ellipsis must
/// not push the result over the limit; the normal case carries it.
#[test]
fn truncate_bytes_ellipsis_budget() {
    let got = truncate_bytes("abcdefgh".to_string(), 1).unwrap();
    assert!(
        got.len() <= 1,
        "max=1 result {got:?} has length {}, over the limit",
        got.len()
    );
    let got = truncate_bytes("abcdefgh".to_string(), 5).unwrap();
    assert!(
        got.ends_with(ELLIPSIS),
        "expected the ellipsis, got {got:?}"
    );
}

/// `TestTruncateRunesCountsCharactersNotBytes`.
#[test]
fn truncate_runes_counts_characters_not_bytes() {
    let s = "éééééééééé";
    let got = truncate_runes(s.to_string(), 5).unwrap();
    assert_eq!(got.chars().count(), 5, "expected 5 characters, got {got:?}");
    // The byte convention comes out clearly shorter.
    let bytes = truncate_bytes(s.to_string(), 5).unwrap();
    assert!(
        bytes.chars().count() < 5,
        "the byte convention should produce fewer characters than the rune convention"
    );
}

/// `TestOneLineCollapsesWhitespace`.
#[test]
fn one_line_collapses_whitespace() {
    let got = one_line(
        "first line\n\nsecond line\twith tab   spaces".to_string(),
        0,
    )
    .unwrap();
    assert!(
        !got.contains(['\n', '\t']),
        "should collapse all whitespace, got {got:?}"
    );
    assert!(
        !got.contains("  "),
        "should not keep consecutive spaces, got {got:?}"
    );
    let got = one_line("éééééééééé".to_string(), 4).unwrap();
    assert_eq!(got.chars().count(), 4, "expected 4 characters, got {got:?}");
}

/// `TestTruncateHTMLNeverCutsTagInHalf`: truncating HTML must not leave a `<` with
/// no closing `>` in the tail.
#[test]
fn truncate_html_never_cuts_tag_in_half() {
    let s =
        "<b>Title</b>bodybodybody<a href=\"https://example.com/very/long/path\">View details</a>";
    let n = s.chars().count() as i64;
    for max in 1..=(n + 2) {
        let got = truncate_html(s.to_string(), max).unwrap();
        if max > 0 {
            assert!(
                (got.chars().count() as i64) <= max,
                "max={max}: {} chars exceeds limit",
                got.chars().count()
            );
        }
        if let Some(lt) = got.rfind('<') {
            assert!(
                got[lt..].contains('>'),
                "max={max}: tail tag was cut in half -> {got:?}"
            );
        }
    }
}

/// `TestTruncateHTMLNeverCutsEntity`: truncation must also avoid cut HTML entities
/// (`&amp` without `;`).
#[test]
fn truncate_html_never_cuts_entity() {
    let s = "aaaa&amp;bbbb&lt;cccc&quot;dddd";
    let n = s.chars().count() as i64;
    for max in 1..=(n + 2) {
        let got = truncate_html(s.to_string(), max).unwrap();
        if let Some(amp) = got.rfind('&') {
            assert!(
                got[amp..].contains(';'),
                "max={max}: entity fragment left at the tail {:?}",
                &got[amp..]
            );
        }
        assert!(
            !got.contains("&amp\0"),
            "max={max}: malformed entity appeared"
        );
    }
}

/// `TestAssetLineOmitsExcess`.
#[test]
fn asset_line_omits_excess() {
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        asset_line(&[], 3),
        "",
        "no assets should return an empty string"
    );
    assert_eq!(asset_line(&v(&["a", "b"]), 3), "a, b");
    assert_eq!(
        asset_line(&v(&["a", "b", "c", "d", "e"]), 2),
        "a, b and 3 more"
    );
    assert_eq!(
        asset_line(&v(&["a", "b", "c", "d", "e", "f", "g", "h"]), 3),
        "a, b, c and 5 more"
    );
}

/// `TestSeverityAndStatusLabels`.
#[test]
fn severity_and_status_labels() {
    assert!(
        !at_least("".to_string(), "low".to_string()).unwrap(),
        "an empty severity has ordinal 0"
    );
    assert!(
        at_least("critical".to_string(), "".to_string()).unwrap(),
        "an empty threshold lets all through"
    );
    assert_eq!(status_label("fixed".to_string()).unwrap(), "Fixed");
    assert_eq!(
        status_label("weird_status".to_string()).unwrap(),
        "weird_status"
    );
    assert_eq!(
        severity_label("critical".to_string()).unwrap(),
        "\u{1F534} Critical"
    );
    assert_eq!(severity_label("weird".to_string()).unwrap(), "weird");
}
