//! `traffic-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `traffic/` package (`sanitize`, `ftsQuote`, `clip`,
//! `boolInt`, `validHeaderName`).
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions: `record.sh` runs a temporary oracle over
//! a corpus and writes `cases.jsonl`; `tests/parity.rs` replays it and asserts
//! byte-identical output. The Go original is untouched.
//!
//! Note: `sanitize`/`clip` truncate on a **byte** boundary in Go (`out[:120]`,
//! `s[:max]`). The recorded corpus keeps every truncation on an ASCII boundary so
//! the byte slice is always valid UTF-8; the ports preserve that byte semantics.

#![forbid(unsafe_code)]

/// `sanitize` (traffic/traffic.go): map path-unsafe chars to `_`, default
/// empty/`_` to `root`, truncate to 120 bytes.
pub fn sanitize(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '?' | '*' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let out = if replaced.is_empty() || replaced == "_" {
        "root".to_string()
    } else {
        replaced
    };
    byte_truncate(&out, 120)
}

/// `ftsQuote` (traffic/traffic.go): wrap in double quotes, doubling inner quotes.
pub fn fts_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `clip` (traffic/traffic.go): if longer than `max` bytes, cut and append a note.
pub fn clip(s: &str, max: i64) -> String {
    if (s.len() as i64) <= max {
        return s.to_string();
    }
    let head = byte_truncate(s, max.max(0) as usize);
    format!(
        "{head}\n... [truncated, {} bytes total; full content in the traffic file tree] ...",
        s.len()
    )
}

/// `boolInt` (traffic/stream.go): 1 if true, else 0.
pub fn bool_int(b: bool) -> i64 {
    if b { 1 } else { 0 }
}

/// `validHeaderName` (traffic/replay.go): RFC 7230 token check (ASCII only).
pub fn valid_header_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    const TOKEN_EXTRA: &[u8] = b"!#$%&'*+-.^_`|~";
    for &c in name.as_bytes() {
        let ok = c.is_ascii_alphanumeric() || TOKEN_EXTRA.contains(&c);
        if !ok {
            return false;
        }
    }
    true
}

/// Go-style byte slice `s[:n]` at (or below) a char boundary, to avoid a panic on
/// a multi-byte cut. For the recorded (ASCII-boundary) corpus this is exact.
fn byte_truncate(s: &str, n: usize) -> String {
    let mut end = n.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}
