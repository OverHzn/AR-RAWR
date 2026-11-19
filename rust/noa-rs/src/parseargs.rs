//! `ParseCompressArgs` — behavioral-parity Rust port of the lenient Compress-call
//! argument parser from the external `norma/noa` package (`parseinput.go`).
//!
//! A model's compression attempt can arrive with a trailing comma, a stringified
//! array, a markdown fence, a truncated tail, or a missing closing brace. The Go
//! parser recovers each layer in turn; this module mirrors that byte-for-byte so the
//! recorded `CompressParseResult` round-trips identically.
//!
//! The two Go `regexp` uses are reproduced by hand (no `regex` crate offline):
//!   - `fenceRE` = `(?s)^\s*```[a-zA-Z0-9]*\s*\n(.*?)\n?\s*```\s*$`  (strip fence)
//!   - `contentKeyRE` = `"content"\s*:\s*\[`                              (find array start)

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `noa.CompressParseKind` values.
pub const PARSE_OK: &str = "ok";
pub const PARSE_EMPTY_INPUT: &str = "empty-input";
pub const PARSE_NOT_OBJECT: &str = "not-object";
pub const PARSE_MISSING_CONTENT: &str = "missing-content";
pub const PARSE_CONTENT_NOT_LIST: &str = "content-not-array";
pub const PARSE_NO_VALID_RANGES: &str = "no-valid-ranges";
pub const PARSE_TRUNCATED: &str = "truncated";
pub const PARSE_MALFORMED_JSON: &str = "malformed-json";

const RAW_PREFIX_LEN: usize = 800;

/// `noa.CompressRange` — one span the model asked to compress. Go marshals with
/// field names (`StartRef`, `EndRef`, `Summary`, `Topic`, `SummaryMaxChars`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CompressRange {
    pub start_ref: String,
    pub end_ref: String,
    pub summary: String,
    pub topic: String,
    pub summary_max_chars: Option<i64>,
}

/// `noa.CompressParseDiagnostics` — how the arguments arrived.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CompressParseDiagnostics {
    pub ok: bool,
    pub kind: String,
    pub invalid_items: i64,
    pub invalid_reasons: Option<Vec<String>>,
    pub raw_prefix: String,
    pub length: i64,
    pub keys: Option<Vec<String>>,
    pub salvaged: bool,
    pub tail_repaired: bool,
}

/// `noa.CompressParseResult` — the parsed call.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CompressParseResult {
    pub ranges: Option<Vec<CompressRange>>,
    pub top_level_topic: String,
    pub summary_max_chars: Option<i64>,
    pub diagnostics: CompressParseDiagnostics,
}

/// `noa.ParseCompressArgs` — decode a Compress call as leniently as safely possible.
pub fn parse_compress_args(raw: &[u8]) -> CompressParseResult {
    let mut res = CompressParseResult::default();
    let d = &mut res.diagnostics;
    d.length = raw.len() as i64;
    if raw.len() > RAW_PREFIX_LEN {
        d.raw_prefix = String::from_utf8_lossy(&raw[..RAW_PREFIX_LEN]).into_owned();
    } else {
        d.raw_prefix = String::from_utf8_lossy(raw).into_owned();
    }

    let trimmed = String::from_utf8_lossy(raw).trim().to_string();
    if trimmed.is_empty() || trimmed == "null" {
        d.kind = PARSE_EMPTY_INPUT.to_string();
        return res;
    }

    // Layer 1-2: the arguments may themselves be a JSON string, and may be wrapped
    // in a markdown fence.
    let mut body = trimmed.clone();
    if let Ok(Value::String(s)) = serde_json::from_str::<Value>(&body) {
        body = s.trim().to_string();
    }
    body = strip_fence(&body);

    let obj = match try_parse_lenient(&body) {
        Some(o) => o,
        None => {
            // Layer 6: salvage whole entries by bracket matching.
            if let Some(entries) = salvage_content_entries(&body) {
                d.salvaged = true;
                d.kind = PARSE_TRUNCATED.to_string();
                res.ranges = entries_to_ranges(&entries, "", None, d);
                d.ok = res.ranges.as_ref().is_some_and(|r| !r.is_empty());
                return res;
            }
            d.kind = PARSE_MALFORMED_JSON.to_string();
            return res;
        }
    };
    // Keys: serde_json's Map is sorted (BTreeMap), matching the sorted oracle.
    d.keys = Some(obj.keys().cloned().collect());

    res.top_level_topic = string_field(&obj, "topic");
    res.summary_max_chars = int_field(&obj, "summaryMaxChars");

    let raw_content = match obj.get("content") {
        Some(v) => v.clone(),
        None => {
            d.kind = PARSE_MISSING_CONTENT.to_string();
            return res;
        }
    };

    let (entries, kind) = decode_content(&raw_content, d);
    if kind != PARSE_OK {
        d.kind = kind.to_string();
        return res;
    }

    res.ranges = entries_to_ranges(&entries, &res.top_level_topic, res.summary_max_chars, d);
    if res.ranges.as_ref().is_none_or(|r| r.is_empty()) && d.invalid_items > 0 {
        d.kind = PARSE_NO_VALID_RANGES.to_string();
        return res;
    }
    d.kind = PARSE_OK.to_string();
    d.ok = true;
    res
}

/// `noa.decodeContent` — unwrap the `content` field (array, or JSON-string of one).
fn decode_content(
    raw_content: &Value,
    d: &mut CompressParseDiagnostics,
) -> (Vec<Map<String, Value>>, &'static str) {
    match raw_content {
        Value::Array(arr) => (to_entry_maps(arr), PARSE_OK),
        Value::String(s) => {
            let s = strip_fence(s.trim());
            if let Some(arr) = try_parse_lenient_array(&s) {
                return (to_entry_maps(&arr), PARSE_OK);
            }
            // Layer 5: tail repair — a dropped final closing brace.
            if let Some(arr) = tail_repair(&s) {
                d.tail_repaired = true;
                return (to_entry_maps(&arr), PARSE_OK);
            }
            if let Some(entries) = salvage_content_entries(&s) {
                d.salvaged = true;
                return (entries, PARSE_OK);
            }
            (Vec::new(), PARSE_CONTENT_NOT_LIST)
        }
        _ => (Vec::new(), PARSE_CONTENT_NOT_LIST),
    }
}

/// `noa.toEntryMaps` — keep only object entries.
fn to_entry_maps(arr: &[Value]) -> Vec<Map<String, Value>> {
    arr.iter()
        .filter_map(|it| it.as_object().cloned())
        .collect()
}

/// `noa.entriesToRanges` — validate each entry and apply top-level defaults.
fn entries_to_ranges(
    entries: &[Map<String, Value>],
    top_topic: &str,
    top_max: Option<i64>,
    d: &mut CompressParseDiagnostics,
) -> Option<Vec<CompressRange>> {
    let mut out: Vec<CompressRange> = Vec::new();
    for e in entries {
        let start = first_string(e, &["startId", "startRef", "messageId"]);
        let end = first_string(e, &["endId", "endRef", "messageId"]);
        let summary = string_field(e, "summary");

        let mut why: Vec<String> = Vec::new();
        if start.is_empty() {
            why.push("missing startId".to_string());
        }
        if end.is_empty() {
            why.push("missing endId".to_string());
        }
        if summary.is_empty() {
            why.push("missing summary".to_string());
        }
        if !why.is_empty() {
            d.invalid_items += 1;
            d.invalid_reasons
                .get_or_insert_with(Vec::new)
                .push(why.join(", "));
            continue;
        }
        let mut r = CompressRange {
            start_ref: start,
            end_ref: end,
            summary,
            topic: string_field(e, "topic"),
            summary_max_chars: None,
        };
        if r.topic.is_empty() {
            r.topic = top_topic.to_string();
        }
        r.summary_max_chars = int_field(e, "summaryMaxChars").or(top_max);
        out.push(r);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `noa.stripFence` — remove a surrounding markdown code fence.
fn strip_fence(s: &str) -> String {
    // fenceRE = (?s)^\s*```[a-zA-Z0-9]*\s*\n(.*?)\n?\s*```\s*$
    let t = s;
    // leading \s*
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    let rest = &t[i..];
    if !rest.starts_with("```") {
        return s.to_string();
    }
    let after_ticks = &rest[3..];
    // [a-zA-Z0-9]* then \s* then \n
    let ab = after_ticks.as_bytes();
    let mut j = 0;
    while j < ab.len() && (ab[j] as char).is_ascii_alphanumeric() {
        j += 1;
    }
    let after_lang = &after_ticks[j..];
    // \s* then \n
    let bb = after_lang.as_bytes();
    let mut k = 0;
    while k < bb.len() && (bb[k] as char).is_ascii_whitespace() && bb[k] != b'\n' {
        k += 1;
    }
    if k >= after_lang.len() || after_lang.as_bytes()[k] != b'\n' {
        return s.to_string();
    }
    let body = &after_lang[k + 1..];
    // now (.*?)\n?\s*```\s*$ — non-greedy: find the last ``` and trailing \s*
    // Go regex with (?s) and non-greedy `.*?` matches the SHORTEST, i.e. first ```.
    // Then \n?\s*```\s*$ must reach end. Find the first "```" such that the remainder
    // after it (optionally a newline + spaces) is empty.
    if let Some(pos) = body.find("```") {
        let tail = &body[pos + 3..];
        // \s*$ — must be all whitespace to end
        if tail.trim().is_empty() {
            // group(1) = body[..pos], then \n? stripped by \s* ... actually group is
            // (.*?) before \n?``` — so body[..pos] possibly ending in \n which \n? eats.
            let group = &body[..pos];
            return group.trim().to_string();
        }
    }
    s.to_string()
}

/// `noa.tryParseLenient` — decode an object, repairing trailing commas and raw
/// newlines inside string literals.
fn try_parse_lenient(s: &str) -> Option<Map<String, Value>> {
    if let Ok(v) = serde_json::from_str::<Value>(s)
        && let Value::Object(m) = v
    {
        return Some(m);
    }
    if let Ok(v) = serde_json::from_str::<Value>(&strip_trailing_commas(s))
        && let Value::Object(m) = v
    {
        return Some(m);
    }
    if let Ok(v) = serde_json::from_str::<Value>(&escape_raw_newlines_in_strings(s))
        && let Value::Object(m) = v
    {
        return Some(m);
    }
    if let Ok(v) =
        serde_json::from_str::<Value>(&escape_raw_newlines_in_strings(&strip_trailing_commas(s)))
        && let Value::Object(m) = v
    {
        return Some(m);
    }
    None
}

/// `noa.tryParseLenientArray`.
fn try_parse_lenient_array(s: &str) -> Option<Vec<Value>> {
    if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
        return Some(a);
    }
    if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&strip_trailing_commas(s)) {
        return Some(a);
    }
    if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&escape_raw_newlines_in_strings(s)) {
        return Some(a);
    }
    None
}

/// `noa.tailRepair` — fix an array whose last object lost its closing brace.
fn tail_repair(s: &str) -> Option<Vec<Value>> {
    let t = s.trim();
    if !t.ends_with(']') {
        return None;
    }
    let without_bracket = t[..t.len() - 1].trim();
    if !without_bracket.ends_with('"') {
        return None;
    }
    let candidate = format!("{without_bracket}}}]");
    if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&candidate) {
        return Some(a);
    }
    None
}

/// `noa.salvageContentEntries` — recover whole objects from a truncated array by
/// matching braces by hand.
fn salvage_content_entries(s: &str) -> Option<Vec<Map<String, Value>>> {
    // contentKeyRE = "content"\s*:\s*\[  -> find its end; else first '['.
    let start = find_content_key_end(s).unwrap_or_else(|| s.find('[').map(|i| i + 1).unwrap_or(0));
    if start == 0 && !s.contains('[') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out: Vec<Map<String, Value>> = Vec::new();
    let mut depth = 0i32;
    let mut obj_start: i64 = -1;
    let mut in_str = false;
    let mut esc = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    obj_start = i as i64;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 && obj_start >= 0 {
                    if let Ok(Value::Object(m)) =
                        serde_json::from_str::<Value>(&s[obj_start as usize..=i])
                    {
                        out.push(m);
                    }
                    obj_start = -1;
                }
            }
            ']' if depth == 0 => {
                return if out.is_empty() { None } else { Some(out) };
            }
            _ => {}
        }
        i += 1;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Find the byte index just past `"content"\s*:\s*\[`.
fn find_content_key_end(s: &str) -> Option<usize> {
    let key = "\"content\"";
    let mut search = 0;
    while let Some(rel) = s[search..].find(key) {
        let after_key = search + rel + key.len();
        let ab = s.as_bytes();
        let mut j = after_key;
        while j < ab.len() && (ab[j] as char).is_ascii_whitespace() {
            j += 1;
        }
        if j < ab.len() && ab[j] == b':' {
            j += 1;
            while j < ab.len() && (ab[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            if j < ab.len() && ab[j] == b'[' {
                return Some(j + 1);
            }
        }
        search = after_key;
    }
    None
}

/// `noa.stripTrailingCommas` — remove commas before a closing brace/bracket,
/// outside string literals.
fn strip_trailing_commas(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut b: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut in_str = false;
    let mut esc = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            b.push(c);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            b.push(c);
            i += 1;
            continue;
        }
        if c == b',' {
            let mut j = i + 1;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\n' | b'\r' | b'\t') {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue; // drop the comma
            }
        }
        b.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&b).into_owned()
}

/// `noa.escapeRawNewlinesInStrings` — escape literal newlines/tabs inside string
/// literals.
fn escape_raw_newlines_in_strings(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut b: Vec<u8> = Vec::with_capacity(bytes.len() + 16);
    let mut in_str = false;
    let mut esc = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
                b.push(c);
            } else if c == b'\\' {
                esc = true;
                b.push(c);
            } else if c == b'"' {
                in_str = false;
                b.push(c);
            } else if c == b'\n' {
                b.extend_from_slice(b"\\n");
            } else if c == b'\r' {
                b.extend_from_slice(b"\\r");
            } else if c == b'\t' {
                b.extend_from_slice(b"\\t");
            } else {
                b.push(c);
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
        }
        b.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&b).into_owned()
}

/// `noa.stringField` — trimmed string value at `key`, or empty.
fn string_field(m: &Map<String, Value>, key: &str) -> String {
    m.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// `noa.firstString` — first non-empty string among `keys`.
fn first_string(m: &Map<String, Value>, keys: &[&str]) -> String {
    for k in keys {
        let s = string_field(m, k);
        if !s.is_empty() {
            return s;
        }
    }
    String::new()
}

/// `noa.intField` — integer value at `key` (accepts JSON number), else None.
fn int_field(m: &Map<String, Value>, key: &str) -> Option<i64> {
    m.get(key).and_then(|v| v.as_i64())
}
