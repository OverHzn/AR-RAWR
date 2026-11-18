//! `traffic-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `traffic/` package (`sanitize`, `ftsQuote`, `clip`,
//! `boolInt`, `validHeaderName`, `uniqueArchiveHosts`, `clipBytes`,
//! `truncateUTF8`, `isBinaryBody`, `binaryTag`, `appliedRules`).
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

use gocompat::strings::{go_to_lower, go_trim_space};

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

/// `uniqueArchiveHosts` (traffic/archive.go): lowercase + trim each host, drop
/// empties and dups, return the survivors sorted (`sort.Strings`).
pub fn unique_archive_hosts(hosts: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        let trimmed = go_trim_space(h);
        let host = go_to_lower(trimmed);
        if host.is_empty() {
            continue;
        }
        if seen.insert(host.clone()) {
            out.push(host);
        }
    }
    out.sort();
    out
}

/// `binaryTypes` (traffic/traffic.go): content-type prefixes treated as binary.
const BINARY_TYPES: &[&str] = &[
    "image/",
    "audio/",
    "video/",
    "font/",
    "application/octet-stream",
    "application/zip",
    "application/gzip",
    "application/x-gzip",
    "application/x-tar",
    "application/x-7z-compressed",
    "application/x-rar",
    "application/pdf",
    "application/x-msdownload",
    "application/vnd.android.package-archive",
    "application/java-archive",
    "application/wasm",
    "application/x-shockwave-flash",
];

/// `clipBytes` (traffic/traffic.go): `b[:n]` (byte slice); returns `b` whole when
/// `len(b) <= n`. Negative `n` is outside the recorded corpus.
pub fn clip_bytes(b: &[u8], n: i64) -> Vec<u8> {
    let n = n.max(0) as usize;
    if b.len() <= n {
        return b.to_vec();
    }
    b[..n].to_vec()
}

/// `truncateUTF8` (traffic/traffic.go): cut to `n` bytes, then back off trailing
/// bytes of an incomplete rune at the cut (Go's `utf8.DecodeLastRune` loop). The
/// recorded corpus uses valid UTF-8 bodies, where `from_utf8`'s `valid_up_to`
/// marks exactly that boundary.
pub fn truncate_utf8(b: &[u8], n: i64) -> String {
    let n = n.max(0) as usize;
    if b.len() <= n {
        return String::from_utf8_lossy(b).into_owned();
    }
    let mut end = n.min(b.len());
    if let Err(e) = std::str::from_utf8(&b[..end]) {
        // Only an incomplete trailing rune is backed off; interior invalid bytes
        // (error_len() == Some) are outside the recorded corpus.
        if e.error_len().is_none() {
            end = e.valid_up_to();
        }
    }
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// `isBinaryBody` (traffic/traffic.go): a `binaryTypes` content-type prefix, or a
/// NUL byte in the first 512 bytes, means binary.
pub fn is_binary_body(content_type: &str, body: &[u8]) -> bool {
    let trimmed = go_trim_space(content_type);
    let mut ct = go_to_lower(trimmed);
    if let Some(i) = ct.find(';') {
        ct = go_trim_space(&ct[..i]).to_string();
    }
    BINARY_TYPES.iter().any(|p| ct.starts_with(p)) || clip_bytes(body, 512).contains(&0)
}

/// `binaryTag` (traffic/traffic.go): `[binary <ct>, <n> bytes, magic=<hex>]`,
/// defaulting an empty content type to `application/octet-stream`.
pub fn binary_tag(content_type: &str, body: &[u8]) -> String {
    let trimmed = go_trim_space(content_type);
    let ct = if trimmed.is_empty() {
        "application/octet-stream".to_string()
    } else {
        trimmed.to_string()
    };
    let magic = hex_encode(&clip_bytes(body, 4));
    format!("[binary {ct}, {} bytes, magic={magic}]", body.len())
}

/// Go `hex.EncodeToString`: lowercase, two hex digits per byte.
fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &byte in b {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}

/// One `rewrite.TraceLine` reduced to the fields `appliedRules` reads (`RuleID`,
/// `Outcome`); the rest of the struct does not affect it.
pub struct TraceLine {
    pub rule_id: i64,
    pub outcome: String,
}

/// `appliedRules` (traffic/traffic.go): whether any line has `Outcome ==
/// rewrite.OutcomeApplied` ("applied"), and the ordered, de-duplicated non-zero
/// `RuleID`s of those lines. Returns `(false, [])` when none apply.
pub fn applied_rules(lines: &[TraceLine]) -> (bool, Vec<i64>) {
    let mut rewritten = false;
    let mut ids: Vec<i64> = Vec::new();
    for l in lines {
        if l.outcome != "applied" {
            continue;
        }
        rewritten = true;
        if l.rule_id != 0 && !ids.contains(&l.rule_id) {
            ids.push(l.rule_id);
        }
    }
    (rewritten, ids)
}
