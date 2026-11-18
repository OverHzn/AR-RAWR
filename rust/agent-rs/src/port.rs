//! The Rust port of `agent/`.
//!
//! Replace each `unimplemented!` with the port of the source function. Return `Err` where
//! the source throws or raises: the caller gets an error there too. After changing a
//! signature, run `rust-anything refresh` to regenerate the bindings. `rust-anything diff` checks
//! behavior against what the source recorded.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use gocompat::strings::{go_has_suffix, go_to_lower, go_trim_right, go_trim_space, go_trim_suffix};
use serde_json::{Value, json};

/// `agent.Unlimited` = `math.MaxInt64`.
const UNLIMITED: i64 = i64::MAX;

/// `agent.BudgetMarginPercent`.
const BUDGET_MARGIN_PERCENT: i64 = 5;
/// `agent.BudgetMarginMinTokens`.
const BUDGET_MARGIN_MIN_TOKENS: i64 = 20_000;
/// `agent.BudgetMarginMinTurns`.
const BUDGET_MARGIN_MIN_TURNS: i64 = 2;

/// `agent.llm.Format` values used by `ConfigFrom`.
const FORMAT_ANTHROPIC: &str = "anthropic";
const FORMAT_OPENAI: &str = "openai";
const FORMAT_OPENAI_RESPONSES: &str = "openai-responses";
const FORMAT_OLLAMA: &str = "ollama";
const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434/v1";

fn budget_json(tokens: i64, turns: i64) -> Value {
    json!({ "Tokens": tokens, "Turns": turns })
}

fn budget_dim(b: &Value, key: &str) -> i64 {
    b.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// `agent.budgetCap`: `<=0` means Unlimited.
fn budget_cap(n: i64) -> i64 {
    if n <= 0 { UNLIMITED } else { n }
}

/// Port of `NewBudget` (`agent/budget.go:42`).
pub fn new_budget(tokens: i64, turns: i64) -> Result<Value, String> {
    Ok(budget_json(budget_cap(tokens), budget_cap(turns)))
}

/// Port of `EffectiveBudget` (`agent/budget.go:53`).
pub fn effective_budget(own: Value, parent_remaining: Value) -> Result<Value, String> {
    Ok(budget_json(
        budget_dim(&own, "Tokens").min(budget_dim(&parent_remaining, "Tokens")),
        budget_dim(&own, "Turns").min(budget_dim(&parent_remaining, "Turns")),
    ))
}

/// Port of `WrapupMargin` (`agent/budget.go:70`).
pub fn wrapup_margin(task: Value) -> Result<Value, String> {
    let m = |capacity: i64, floor: i64| -> i64 {
        if capacity == UNLIMITED {
            0
        } else {
            // Go: `capacity*BudgetMarginPercent/100` (int64 wrap-around), floored.
            capacity
                .wrapping_mul(BUDGET_MARGIN_PERCENT)
                .wrapping_div(100)
                .max(floor)
        }
    };
    Ok(budget_json(
        m(budget_dim(&task, "Tokens"), BUDGET_MARGIN_MIN_TOKENS),
        m(budget_dim(&task, "Turns"), BUDGET_MARGIN_MIN_TURNS),
    ))
}

/// Port of `BudgetWrapupBlocks` (`agent/budget.go:230`).
pub fn budget_wrapup_blocks(tool: String) -> Result<bool, String> {
    Ok(tool != "record_fact"
        && tool != "insert_assets"
        && tool != "report_finding"
        && tool != "update_finding_report"
        && tool != "record_coverage")
}

/// Port of `BuiltinPromptSeeds` (`agent/promptcatalog.go:140`).
pub fn builtin_prompt_seeds() -> Result<Value, String> {
    Ok(json!({
        "goals": include_str!("consts/goalsDefaultTmpl.txt"),
        "planner": include_str!("consts/plannerDefaultTmpl.txt"),
        "mainagent": include_str!("consts/mainAgentDefaultTmpl.txt"),
        "worker": include_str!("consts/workerDefaultTmpl.txt"),
        "auto": include_str!("consts/autoDefaultTmpl.txt"),
        "pentest": include_str!("consts/pentestDefaultTmpl.txt"),
    }))
}

/// `agent.normalizeOllamaBaseURL` (`agent/provider.go`).
fn normalize_ollama_base_url(raw: &str) -> String {
    let b = go_trim_right(go_trim_suffix(go_trim_space(raw), "/chat/completions"), "/");
    if b.is_empty() {
        return DEFAULT_OLLAMA_BASE_URL.to_string();
    }
    let mut b = b.to_string();
    if !go_has_suffix(&b, "/v1") {
        b.push_str("/v1");
    }
    b
}

/// Port of `ConfigFrom` (`agent/provider.go:293`).
pub fn config_from(
    provider: String,
    model: String,
    base_url: String,
    api_key: String,
    proxy: String,
) -> Result<Value, String> {
    let mut c_model = go_trim_space(&model).to_string();
    let mut c_base = go_trim_right(go_trim_space(&base_url), "/").to_string();
    let c_key = go_trim_space(&api_key).to_string();
    let c_proxy = go_trim_space(&proxy).to_string();
    let format: &str;
    match go_trim_space(&provider) {
        "openai" => {
            format = FORMAT_OPENAI;
            c_base = go_trim_right(go_trim_suffix(&c_base, "/chat/completions"), "/").to_string();
            if c_model.is_empty() {
                c_model = "gpt-4o".to_string();
            }
        }
        "openai-responses" => {
            format = FORMAT_OPENAI_RESPONSES;
            c_base = go_trim_right(go_trim_suffix(&c_base, "/responses"), "/").to_string();
            if c_model.is_empty() {
                c_model = "gpt-5".to_string();
            }
        }
        "ollama" => {
            format = FORMAT_OLLAMA;
            c_base = normalize_ollama_base_url(&c_base);
            if c_model.is_empty() {
                c_model = "llama3.2".to_string();
            }
        }
        _ => {
            format = FORMAT_ANTHROPIC;
            c_base = go_trim_right(go_trim_suffix(&c_base, "/v1/messages"), "/").to_string();
            if c_model.is_empty() {
                c_model = "claude-opus-4-8".to_string();
            }
        }
    }
    Ok(json!({
        "Format": format,
        "BaseURL": c_base,
        "APIKey": c_key,
        "Model": c_model,
        "Proxy": c_proxy,
        "RatePerSecond": 0.0,
        "RatePerMinute": 0.0,
        "ContextWindowK": 0,
        "ThinkingType": "",
        "ReasoningEffort": "",
        "Stream": true,
        "MaxTokens": 0,
        "MaxTokensField": "",
        "SessionHeaderKey": "",
        "UserAgent": "",
        "Retry": {
            "ConnectAttempts": 0,
            "ConnectInterval": 0,
            "EmptyAttempts": 0,
            "EmptyInterval": 0,
            "StreamAttempts": 0,
            "StreamInterval": 0,
        }
    }))
}

/// `agent.nonFailoverHTTPStatus` (`agent/provider.go:415`). Go `\b` is ASCII.
fn non_failover_http_status_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?:status(?:\s+code)?|http(?:\s+status)?)\s*[=:]?\s*(?:401|403|5\d\d)(?-u:\b)",
        )
        .expect("nonFailoverHTTPStatus regex")
    })
}

/// `agent.transientQuotaLimit` (`agent/provider.go:416`). Go `\b` is ASCII.
fn transient_quota_limit_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:(?-u:\b)(?:rpm|tpm|rpd|qps)(?-u:\b)|quota[_\s-]*metric|rate[_\s-]*limit|too many requests|(?:requests?|tokens?)\s+(?:per|/)\s*(?:second|minute)|(?:per|/)\s*(?:second|minute)\s+(?:requests?|tokens?)|generate[_\s-]*requests[_\s-]*per[_\s-]*(?:minute|second)|tokens?[_\s-]*per[_\s-]*(?:minute|second))",
        )
        .expect("transientQuotaLimit regex")
    })
}

/// Port of `IsQuotaExhaustedMessage` (`agent/provider.go:420`).
pub fn is_quota_exhausted_message(message: String) -> Result<bool, String> {
    let message = go_to_lower(&message);
    if non_failover_http_status_re().is_match(&message) {
        return Ok(false);
    }
    if transient_quota_limit_re().is_match(&message) {
        return Ok(false);
    }
    const MARKERS: &[&str] = &[
        "insufficient_quota",
        "quota_exceeded",
        "quota exceeded",
        "quota exhausted",
        "exceeded your current quota",
        "billing_hard_limit_reached",
        "billing hard limit",
        "billing_not_active",
        "credit balance",
        "insufficient credit",
        "insufficient balance",
        "balance is too low",
        "payment required",
        "status 402",
        "insufficient funds",
        "out of credit",
        "credit used up",
        "in arrears",
    ];
    for marker in MARKERS {
        if message.contains(marker) {
            return Ok(true);
        }
    }
    Ok(message.contains("resource_exhausted")
        && !message.contains("rate limit")
        && !message.contains("too many requests"))
}

/// Port of `ValidateUserAgent` (`agent/provider.go:566`).
pub fn validate_user_agent(raw: String) -> Result<String, String> {
    let ua = go_trim_space(&raw);
    if ua.is_empty() {
        return Ok(String::new());
    }
    if ua.len() > 256 {
        return Err(format!("user agent too long: {} bytes (max 256)", ua.len()));
    }
    for r in ua.chars() {
        if (r as u32) < 0x20 || (r as u32) == 0x7f {
            return Err("user agent contains a control character".to_string());
        }
    }
    Ok(ua.to_string())
}

/// Port of `IsRefusalStop` (`agent/providerstop.go:15`).
pub fn is_refusal_stop(stop_reason: String) -> Result<bool, String> {
    Ok(stop_reason == "refusal" || stop_reason == "content_filter")
}

/// Port of `WorkerSessionID` (`agent/worker.go:106`).
pub fn worker_session_id(exploration_id: i64, intent_id: i64) -> Result<String, String> {
    Ok(format!("exp{}-worker-i{}", exploration_id, intent_id))
}

/// Port of `WrapupDefault` (`agent/wrapup.go:92`).
pub fn wrapup_default(agent_key: String) -> Result<String, String> {
    Ok(match agent_key.as_str() {
        "worker" => include_str!("consts/settleWrapUpPrompt.txt").to_string(),
        "planner" => include_str!("consts/plannerWrapUpDefault.txt").to_string(),
        "mainagent" => include_str!("consts/mainAgentWrapUpDefault.txt").to_string(),
        _ => include_str!("consts/genericWrapUpDefault.txt").to_string(),
    })
}

/// Port of `WrapupTurnsDefault` (`agent/wrapup.go:101`).
pub fn wrapup_turns_default(_agent_key: String) -> Result<i64, String> {
    // wrapupTurnDefaults maps worker/planner/mainagent to 10; genericWrapupTurns = 10.
    Ok(10)
}

/// Port of `TaskTimeoutWrapupDefault` (`agent/wrapup.go:198`).
pub fn task_timeout_wrapup_default(agent_key: String) -> Result<String, String> {
    Ok(match agent_key.as_str() {
        "worker" => include_str!("consts/workerTaskTimeoutDefault.txt").to_string(),
        "planner" => include_str!("consts/plannerTaskTimeoutDefault.txt").to_string(),
        _ => String::new(),
    })
}
