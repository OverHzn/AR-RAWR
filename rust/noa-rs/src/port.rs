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

use serde::{Deserialize, Serialize};

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

// ---------------------------------------------------------------------------
// Batch 9b: archive + panel + config helpers.
// ---------------------------------------------------------------------------

/// `noa.ArchiveFileName` — `<blockID>_<startRef>-<endRef>.md`.
pub fn archive_file_name(block_id: &str, start_ref: &str, end_ref: &str) -> String {
    format!("{block_id}_{start_ref}-{end_ref}.md")
}

/// `noa.ArchiveTierDir` — the subdirectory a tier's archives live in: `tier<N>`.
pub fn archive_tier_dir(tier: i64) -> String {
    format!("tier{tier}")
}

/// `noa.PanelBlockCount` — count the `\bb\d+\(T\d+\)=` occurrences in a rendered
/// panel. The Go pattern's `\b` word boundary, case-sensitivity, and exact spacing
/// (no space before `(` or before `=`) are reproduced by hand: a block line is a
/// lowercase `b` at a word boundary, digits, `(T`, digits, `)=`.
pub fn panel_block_count(panel: &str) -> i64 {
    let b = panel.as_bytes();
    let mut count = 0i64;
    let mut i = 0usize;
    while i < b.len() {
        // A match starts at a 'b' that begins a word (previous byte is not a
        // word char, or it is the start of the string).
        if b[i] == b'b' && (i == 0 || !is_word_byte(b[i - 1])) {
            let mut j = i + 1;
            // \d+ (at least one digit)
            let digits_start = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start && j + 1 < b.len() && b[j] == b'(' && b[j + 1] == b'T' {
                let mut k = j + 2;
                let t_digits = k;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                if k > t_digits && k + 1 < b.len() && b[k] == b')' && b[k + 1] == b'=' {
                    count += 1;
                    i = k + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    count
}

/// A byte is a "word" byte for the regex `\b` boundary (`[0-9A-Za-z_]`).
fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `noa.TierConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TierConfig {
    pub enabled: bool,
    pub max_tier: i64,
    pub tier2_trigger: i64,
    pub tier3_trigger: i64,
}

/// `noa.NudgeConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NudgeConfig {
    pub max_context_limit_pct: f64,
    pub min_context_limit_pct: f64,
    pub emergency_threshold_pct: f64,
    pub growth_ratio: f64,
    pub growth_floor: i64,
    pub growth_cap: i64,
    pub min_growth_floor: i64,
    pub min_growth_ratio: f64,
    pub tier2_growth_multiplier: f64,
    pub min_pressure_benefit_tokens: Option<i64>,
}

/// `noa.TruncateConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TruncateConfig {
    pub threshold: f64,
}

/// `noa.CompressConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CompressConfig {
    pub min_compress_range: i64,
    pub min_summary_length: i64,
    pub max_summary_length: i64,
}

/// `noa.Config` — value-typed fields only. The Go struct also carries
/// `IsToolProtected func(string) bool`, which is not value-typed and is never read
/// by `ValidateConfig`, so it is omitted here (default: no host predicate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Config {
    pub model_context_limit: i64,
    pub tiers: TierConfig,
    pub nudge: NudgeConfig,
    pub truncate: TruncateConfig,
    pub compress: CompressConfig,
    pub protected_tools: Option<Vec<String>>,
    pub preserve_recent_messages: i64,
    pub preserve_recent_tokens: i64,
    pub max_compress_attempts: i64,
}

/// `noa.DefaultConfig` — the tuned defaults for a given context window.
pub fn default_config(model_context_limit: i64) -> Config {
    Config {
        model_context_limit,
        tiers: TierConfig {
            enabled: true,
            max_tier: 3,
            tier2_trigger: 5,
            tier3_trigger: 10,
        },
        nudge: NudgeConfig {
            max_context_limit_pct: 0.75,
            min_context_limit_pct: 0.45,
            emergency_threshold_pct: 0.95,
            growth_ratio: 0.05,
            growth_floor: 50_000,
            growth_cap: 50_000,
            min_growth_floor: 20_000,
            min_growth_ratio: 0.45,
            tier2_growth_multiplier: 1.5,
            min_pressure_benefit_tokens: None,
        },
        truncate: TruncateConfig { threshold: 0.95 },
        compress: CompressConfig {
            min_compress_range: 5_000,
            min_summary_length: 50,
            max_summary_length: 20_000,
        },
        protected_tools: None,
        preserve_recent_messages: 5,
        preserve_recent_tokens: 5_000,
        max_compress_attempts: 3,
    }
}

/// Format an `f64` the way Go's `%g` verb does for the values `ValidateConfig`
/// prints (shortest round-tripping representation). For the integral-valued floats
/// in the corpus (`0.0`, `2.0`) it yields `0` / `2`; otherwise the minimal decimal.
fn format_g(v: f64) -> String {
    // Go's %g uses the shortest representation that round-trips. Rust's `{}` on f64
    // also produces the shortest round-tripping form, but it never drops a trailing
    // `.0` for integral values the way Go's %g does. Mirror Go: drop a bare `.0`.
    let s = format!("{v}");
    if let Some(stripped) = s.strip_suffix(".0") {
        // Only strip when the fractional part is exactly ".0" (e.g. "2.0" -> "2",
        // "0.0" -> "0"), leaving "0.95" and "1.5" untouched.
        stripped.to_string()
    } else {
        s
    }
}

/// `noa.ValidateConfig` — return warnings (never errors) for a questionable knob.
pub fn validate_config(c: &Config) -> Vec<String> {
    let mut w: Vec<String> = Vec::new();

    if c.model_context_limit <= 0 {
        w.push(format!(
            "ModelContextLimit must be positive, got {}",
            c.model_context_limit
        ));
    }
    if c.nudge.min_context_limit_pct > c.nudge.max_context_limit_pct {
        w.push(format!(
            "Nudge.MinContextLimitPct ({}) must not exceed MaxContextLimitPct ({})",
            format_g(c.nudge.min_context_limit_pct),
            format_g(c.nudge.max_context_limit_pct)
        ));
    }
    if c.nudge.max_context_limit_pct > c.nudge.emergency_threshold_pct {
        w.push(format!(
            "Nudge.MaxContextLimitPct ({}) must not exceed EmergencyThresholdPct ({})",
            format_g(c.nudge.max_context_limit_pct),
            format_g(c.nudge.emergency_threshold_pct)
        ));
    }
    if c.truncate.threshold <= 0.0 || c.truncate.threshold > 1.0 {
        w.push(format!(
            "Truncate.Threshold must be in (0, 1], got {}",
            format_g(c.truncate.threshold)
        ));
    }
    if c.tiers.tier2_trigger < 1 {
        w.push(format!(
            "Tiers.Tier2Trigger must be >= 1, got {}",
            c.tiers.tier2_trigger
        ));
    }
    if c.tiers.tier3_trigger <= c.tiers.tier2_trigger {
        w.push(format!(
            "Tiers.Tier3Trigger ({}) must exceed Tier2Trigger ({})",
            c.tiers.tier3_trigger, c.tiers.tier2_trigger
        ));
    }
    if c.tiers.max_tier != 2 && c.tiers.max_tier != 3 {
        w.push(format!(
            "Tiers.MaxTier must be 2 or 3, got {} — the tier rule prompts only cover up to 3",
            c.tiers.max_tier
        ));
    }
    if c.compress.min_summary_length >= c.compress.max_summary_length {
        w.push(format!(
            "Compress.MinSummaryLength ({}) must be below MaxSummaryLength ({})",
            c.compress.min_summary_length, c.compress.max_summary_length
        ));
    }
    if let Some(v) = c.nudge.min_pressure_benefit_tokens
        && v < 0
    {
        w.push(format!(
            "Nudge.MinPressureBenefitTokens must be >= 0, got {}",
            v
        ));
    }
    if c.max_compress_attempts < 1 {
        w.push(format!(
            "MaxCompressAttempts must be >= 1, got {}",
            c.max_compress_attempts
        ));
    }
    w
}
