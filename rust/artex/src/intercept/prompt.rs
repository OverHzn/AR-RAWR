//! Judge prompt assembly + strict verdict parsing — port of `intercept/prompt.go`.
//!
//! `ParseVerdict` never extracts a decision keyword from prose or a broken JSON
//! reply; invalid/incomplete responses follow the configured model-failure path.

use crate::db::error::Error;
use crate::intercept::prompt_text::{
    DEFAULT_JUDGE_PROMPT, JUDGE_CONTEXT_BOUNDARY, JUDGE_OUTPUT_CONTRACT,
};

/// `Verdict`: the parsed outcome of the judge's JSON reply. `action` is empty
/// when unparseable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verdict {
    pub action: String,
    pub reason: String,
}

/// `EffectiveJudgePrompt`: append the context boundary + output contract to a
/// (possibly custom) review policy when absent.
pub fn effective_judge_prompt(prompt: &str) -> String {
    let mut p = prompt.to_string();
    if !p.contains(JUDGE_CONTEXT_BOUNDARY) {
        p.push_str("\n\n");
        p.push_str(JUDGE_CONTEXT_BOUNDARY);
    }
    if !p.contains(JUDGE_OUTPUT_CONTRACT) {
        p.push_str("\n\n");
        p.push_str(JUDGE_OUTPUT_CONTRACT);
    }
    p
}

/// `DefaultJudgePrompt`: the built-in review policy.
pub fn default_judge_prompt() -> &'static str {
    DEFAULT_JUDGE_PROMPT
}

/// `stripCodeFence`: deterministically unwrap a fenced reply (```json … ```)
/// before strict parsing. An UNCLOSED fence is left alone — completing it would
/// invent a verdict the model never gave.
fn strip_code_fence(text: &str) -> String {
    let t = text.trim();
    if t.len() <= 6 || !t.starts_with("```") || !t.ends_with("```") {
        return t.to_string();
    }
    let mut inner = t[3..t.len() - 3].trim();
    if !inner.starts_with('{') {
        // Drop the opening fence's language tag line (```json).
        if let Some((_, rest)) = inner.split_once('\n') {
            inner = rest.trim();
        }
    }
    inner.to_string()
}

/// `ParseVerdict`: require a complete, unambiguous JSON object with exactly a
/// decision and a structured explanation.
pub fn parse_verdict(text: &str) -> Verdict {
    let stripped = strip_code_fence(text);
    let v: serde_json::Value = match serde_json::from_str(&stripped) {
        Ok(v) => v,
        Err(_) => return Verdict::default(),
    };
    let obj = match v.as_object() {
        Some(o) if o.len() == 2 => o,
        _ => return Verdict::default(),
    };
    // Duplicate keys are rejected by serde_json's default map (last wins); Go's
    // strict decoder rejects them. Detect via a raw token scan below.
    if has_duplicate_keys(&stripped) {
        return Verdict::default();
    }
    let action = match obj.get("decision").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return Verdict::default(),
    };
    let reason = match obj.get("comment").and_then(|v| v.as_str()) {
        Some(r) => r.trim(),
        None => return Verdict::default(),
    };
    if !matches!(action, "allow" | "ask" | "deny") {
        return Verdict::default();
    }
    if reason.len() > 2400 || !reason.starts_with("Actual action: ") {
        return Verdict::default();
    }
    let after = &reason["Actual action: ".len()..];
    let Some((operation, rest)) = after.split_once("; Consequence if successful: ") else {
        return Verdict::default();
    };
    if operation.trim().is_empty() {
        return Verdict::default();
    }
    let Some((consequence, rule)) = rest.split_once("; Matched rule: ") else {
        return Verdict::default();
    };
    if consequence.trim().is_empty() || rule.trim().is_empty() {
        return Verdict::default();
    }
    Verdict {
        action: action.to_string(),
        reason: reason.to_string(),
    }
}

/// Detect duplicate keys in a flat JSON object (Go's decoder rejects them).
fn has_duplicate_keys(s: &str) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('"') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('"') else { break };
        let key = &rest[..close];
        rest = &rest[close + 1..];
        // Only keys immediately preceding a colon count (values are strings too,
        // but a value string is followed by ',' or '}', not ':').
        let after = rest.trim_start();
        if after.starts_with(':') {
            if seen.contains(&key) {
                return true;
            }
            seen.push(key);
        }
    }
    false
}

// Keep Error referenced (the judge caller maps unparseable replies to its own
// failure path; this module only classifies them).
#[allow(dead_code)]
fn _err(_: &Error) {}
