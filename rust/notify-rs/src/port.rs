//! The Rust port of `notify/`.
//!
//! Faithful port of the pure helpers in `notify/`: channel registry lookups,
//! severity/label helpers, config masking, byte/rune/HTML truncation, and filter
//! parsing/matching. `rust-anything diff` checks behavior against the source.

#![forbid(unsafe_code)]

use serde_json::{Map, Value, json};

// ===========================================================================
// notify.go: channel kinds, severity, status
// ===========================================================================

const KIND_DINGTALK: &str = "dingtalk";
const KIND_FEISHU: &str = "feishu";
const KIND_WECOM: &str = "wecom";
const KIND_WEBHOOK: &str = "webhook";
const KIND_TELEGRAM: &str = "telegram";
const KIND_EMAIL: &str = "email";

const EVENT_FINDING_STATUS_CHANGED: &str = "finding_status_changed";

fn registry_kinds() -> [&'static str; 6] {
    [
        KIND_DINGTALK,
        KIND_FEISHU,
        KIND_WECOM,
        KIND_WEBHOOK,
        KIND_TELEGRAM,
        KIND_EMAIL,
    ]
}

fn severity_rank_of(severity: &str) -> i64 {
    match severity {
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "critical" => 4,
        _ => 0,
    }
}

// ===========================================================================
// render.go: truncation helpers
// ===========================================================================

const ELLIPSIS: &str = "\u{2026}"; // "…"

/// Go's `utf8.RuneStart(b)`: reports whether b is the first byte of a rune.
fn is_rune_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

/// Port of `TruncateBytes` (`notify/render.go:22`).
pub fn truncate_bytes(s: String, max: i64) -> Result<String, String> {
    if max <= 0 || s.len() as i64 <= max {
        return Ok(s);
    }
    let max = max as usize;
    let mut budget = max as i64 - ELLIPSIS.len() as i64;
    let mut suffix = ELLIPSIS;
    if budget < 0 {
        // max is shorter than the ellipsis: drop the ellipsis, just truncate.
        budget = max as i64;
        suffix = "";
    }
    let bytes = s.as_bytes();
    let mut cut = budget as usize;
    while cut > 0 && cut < bytes.len() && !is_rune_start(bytes[cut]) {
        cut -= 1;
    }
    let cut = cut.min(bytes.len());
    Ok(format!("{}{}", &s[..cut], suffix))
}

/// Go's `strings.Fields` + `strings.Join(fields, " ")`.
fn go_fields_join(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Port of `TruncateRunes` (`notify/render.go:58`).
pub fn truncate_runes(s: String, max: i64) -> Result<String, String> {
    if max <= 0 {
        return Ok(s);
    }
    let max = max as usize;
    let runes: Vec<char> = s.chars().collect();
    if runes.len() <= max {
        return Ok(s);
    }
    if max <= 1 {
        return Ok(runes[..max].iter().collect());
    }
    let mut out: String = runes[..max - 1].iter().collect();
    out.push_str(ELLIPSIS);
    Ok(out)
}

/// Port of `OneLine` (`notify/render.go:45`).
pub fn one_line(s: String, max: i64) -> Result<String, String> {
    let s = go_fields_join(&s);
    truncate_runes(s, max)
}

/// Port of `TruncateHTML` (`notify/render.go:85`).
pub fn truncate_html(s: String, max: i64) -> Result<String, String> {
    if max <= 0 || s.chars().count() as i64 <= max {
        return Ok(s);
    }
    let mut cut = truncate_runes(s, max)?;
    // If the tail is a fragment starting with '<' (no '>' after the last '<'),
    // back off to before the '<'.
    if let Some(lt) = cut.rfind('<')
        && !cut[lt..].contains('>')
    {
        cut = cut[..lt].to_string();
    }
    // If the tail is a cut HTML entity (e.g. "&amp" without ";"), back off.
    if let Some(amp) = cut.rfind('&')
        && !cut[amp..].contains(';')
    {
        cut = cut[..amp].to_string();
    }
    Ok(cut)
}

// ===========================================================================
// mask.go: masking
// ===========================================================================

const MASKED_PREFIX: &str = "__masked__";

/// Port of `MaskedValue` (`notify/mask.go:29`).
pub fn masked_value(secret: String) -> Result<String, String> {
    if secret.len() <= 6 {
        return Ok(MASKED_PREFIX.to_string());
    }
    Ok(format!(
        "{MASKED_PREFIX}:\u{2026}{}",
        &secret[secret.len() - 6..]
    ))
}

/// Port of `IsMasked` (`notify/mask.go:38`).
pub fn is_masked(v: String) -> Result<bool, String> {
    Ok(v.starts_with(MASKED_PREFIX))
}

// ===========================================================================
// channel.go: registry lookups
// ===========================================================================

/// Port of `Get` (`notify/channel.go:71`). The `Channel` interface is not part
/// of the wire; the original returns a non-nil channel iff the kind is known.
pub fn get(kind: String) -> Result<Value, String> {
    let present = registry_kinds().contains(&kind.as_str());
    if present {
        Ok(json!(["__channel__", true]))
    } else {
        Ok(json!([Value::Null, false]))
    }
}

/// Port of `ValidKind` (`notify/channel.go:77`).
pub fn valid_kind(kind: String) -> Result<bool, String> {
    Ok(registry_kinds().contains(&kind.as_str()))
}

/// Port of `Kinds` (`notify/channel.go:83`): sorted lexicographically.
pub fn kinds() -> Result<Vec<String>, String> {
    let mut out: Vec<String> = registry_kinds().iter().map(|s| s.to_string()).collect();
    out.sort();
    Ok(out)
}

// ===========================================================================
// notify.go: severity + status labels
// ===========================================================================

/// Port of `SeverityRank` (`notify/notify.go:51`).
pub fn severity_rank(severity: String) -> Result<i64, String> {
    Ok(severity_rank_of(&severity))
}

/// Port of `SeverityLabel` (`notify/notify.go:55`).
pub fn severity_label(severity: String) -> Result<String, String> {
    Ok(match severity.as_str() {
        "critical" => "\u{1F534} Critical".to_string(),
        "high" => "\u{1F7E0} High".to_string(),
        "medium" => "\u{1F7E1} Medium".to_string(),
        "low" => "\u{1F535} Low".to_string(),
        _ => severity,
    })
}

/// Port of `StatusLabel` (`notify/notify.go:72`).
pub fn status_label(status: String) -> Result<String, String> {
    Ok(match status.as_str() {
        "pending" => "Pending".to_string(),
        "in_progress" => "In progress".to_string(),
        "confirmed" => "Confirmed".to_string(),
        "resolved" => "Resolved".to_string(),
        "fixed" => "Fixed".to_string(),
        "false_positive" => "False positive".to_string(),
        "ignored" => "Ignored".to_string(),
        "duplicate" => "Duplicate".to_string(),
        "risk_accepted" => "Risk accepted".to_string(),
        _ => status,
    })
}

/// Port of `AtLeast` (`notify/notify.go:100`).
pub fn at_least(severity: String, min: String) -> Result<bool, String> {
    if min.is_empty() {
        return Ok(true);
    }
    Ok(severity_rank_of(&severity) >= severity_rank_of(&min))
}

// ===========================================================================
// filter.go: ParseFilter + Match
// ===========================================================================

/// Port of `ParseFilter` (`notify/filter.go:43`). `raw` arrives as a list of
/// bytes (JSON has no []byte). On any parse failure the zero Filter is returned.
pub fn parse_filter(raw: Vec<i64>) -> Result<Value, String> {
    if raw.is_empty() {
        return Ok(empty_filter());
    }
    let bytes: Vec<u8> = raw.iter().map(|&b| b as u8).collect();
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(_) => return Ok(empty_filter()),
    };
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(empty_filter()),
    };
    Ok(filter_from_json(&value))
}

fn empty_filter() -> Value {
    json!({
        "min_severity": "",
        "task_ids": Value::Null,
        "asset_ids": Value::Null,
        "vulnclass_include": Value::Null,
        "vulnclass_exclude": Value::Null,
        "on_status_change": false
    })
}

/// Map a JSON object onto the Filter wire shape (missing/wrong-typed -> Go zero).
fn filter_from_json(v: &Value) -> Value {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return empty_filter(),
    };
    let get_str = |k: &str| -> String {
        obj.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_i64_list = |k: &str| -> Value {
        match obj.get(k).and_then(|x| x.as_array()) {
            Some(arr) => {
                let items: Vec<Value> = arr
                    .iter()
                    .map(|x| match x {
                        Value::Number(n) => json!(
                            n.as_i64()
                                .or_else(|| n.as_f64().map(|f| f as i64))
                                .unwrap_or(0)
                        ),
                        _ => json!(0),
                    })
                    .collect();
                Value::Array(items)
            }
            None => Value::Null,
        }
    };
    let get_str_list = |k: &str| -> Value {
        match obj.get(k).and_then(|x| x.as_array()) {
            Some(arr) => {
                let items: Vec<Value> = arr
                    .iter()
                    .map(|x| json!(x.as_str().unwrap_or("")))
                    .collect();
                Value::Array(items)
            }
            None => Value::Null,
        }
    };
    let on_status_change = obj
        .get("on_status_change")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    json!({
        "min_severity": get_str("min_severity"),
        "task_ids": get_i64_list("task_ids"),
        "asset_ids": get_i64_list("asset_ids"),
        "vulnclass_include": get_str_list("vulnclass_include"),
        "vulnclass_exclude": get_str_list("vulnclass_exclude"),
        "on_status_change": on_status_change
    })
}

/// Port of `ValidMinSeverity` (`notify/filter.go:55`).
pub fn valid_min_severity(s: String) -> Result<bool, String> {
    if s.is_empty() {
        return Ok(true);
    }
    Ok(matches!(s.as_str(), "low" | "medium" | "high" | "critical"))
}

struct Filter {
    min_severity: String,
    task_ids: Option<Vec<i64>>,
    asset_ids: Option<Vec<i64>>,
    vulnclass_include: Option<Vec<String>>,
    vulnclass_exclude: Option<Vec<String>>,
    on_status_change: bool,
}

struct Snapshot {
    kind: String,
    task_id: i64,
    vulnclass: String,
    severity: String,
    asset_ids: Option<Vec<i64>>,
}

fn filter_from_value(v: &Value) -> Filter {
    let obj = v.as_object();
    let get_str = |k: &str| -> String {
        obj.and_then(|o| o.get(k))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_i64_list = |k: &str| -> Option<Vec<i64>> {
        obj.and_then(|o| o.get(k))
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|x| match x {
                        Value::Number(n) => n
                            .as_i64()
                            .or_else(|| n.as_f64().map(|f| f as i64))
                            .unwrap_or(0),
                        _ => 0,
                    })
                    .collect()
            })
    };
    let get_str_list = |k: &str| -> Option<Vec<String>> {
        obj.and_then(|o| o.get(k))
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|x| x.as_str().unwrap_or("").to_string())
                    .collect()
            })
    };
    let on_status_change = obj
        .and_then(|o| o.get("on_status_change"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    Filter {
        min_severity: get_str("min_severity"),
        task_ids: get_i64_list("task_ids"),
        asset_ids: get_i64_list("asset_ids"),
        vulnclass_include: get_str_list("vulnclass_include"),
        vulnclass_exclude: get_str_list("vulnclass_exclude"),
        on_status_change,
    }
}

fn snapshot_from_value(v: &Value) -> Snapshot {
    let obj = v.as_object();
    let get_str = |k: &str| -> String {
        obj.and_then(|o| o.get(k))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let task_id = obj
        .and_then(|o| o.get("task_id"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let asset_ids = obj
        .and_then(|o| o.get("asset_ids"))
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|x| x.as_i64().unwrap_or(0))
                .collect::<Vec<i64>>()
        });
    Snapshot {
        kind: get_str("kind"),
        task_id,
        vulnclass: get_str("vulnclass"),
        severity: get_str("severity"),
        asset_ids,
    }
}

/// Go's `slices.Contains`.
fn contains_i64(list: &[i64], v: i64) -> bool {
    list.contains(&v)
}

/// Port of `intersectsInt` (`notify/filter.go:119`).
fn intersects_int(a: &[i64], b: &[i64]) -> bool {
    b.iter().any(|v| contains_i64(a, *v))
}

/// Port of `containsAnyFold` (`notify/filter.go:132`).
fn contains_any_fold(s: &str, keywords: &[String]) -> bool {
    let lower = s.to_lowercase();
    for kw in keywords {
        let kw = kw.trim().to_lowercase();
        if !kw.is_empty() && lower.contains(&kw) {
            return true;
        }
    }
    false
}

/// Port of `Match` (`notify/filter.go:91`).
pub fn r#match(f: Value, s: Value) -> Result<bool, String> {
    let f = filter_from_value(&f);
    let s = snapshot_from_value(&s);

    if s.kind == EVENT_FINDING_STATUS_CHANGED && !f.on_status_change {
        return Ok(false);
    }
    // AtLeast(s.Severity, f.MinSeverity)
    let at_least = if f.min_severity.is_empty() {
        true
    } else {
        severity_rank_of(&s.severity) >= severity_rank_of(&f.min_severity)
    };
    if !at_least {
        return Ok(false);
    }
    if let Some(task_ids) = &f.task_ids
        && !task_ids.is_empty()
        && !contains_i64(task_ids, s.task_id)
    {
        return Ok(false);
    }
    if let Some(asset_ids) = &f.asset_ids
        && !asset_ids.is_empty()
    {
        let s_asset_ids = s.asset_ids.clone().unwrap_or_default();
        if !intersects_int(asset_ids, &s_asset_ids) {
            return Ok(false);
        }
    }
    if let Some(exclude) = &f.vulnclass_exclude
        && !exclude.is_empty()
        && contains_any_fold(&s.vulnclass, exclude)
    {
        return Ok(false);
    }
    if let Some(include) = &f.vulnclass_include
        && !include.is_empty()
        && !contains_any_fold(&s.vulnclass, include)
    {
        return Ok(false);
    }
    Ok(true)
}

// ===========================================================================
// helpers kept for completeness (Go `strings.TrimSpace` semantics)
// ===========================================================================

#[allow(dead_code)]
fn go_trim_space(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

#[allow(dead_code)]
fn _unused_map(_m: &Map<String, Value>) {}
