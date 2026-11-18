//! `enrich-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `enrich/` package (`id0`, `itoa`, `hostOf`).
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions: `record.sh` runs a temporary oracle over
//! a corpus and writes `cases.jsonl`; `tests/parity.rs` replays it and asserts
//! byte-identical output. The Go original is untouched.

#![forbid(unsafe_code)]

use gocompat::neturl::parse_url;

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
