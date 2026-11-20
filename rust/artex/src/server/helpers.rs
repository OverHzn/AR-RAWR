//! Pure server helpers — slice of `server/manager.go`.

use serde_json::Value as Json;

/// `decodeStrMap`: JSON object -> string map; null / invalid / empty input
/// yields a writable empty map (callers assign into it).
pub fn decode_str_map(raw: Option<&[u8]>) -> std::collections::BTreeMap<String, String> {
    let Some(raw) = raw else {
        return std::collections::BTreeMap::new();
    };
    if raw.is_empty() {
        return std::collections::BTreeMap::new();
    }
    match serde_json::from_slice::<Option<Json>>(raw) {
        Ok(Some(Json::Object(map))) => map
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect(),
        _ => std::collections::BTreeMap::new(),
    }
}

/// `encodeJSON`: serialize a value to JSON bytes (None on failure).
pub fn encode_json(v: &impl serde::Serialize) -> Option<Vec<u8>> {
    serde_json::to_vec(v).ok()
}

// Goal ambiguity gate (server/goal_gate.go, #99).

use regex::Regex;

const GOAL_MIN_LEN: usize = 80;

/// `goalGateCheck`: reject goals too short or naming no concrete target, unless
/// the literal [targetless] opt-out is present.
pub fn goal_gate_check(goal: &str) -> Result<(), String> {
    let g = goal.trim();
    if Regex::new(r"\[targetless\]").unwrap().is_match(g) {
        return Ok(());
    }
    if g.len() < GOAL_MIN_LEN {
        return Err("goal is too short to carry a testable objective (min 80 chars): name the target and what counts as success; add the literal marker [targetless] only for genuinely target-less analysis tasks".to_string());
    }
    let patterns = [
        r"(?i)\bhttps?://\S+",
        r"(?i)\b[a-z0-9][a-z0-9-]{1,61}\.[a-z]{2,}(:[0-9]+)?(/\S*)?\b",
        r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b",
        r"(?i)\blocalhost:[0-9]+\b",
        r"(?i)(^|\s)/[a-z0-9_-]+(/[a-z0-9_.-]+)+",
    ];
    if patterns.iter().any(|p| Regex::new(p).unwrap().is_match(g)) {
        return Ok(());
    }
    Err("goal names no target: include the URL/host/IP/path under test (or the literal marker [targetless] for target-less analysis tasks) so workers cannot be dispatched against an unrelated asset".to_string())
}

// Worker model-error retry classification (server/engine.go).

/// Error kinds that make a worker model error non-retryable.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerModelError {
    NoaBudget,
    TaskLlm,
    Generic,
}

impl WorkerModelError {
    pub fn noa_budget() -> Self {
        WorkerModelError::NoaBudget
    }
    pub fn task_llm() -> Self {
        WorkerModelError::TaskLlm
    }
}

/// `retryableWorkerModelError`: only model errors that are neither a NOA budget
/// refusal nor a task-LLM runtime error are retried.
pub fn retryable_worker_model_error(is_model_error: bool, err: &WorkerModelError) -> bool {
    is_model_error && !matches!(err, WorkerModelError::NoaBudget | WorkerModelError::TaskLlm)
}
