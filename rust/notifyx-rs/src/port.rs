//! Rust port of the unexported, pure config helpers in the Go `notify/` package.
//!
//! The `cfg*` helpers read a typed value out of a decoded notification config
//! (`map[string]any`); the harness passes that map as a JSON object. `ValidKind` /
//! `Kinds` expose the channel registry's kind set (`registry` in channel.go).

use std::collections::BTreeMap;

use gocompat::strings;
use serde_json::Value;

/// `cfgString` (`notify/channel.go`): the trimmed string at `key`, or `""` when the
/// key is absent or its value is not a string.
pub fn cfg_string(cfg: &Value, key: &str) -> String {
    match cfg.get(key) {
        Some(Value::String(s)) => strings::go_trim_space(s).to_string(),
        _ => String::new(),
    }
}

/// `cfgInt` (`notify/channel.go`): the integer at `key`. JSON numbers (Go `float64`
/// after decode, or an `int`) truncate toward zero; strings parse as a trimmed
/// decimal (`strconv.Atoi`); anything else, or a parse error, yields 0.
pub fn cfg_int(cfg: &Value, key: &str) -> i64 {
    match cfg.get(key) {
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64).unwrap_or(0),
        Some(Value::String(s)) => strings::go_trim_space(s).parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// `cfgBool` (`notify/channel.go`): the bool at `key`. A string is lower-cased and
/// trimmed, then truthy when it is `"true"`, `"1"`, or `"yes"`; anything else is
/// false.
pub fn cfg_bool(cfg: &Value, key: &str) -> bool {
    match cfg.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            let s = strings::go_to_lower(strings::go_trim_space(s));
            s == "true" || s == "1" || s == "yes"
        }
        _ => false,
    }
}

/// `cfgStrings` (`notify/channel.go`): the non-empty trimmed strings in the array at
/// `key` (non-string elements skipped). When the value is not an array, a single
/// non-empty string is accepted as a one-element list. Returns `None` (JSON `null`)
/// when nothing usable is found, mirroring Go's nil slice.
pub fn cfg_strings(cfg: &Value, key: &str) -> Option<Vec<String>> {
    match cfg.get(key) {
        Some(Value::Array(raw)) => {
            let mut out = Vec::new();
            for v in raw {
                if let Value::String(s) = v {
                    let s = strings::go_trim_space(s);
                    if !s.is_empty() {
                        out.push(s.to_string());
                    }
                }
            }
            Some(out)
        }
        _ => {
            let s = cfg_string(cfg, key);
            if s.is_empty() { None } else { Some(vec![s]) }
        }
    }
}

/// `cfgMap` (`notify/channel.go`): the string→string map at `key`, keeping only
/// string values and non-blank trimmed keys. Returns `None` (JSON `null`) when the
/// value is not an object; an object with no usable pairs yields an empty map.
pub fn cfg_map(cfg: &Value, key: &str) -> Option<BTreeMap<String, String>> {
    match cfg.get(key) {
        Some(Value::Object(raw)) => {
            let mut out = BTreeMap::new();
            for (k, v) in raw {
                let k = strings::go_trim_space(k);
                if k.is_empty() {
                    continue;
                }
                if let Value::String(s) = v {
                    out.insert(k.to_string(), s.clone());
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// The channel kinds in `registry` (notify/channel.go:61), sorted for `Kinds`.
const KINDS: [&str; 6] = [
    "dingtalk", "email", "feishu", "telegram", "webhook", "wecom",
];

/// `ValidKind` (`notify/channel.go`): whether `kind` names a registered channel.
pub fn valid_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// `Kinds` (`notify/channel.go`): the registered channel kinds, sorted.
pub fn kinds() -> Vec<String> {
    KINDS.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Batch 9u: pure render/escape leaf helpers (notify/html.go, telegram.go,
// markdown.go, render.go, filter.go). All side-effect-free, value-typed.
// ---------------------------------------------------------------------------

/// `htmlEscape` (`notify/html.go`): escape HTML text content — `&`, `<`, `>`
/// (in that order, like Go's sequential `strings.ReplaceAll`).
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `htmlEscapeAttr` (`notify/html.go`): HTML attribute value — text escaping plus
/// `"`.
pub fn html_escape_attr(s: &str) -> String {
    html_escape(s).replace('"', "&quot;")
}

/// `telegramEscape` (`notify/telegram.go`): byte-identical to [`html_escape`] (the
/// Go function has the same three `strings.ReplaceAll` calls).
pub fn telegram_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `telegramEscapeAttr` (`notify/telegram.go`): text escaping plus `"`.
pub fn telegram_escape_attr(s: &str) -> String {
    telegram_escape(s).replace('"', "&quot;")
}

/// `markdownEscape` (`notify/markdown.go`): escape Markdown-significant characters
/// with a backslash. Go builds a `strings.NewReplacer` over 13 **single-char** old
/// strings; with single-char keys that is a single left-to-right pass where each
/// char maps to its escaped form (or itself) — no rescan of the replacement. The
/// per-char match below reproduces that exactly.
pub fn markdown_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '*' => out.push_str("\\*"),
            '_' => out.push_str("\\_"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '!' => out.push_str("\\!"),
            '#' => out.push_str("\\#"),
            '>' => out.push_str("\\>"),
            '|' => out.push_str("\\|"),
            '~' => out.push_str("\\~"),
            _ => out.push(c),
        }
    }
    out
}

/// `htmlSeverityColor` (`notify/html.go`): accent colour per severity.
pub fn html_severity_color(severity: &str) -> String {
    match severity {
        "critical" => "#d32029",
        "high" => "#e8830c",
        "medium" => "#d4b106",
        "low" => "#1677ff",
        _ => "#8c8c8c",
    }
    .to_string()
}

/// `containsAnyFold` (`notify/filter.go`): whether `s` (lower-cased) contains any
/// non-blank keyword, each keyword trimmed then lower-cased.
pub fn contains_any_fold(s: &str, keywords: &[String]) -> bool {
    let lower = strings::go_to_lower(s);
    for kw in keywords {
        let kw = strings::go_to_lower(strings::go_trim_space(kw));
        if !kw.is_empty() && lower.contains(&kw) {
            return true;
        }
    }
    false
}

/// `intersectsInt` (`notify/filter.go`): whether the two int64 slices share any
/// element (Go's `slices.Contains` scan over `b`).
pub fn intersects_int(a: &[i64], b: &[i64]) -> bool {
    b.iter().any(|v| a.contains(v))
}

/// `itoa` (`notify/render.go`): short alias for `strconv.Itoa`. For the i64 values
/// reaching it (counts), Rust's `to_string` is identical.
pub fn itoa(n: i64) -> String {
    n.to_string()
}

/// `assetLine` (`notify/render.go`): render an asset list onto one line; when there
/// are more than `limit` assets, the rest collapse to " and N more".
pub fn asset_line(assets: &[String], limit: i64) -> String {
    if assets.is_empty() {
        return String::new();
    }
    if limit <= 0 || (assets.len() as i64) <= limit {
        return assets.join(", ");
    }
    let head = assets[..limit as usize].join(", ");
    format!("{} and {} more", head, itoa(assets.len() as i64 - limit))
}

/// `byteSize` (`notify/render.go`): byte length of `s`.
pub fn byte_size(s: &str) -> i64 {
    s.len() as i64
}

/// `runeSize` (`notify/render.go`): rune count of `s`.
pub fn rune_size(s: &str) -> i64 {
    s.chars().count() as i64
}

// ---------------------------------------------------------------------------
// Batch 9u2: notify render leaves (render.go markdownText/snippet, TruncateRunes/
// OneLine, channel.go isBlankConfigValue/sameConfigValue). `markdown_escape` above.
// ---------------------------------------------------------------------------

/// The Markdown ellipsis (`…`), matching `notify.render.go`.
const ELLIPSIS: &str = "…";

/// `TruncateRunes` (`notify/render.go`): cut `s` to at most `max` runes, appending
/// an ellipsis when truncated. `max <= 0` means no truncation; `max <= 1` truncates
/// without an ellipsis.
pub fn truncate_runes(s: &str, max: i64) -> String {
    if max <= 0 {
        return s.to_string();
    }
    let runes: Vec<char> = s.chars().collect();
    if (runes.len() as i64) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return runes[..max as usize].iter().collect();
    }
    let mut out: String = runes[..(max - 1) as usize].iter().collect();
    out.push_str(ELLIPSIS);
    out
}

/// `OneLine` (`notify/render.go`): collapse whitespace to single spaces, then
/// [`truncate_runes`].
pub fn one_line(s: &str, max: i64) -> String {
    truncate_runes(&strings::go_fields_join(s), max)
}

/// `markdownText` (`notify/render.go`): [`markdown_escape`] of [`one_line`].
pub fn markdown_text(s: &str, max_runes: i64) -> String {
    markdown_escape(&one_line(s, max_runes))
}

/// `snippet` (`notify/render.go`): a one-line, 200-rune preview of raw bytes.
pub fn snippet(raw: &str) -> String {
    one_line(raw, 200)
}

/// `isBlankConfigValue` (`notify/channel.go`): nil is blank; a string is blank when
/// it trims to empty; every other type is non-blank.
pub fn is_blank_config_value(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => strings::go_trim_space(s).is_empty(),
        _ => false,
    }
}

/// `sameConfigValue` (`notify/channel.go`): both blank, or the two values marshal
/// to the same JSON (serde `Value` equality mirrors Go's `json.Marshal` string
/// comparison for the recorded corpus).
pub fn same_config_value(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    if is_blank_config_value(a) && is_blank_config_value(b) {
        return true;
    }
    a == b
}

// ---------------------------------------------------------------------------
// Batch 9af: small pure leaves in `notify/` (SMTP reply-code parse, Feishu
// severity → template colour).
// ---------------------------------------------------------------------------

/// `smtpReplyCode` (`notify/...`): the 3-digit SMTP reply code at the start of
/// `text`, or 0 when `text` is shorter than 3 bytes or the leading three bytes
/// are not an integer (matching `strconv.Atoi(text[:3])`).
pub fn smtp_reply_code(text: &str) -> i64 {
    let b = text.as_bytes();
    if b.len() < 3 {
        return 0;
    }
    std::str::from_utf8(&b[..3])
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_default()
}

/// `feishuSeverityTemplate` (`notify/...`): the Feishu card template colour for a
/// severity (`red`/`orange`/`yellow`/`blue`, default `grey`).
pub fn feishu_severity_template(severity: &str) -> String {
    match severity {
        "critical" => "red",
        "high" => "orange",
        "medium" => "yellow",
        "low" => "blue",
        _ => "grey",
    }
    .to_string()
}
