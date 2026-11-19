//! `noa-rs` — behavioral-parity Rust port of the *pure, value-typed* helpers of the
//! external `github.com/Autumn-27/norma/noa` package.
//!
//! `noa` is norma's context-compression engine. This crate ports the small,
//! self-contained codec + formatting + token-estimation helpers that are pure and
//! value-typed; the stateful bulk (compression pipeline, prune, truncate, archive,
//! nudges, panel rendering over `CompressionState`) stays out of scope here.
//!
//! The whole package imports only the standard library, so the parity oracle is a
//! small standalone Go module (no module-cache / `llm-rs` entanglement). The Go
//! `regexp` uses in these helpers are reproduced by hand (no `regex` crate offline):
//! the patterns are tiny and fully specified by the recorded ground truth.

#![forbid(unsafe_code)]

// `noa` ref / block-id constants (const.go).
/// Zero-padded width of a message ref: `m00001` .. `m99999`.
pub const REF_WIDTH: usize = 5;
/// Smallest valid ref index.
pub const MIN_REF_INDEX: i64 = 1;
/// Largest valid ref index.
pub const MAX_REF_INDEX: i64 = 99999;
/// Marks a hard-protected message: it gets a `BLOCKED` entry and no number.
pub const BLOCKED_REF: &str = "BLOCKED";
/// `SummaryIDPrefix + blockID` is the synthetic id of a rendered summary.
pub const SUMMARY_ID_PREFIX: &str = "noa_summary_";

/// `noa.FormatTokens` — render a token count compactly: exact below 1000, one
/// decimal `…K` below 10000, whole `…K` (round-half-up) at/above 10000.
pub fn format_tokens(n: i64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 10000 {
        return format!("{:.1}K", n as f64 / 1000.0);
    }
    format!("{}K", round_half_up(n as f64 / 1000.0))
}

/// `noa.roundHalfUp` — JavaScript `Math.round` semantics for the non-negative
/// values used here (half rounds up; agrees with away-from-zero on positives).
fn round_half_up(f: f64) -> i64 {
    (f + 0.5).floor() as i64
}

/// `noa.IndexToRef` — render a 1-based index as `mNNNNN` (`m%05d`). Out-of-range
/// input is a programming error and yields `""`.
pub fn index_to_ref(index: i64) -> String {
    if !(MIN_REF_INDEX..=MAX_REF_INDEX).contains(&index) {
        return String::new();
    }
    format!("m{index:0REF_WIDTH$}")
}

/// `noa.RefToIndex` — parse `mNNNNN` back to its index, tolerating surrounding
/// space, upper case, and missing zero padding (`m5` == `m00005`). `ok=false` for
/// anything else (bad prefix, out-of-range, non-digit, empty digits).
///
/// Mirrors Go's `^m0*(\d{1,5})$` without a regex crate: lowercase, trim, require a
/// leading `m`, then 1..=5 digits that may have leading zeros but whose value must
/// land in `[MIN_REF_INDEX, MAX_REF_INDEX]`.
pub fn ref_to_index(r: &str) -> (i64, bool) {
    let s = r.trim().to_lowercase();
    let Some(rest) = s.strip_prefix('m') else {
        return (0, false);
    };
    if rest.is_empty() || rest.len() > 5 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return (0, false);
    }
    // Go regex `0*(\d{1,5})`: the value is the full digit run parsed (leading zeros
    // are tolerated, so "m00005" and "m5" both yield 5). `strconv.Atoi` of all digits.
    let n: i64 = rest.parse().unwrap_or(0);
    if !(MIN_REF_INDEX..=MAX_REF_INDEX).contains(&n) {
        return (0, false);
    }
    (n, true)
}

/// `noa.ParseBlockID` — accept `b5`, `b005`, or bare `5`, plus a trailing tier
/// label as displayed (`b5(T2)` / `(t2)`). Returns the canonical `bN`.
///
/// Mirrors Go: lowercase + trim, strip a trailing `(t<digits>)`, then match
/// `^b0*(\d{1,9})$` or a bare integer `>= 1`.
pub fn parse_block_id(s: &str) -> (String, bool) {
    let lowered = s.trim().to_lowercase();
    // Strip a trailing "(t<digits>)" tier suffix.
    let stripped = strip_tier_suffix(&lowered);
    if let Some(num) = match_block_id(&stripped) {
        if num >= 1 {
            return (format!("b{num}"), true);
        }
        return (String::new(), false);
    }
    // Bare integer fallback: strconv.Atoi(s) >= 1.
    if let Ok(n) = stripped.parse::<i64>()
        && n >= 1
    {
        return (format!("b{n}"), true);
    }
    (String::new(), false)
}

/// Strip a trailing `(t<digits>)` (the tier label the panel/block map renders).
fn strip_tier_suffix(s: &str) -> String {
    let Some(open) = s.rfind("(t") else {
        return s.to_string();
    };
    if !s.ends_with(')') {
        return s.to_string();
    }
    let inner = &s[open + 2..s.len() - 1];
    if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
        return s[..open].to_string();
    }
    s.to_string()
}

/// Match `^b0*(\d{1,9})$` and return the parsed integer (leading zeros tolerated).
fn match_block_id(s: &str) -> Option<i64> {
    let rest = s.strip_prefix('b')?;
    if rest.is_empty() || rest.len() > 9 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// `noa.SummaryMessageID` — the synthetic id of a block's rendered summary.
pub fn summary_message_id(block_id: &str) -> String {
    format!("{SUMMARY_ID_PREFIX}{block_id}")
}

/// `noa.BlockIDFromSummaryID` — inverse of `summary_message_id`; `None` if `s` is
/// not a summary id.
pub fn block_id_from_summary_id(s: &str) -> (String, bool) {
    match s.strip_prefix(SUMMARY_ID_PREFIX) {
        Some(rest) => (rest.to_string(), true),
        None => (String::new(), false),
    }
}

/// `noa.DefaultCountTokens` — approximate tokens as one per CJK rune plus one per
/// four other runes (counted by Unicode scalar, so astral chars count once).
pub fn default_count_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let mut cjk = 0i64;
    let mut other = 0i64;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + ceil_div(other, 4)
}

/// `noa.EstimateTokensFast` — rough figure: ceil(rune-count / 4).
pub fn estimate_tokens_fast(text: &str) -> i64 {
    ceil_div(text.chars().count() as i64, 4)
}

/// `noa.isCJK` — CJK Unified Ideographs, Hiragana+Katakana, Hangul Syllables.
fn is_cjk(r: char) -> bool {
    let c = r as u32;
    (0x4E00..=0x9FFF).contains(&c)        // CJK Unified Ideographs
        || (0x3040..=0x30FF).contains(&c) // Hiragana + Katakana
        || (0xAC00..=0xD7AF).contains(&c) // Hangul Syllables
}

/// `noa.ceilDiv` — ceiling division; non-positive numerator yields 0.
fn ceil_div(n: i64, d: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    (n + d - 1) / d
}
