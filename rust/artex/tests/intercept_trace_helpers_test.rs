//! Differential tests for pure `intercept/trace.go` helpers — oracle values
//! captured live from Go.

use artex::intercept::trace_helpers::{bounded, digest_input};

#[test]
fn digest_input_oracle() {
    let cases: &[(&str, &str)] = &[
        (
            r#"{"b":2,"a":1}"#,
            "43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777",
        ),
        (
            r#"{"x":1.0,"y":"s"}"#,
            "9b617978dfba1ecdbbfc0012b802e9e3c764369d758b18742e74d196b355c0db",
        ),
        (
            "not-json",
            "0c21a879c732a67910d80988df4919d794f6a070aab610ef865032a28046b021",
        ),
        (
            r#"{"n":null}"#,
            "5b4da02351c1c20974b216a5e3a4edb59ac51cbda6be3e9d82952b4f5beea463",
        ),
        (
            "[]",
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(&digest_input(input.as_bytes()), want, "digest of {input:?}");
    }
}

#[test]
fn bounded_oracle() {
    // (input, limit, expected text, expected cut)
    let cases: &[(&str, usize, &str, bool)] = &[
        ("", 0, "", false),
        ("abc", 10, "abc", false),
        ("hello", 3, "hel", true),
        ("héllo", 3, "hé", true),  // 3 bytes: h(1) + é(2) — no half-rune
        ("héllo", 4, "hél", true), // 4 bytes: h + é + l
    ];
    for (s, n, want, cut) in cases {
        let (got, got_cut) = bounded(s, *n);
        assert_eq!(
            (got.as_str(), got_cut),
            (*want, *cut),
            "bounded({s:?}, {n})"
        );
    }
}
