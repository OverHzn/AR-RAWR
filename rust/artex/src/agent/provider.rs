//! Pure provider config helpers — slice of `agent/provider.go`.

use regex::Regex;

pub const MAX_USER_AGENT_LEN: usize = 256;
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434/v1";

const NON_FAILOVER_HTTP_STATUS: &str =
    r"(?:status(?:\s+code)?|http(?:\s+status)?)\s*[=:]?\s*(?:401|403|5\d\d)\b";
const TRANSIENT_QUOTA_LIMIT: &str = r"(?i)(?:\b(?:rpm|tpm|rpd|qps)\b|quota[_\s-]*metric|rate[_\s-]*limit|too many requests|(?:requests?|tokens?)\s+(?:per|/)\s*(?:second|minute)|(?:per|/)\s*(?:second|minute)\s+(?:requests?|tokens?)|generate[_\s-]*requests[_\s-]*per[_\s-]*(?:minute|second)|tokens?[_\s-]*per[_\s-]*(?:minute|second))";

/// `IsQuotaExhaustedMessage`: only explicit balance/billing/quota-exhaustion
/// signals rotate providers. Generic 429/rate-limit, auth and 5xx never do.
pub fn is_quota_exhausted_message(message: &str) -> bool {
    let m = message.to_lowercase();
    if Regex::new(NON_FAILOVER_HTTP_STATUS).unwrap().is_match(&m) {
        return false;
    }
    if Regex::new(TRANSIENT_QUOTA_LIMIT).unwrap().is_match(&m) {
        return false;
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
    if MARKERS.iter().any(|marker| m.contains(marker)) {
        return true;
    }
    m.contains("resource_exhausted")
        && !m.contains("rate limit")
        && !m.contains("too many requests")
}

/// `ValidateUserAgent`: trim, cap at 256 bytes, reject every control character.
pub fn validate_user_agent(raw: &str) -> Result<String, String> {
    let ua = raw.trim();
    if ua.is_empty() {
        return Ok(String::new());
    }
    if ua.len() > MAX_USER_AGENT_LEN {
        return Err(format!(
            "user agent too long: {} bytes (max {})",
            ua.len(),
            MAX_USER_AGENT_LEN
        ));
    }
    for c in ua.chars() {
        if (c as u32) < 0x20 || c == '\u{7f}' {
            return Err("user agent contains a control character".to_string());
        }
    }
    Ok(ua.to_string())
}

/// `isFalsy`: the four spellings treated as false in env/config values.
pub fn is_falsy(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// `normalizeOllamaBaseURL`: strip /chat/completions and trailing slashes,
/// default when empty, append /v1 once.
pub fn normalize_ollama_base_url(raw: &str) -> String {
    let mut b = raw.trim().to_string();
    if let Some(stripped) = b.strip_suffix("/chat/completions") {
        b = stripped.to_string();
    }
    b = b.trim_end_matches('/').to_string();
    if b.is_empty() {
        return DEFAULT_OLLAMA_BASE_URL.to_string();
    }
    if !b.ends_with("/v1") {
        b.push_str("/v1");
    }
    b
}

pub const DEFAULT_WINDOW_K: i64 = 200;
pub const MIN_WINDOW_K: i64 = 32;
pub const MAX_WINDOW_K: i64 = 1000;

/// `CompactionWindow`: window in tokens with a 32K floor (compaction math).
pub fn compaction_window(context_window_k: i64) -> i64 {
    let k = if context_window_k <= 0 {
        DEFAULT_WINDOW_K
    } else {
        context_window_k
    };
    k.clamp(MIN_WINDOW_K, MAX_WINDOW_K) * 1000
}

/// `ContextWindowTokens`: actual configured window (no compaction floor), for
/// pre-filtering and side-question budgets.
pub fn context_window_tokens(context_window_k: i64) -> i64 {
    let k = if context_window_k <= 0 {
        DEFAULT_WINDOW_K
    } else {
        context_window_k
    };
    k.min(MAX_WINDOW_K) * 1000
}
