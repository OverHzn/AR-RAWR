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
pub fn round_half_up(f: f64) -> i64 {
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

/// `noa.CountMessageTokens` — tokens attributed to one message's text via the
/// resolved `TokenCountFn` (a nil callback defaults to `DefaultCountTokens`).
pub fn count_message_tokens(m: &CoreMessage, count: fn(&str) -> i64) -> i64 {
    count(&m.text)
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
// Batch 9c: turn-integrity helpers over CoreMessage.
// ---------------------------------------------------------------------------

/// `noa.Role` values.
pub const ROLE_USER: &str = "user";
pub const ROLE_ASSISTANT: &str = "assistant";
pub const ROLE_TOOL: &str = "tool";

/// `noa.ContentType` values.
pub const CT_TEXT: &str = "text";
pub const CT_TOOL_CALL: &str = "tool-call";
pub const CT_TOOL_RESULT: &str = "tool-result";
pub const CT_REASONING: &str = "reasoning";

/// `noa.CoreMessage` — one flattened message. Go marshals with field names
/// (`ID`, `Role`, `ContentType`, `Text`, `ToolName`, `ToolCallID`), no json tags.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CoreMessage {
    #[serde(rename = "ID")]
    pub id: String,
    pub role: String,
    pub content_type: String,
    pub text: String,
    pub tool_name: String,
    #[serde(rename = "ToolCallID")]
    pub tool_call_id: String,
}

/// `noa.TurnGroup` — one assistant turn: leading reasoning run, the assistant acts
/// after it, and the tool results those acts produced. Each index list is a Go nil
/// slice when empty (rendered `null`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TurnGroup {
    pub reasoning_idx: Option<Vec<i64>>,
    pub act_idx: Option<Vec<i64>>,
    pub result_idx: Option<Vec<i64>>,
}

/// `noa.isAssistantAct` (unexported; used transitively): an assistant text or
/// tool-call message.
fn is_assistant_act(m: &CoreMessage) -> bool {
    m.role == ROLE_ASSISTANT && (m.content_type == CT_TEXT || m.content_type == CT_TOOL_CALL)
}

/// `noa.ComputeTurnGroups` — partition messages into assistant turns. Returns
/// `None` (Go nil slice → `null`) when there are no messages / no groups.
pub fn compute_turn_groups(msgs: &[CoreMessage]) -> Option<Vec<TurnGroup>> {
    let mut groups: Vec<TurnGroup> = Vec::new();
    let mut i = 0usize;
    while i < msgs.len() {
        if msgs[i].content_type != CT_REASONING && !is_assistant_act(&msgs[i]) {
            i += 1;
            continue;
        }
        let mut g = TurnGroup::default();
        let mut reasoning: Vec<i64> = Vec::new();
        while i < msgs.len() && msgs[i].content_type == CT_REASONING {
            reasoning.push(i as i64);
            i += 1;
        }
        g.reasoning_idx = opt_vec(reasoning);

        let mut call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut acts: Vec<i64> = Vec::new();
        while i < msgs.len() && is_assistant_act(&msgs[i]) {
            acts.push(i as i64);
            if !msgs[i].tool_call_id.is_empty() {
                call_ids.insert(msgs[i].tool_call_id.clone());
            }
            i += 1;
        }
        g.act_idx = opt_vec(acts);

        let mut results: Vec<i64> = Vec::new();
        while i < msgs.len()
            && msgs[i].content_type == CT_TOOL_RESULT
            && call_ids.contains(&msgs[i].tool_call_id)
        {
            results.push(i as i64);
            i += 1;
        }
        g.result_idx = opt_vec(results);

        if g.reasoning_idx.is_some() || g.act_idx.is_some() {
            groups.push(g);
        }
    }
    opt_vec_val(groups)
}

/// Fold a built `Vec` into `Option`: empty → `None` (Go nil slice → `null`).
fn opt_vec(v: Vec<i64>) -> Option<Vec<i64>> {
    if v.is_empty() { None } else { Some(v) }
}

/// Fold a built `Vec<TurnGroup>` into `Option`: empty → `None`.
fn opt_vec_val(v: Vec<TurnGroup>) -> Option<Vec<TurnGroup>> {
    if v.is_empty() { None } else { Some(v) }
}

/// `noa.WithdrawSplitTurns` — remove from `folded` any turn that would be left
/// broken (reasoning folded away while one of its tool calls stays visible).
///
/// Returns `(out, withdrawn)`. `folded == None` (Go nil map) → `(None, None)`;
/// an empty non-nil map → `(Some(empty), None)`; otherwise a processed copy plus
/// the withdrawn ids (`None` if none were withdrawn, mirroring Go's nil slice).
pub fn withdraw_split_turns(
    msgs: &[CoreMessage],
    folded: Option<std::collections::HashMap<String, bool>>,
) -> (
    Option<std::collections::HashMap<String, bool>>,
    Option<Vec<String>>,
) {
    let mut out = match folded {
        None => return (None, None),
        Some(m) => m,
    };
    if out.is_empty() {
        return (Some(out), None);
    }

    let mut withdrawn: Vec<String> = Vec::new();
    for g in compute_turn_groups(msgs).unwrap_or_default() {
        let reasoning: Vec<i64> = g.reasoning_idx.clone().unwrap_or_default();
        if reasoning.is_empty() {
            continue; // nothing to lose
        }
        let reasoning_folded = reasoning.iter().any(|&i| {
            out.get(msgs[i as usize].id.as_str())
                .copied()
                .unwrap_or(false)
        });
        if !reasoning_folded {
            continue;
        }
        let acts: Vec<i64> = g.act_idx.clone().unwrap_or_default();
        let call_stays_visible = acts.iter().any(|&i| {
            msgs[i as usize].content_type == CT_TOOL_CALL
                && !out
                    .get(msgs[i as usize].id.as_str())
                    .copied()
                    .unwrap_or(false)
        });
        if !call_stays_visible {
            continue;
        }
        // Flatten the group's indices and delete any that are in `out`.
        let mut all: Vec<i64> = Vec::new();
        all.extend(reasoning);
        all.extend(acts);
        all.extend(g.result_idx.clone().unwrap_or_default());
        for i in all {
            let id = msgs[i as usize].id.clone();
            if out.get(&id).copied().unwrap_or(false) {
                out.remove(&id);
                withdrawn.push(id);
            }
        }
    }
    let withdrawn_opt = if withdrawn.is_empty() {
        None
    } else {
        Some(withdrawn)
    };
    (Some(out), withdrawn_opt)
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

// ---------------------------------------------------------------------------
// Batch 9d: pair-boundary adjusters over CoreMessage.
// ---------------------------------------------------------------------------

/// `noa.CompressToolName` — the single source of truth for the tool's name.
pub const COMPRESS_TOOL_NAME: &str = "Compress";
/// `noa.ToolPairMaxScan` — bounds how far boundary adjustment looks for the other
/// half of a tool exchange.
pub const TOOL_PAIR_MAX_SCAN: i64 = 20;

/// `noa.AdjustBoundariesForToolPairs` — widen `[start, end]` so no tool exchange
/// is cut in half. The scan is bounded by `max_scan` in each direction; a gap
/// (message whose call id is not in range) stops the walk.
pub fn adjust_boundaries_for_tool_pairs(
    start: i64,
    end: i64,
    msgs: &[CoreMessage],
    max_scan: i64,
) -> (i64, i64) {
    if start < 0 || end >= msgs.len() as i64 || start > end {
        return (start, end);
    }
    let mut start = start;
    let mut end = end;

    let mut in_range: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in &msgs[start as usize..=end as usize] {
        if !m.tool_call_id.is_empty() && m.tool_name != COMPRESS_TOOL_NAME {
            in_range.insert(m.tool_call_id.clone());
        }
    }
    if in_range.is_empty() {
        return (start, end);
    }

    let forward_limit = std::cmp::min(end + max_scan, msgs.len() as i64 - 1);
    let backward_limit = std::cmp::max(start - max_scan, 0);

    let mut i = end + 1;
    while i <= forward_limit {
        let m = &msgs[i as usize];
        if m.tool_call_id.is_empty() || !in_range.contains(&m.tool_call_id) {
            break;
        }
        end = i;
        i += 1;
    }
    let mut i = start - 1;
    while i >= backward_limit {
        let m = &msgs[i as usize];
        if m.tool_call_id.is_empty() || !in_range.contains(&m.tool_call_id) {
            break;
        }
        start = i;
        i -= 1;
    }
    (start, end)
}

/// `noa.AdjustBoundariesForReasoningPairs` — widen `[start, end]` so a reasoning
/// run and the assistant act it belongs to stay together.
pub fn adjust_boundaries_for_reasoning_pairs(
    start: i64,
    end: i64,
    msgs: &[CoreMessage],
) -> (i64, i64) {
    if start < 0 || end >= msgs.len() as i64 || start > end {
        return (start, end);
    }
    let mut start = start;
    let mut end = end;

    // A reasoning run at the tail pulls in the assistant burst that follows it.
    if msgs[end as usize].content_type == CT_REASONING {
        let mut i = end + 1;
        while (i as usize) < msgs.len() && msgs[i as usize].content_type == CT_REASONING {
            i += 1;
        }
        if (i as usize) < msgs.len() && is_assistant_act(&msgs[i as usize]) {
            while (i as usize) < msgs.len() && is_assistant_act(&msgs[i as usize]) {
                end = i;
                i += 1;
            }
        }
    }

    // An assistant act at the head pulls in the reasoning run before it.
    if is_assistant_act(&msgs[start as usize]) {
        let mut i = start - 1;
        while i >= 0 && msgs[i as usize].content_type == CT_REASONING {
            start = i;
            i -= 1;
        }
    }
    (start, end)
}

/// `noa.ApplyPairBoundaryAdjustments` — run both widenings to a fixed point, at
/// most twice (each widening can expose work for the other exactly once).
pub fn apply_pair_boundary_adjustments(start: i64, end: i64, msgs: &[CoreMessage]) -> (i64, i64) {
    let mut start = start;
    let mut end = end;
    for _ in 0..2 {
        let (s0, e0) = (start, end);
        let (s, e) = adjust_boundaries_for_reasoning_pairs(s0, e0, msgs);
        let (s, e) = adjust_boundaries_for_tool_pairs(s, e, msgs, TOOL_PAIR_MAX_SCAN);
        if s == start && e == end {
            break;
        }
        start = s;
        end = e;
    }
    (start, end)
}
