//! Differential tests for `db/exploration.go:jsonbClean` — port of
//! `TestJsonbClean` plus backslash-parity edge cases.

use artex::db::jsonb_clean::jsonb_clean;

#[test]
fn jsonb_clean_oracle() {
    let marshaled = serde_json::to_string(&serde_json::json!({"body": "ab\u{0}cd"})).unwrap();
    assert!(marshaled.contains(r"\u0000"));
    let cleaned = jsonb_clean(marshaled.as_bytes());
    let text = String::from_utf8(cleaned).unwrap();
    assert!(!text.contains(r"\u0000"), "NUL escape left: {text}");
    let out: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(out["body"], "abcd");

    let lit = br#"{"body":"\\u0000"}"#;
    assert_eq!(
        jsonb_clean(lit),
        lit.to_vec(),
        "literal \\u0000 must be untouched"
    );

    let plain = br#"{"body":"hello"}"#;
    assert_eq!(jsonb_clean(plain), plain.to_vec());
}

#[test]
fn jsonb_clean_backslash_parity() {
    // Three backslashes before the escape -> genuine (odd run means the last
    // backslash starts the escape); four -> literal.
    let odd = br#"{"x":"\\\u0000"}"#;
    assert!(!String::from_utf8(jsonb_clean(odd))
        .unwrap()
        .contains(r"\u0000"));
    let even = br#"{"x":"\\\\u0000"}"#;
    assert_eq!(jsonb_clean(even), even.to_vec());
}
