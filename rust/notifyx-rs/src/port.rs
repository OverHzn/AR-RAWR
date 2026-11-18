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
