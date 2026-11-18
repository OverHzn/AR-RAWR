//! `enrich-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `enrich/` package (`id0`, `itoa`, `hostOf`, `uniq`, `isBlank`).
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions: `record.sh` runs a temporary oracle over
//! a corpus and writes `cases.jsonl`; `tests/parity.rs` replays it and asserts
//! byte-identical output. The Go original is untouched.

#![forbid(unsafe_code)]

use gocompat::neturl::parse_url;
use serde_json::Value;

/// `id0` (enrich/enrich.go): `id <= 0`.
pub fn id0(id: i64) -> bool {
    id <= 0
}

/// `itoa` (enrich/enrich.go): `strconv.FormatInt(n, 10)`.
pub fn itoa(n: i64) -> String {
    n.to_string()
}

/// `hostOf` (enrich/enrich.go): `url.Parse(rawURL)` then `u.Hostname()`, or ""
/// on parse error.
pub fn host_of(raw_url: &str) -> String {
    match parse_url(raw_url) {
        Ok(u) => u.hostname(),
        Err(_) => String::new(),
    }
}

/// `uniq` (enrich/enrich.go): order-preserving dedup of a string slice.
pub fn uniq(inp: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in inp {
        if seen.insert(s.clone()) {
            out.push(s.clone());
        }
    }
    out
}

/// `isBlank` (enrich/enrich.go): whether a decoded JSON value is "blank" — `null`, an
/// empty string, or a numeric zero. Go switches on the dynamic type (`nil` / `string`
/// / `float64` / `int`); JSON numbers decode to `float64`, so any numeric zero is
/// blank, and bool / array / object are not blank.
pub fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}
