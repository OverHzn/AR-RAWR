//! Differential tests for `agent/provider.go` pure helpers — ports of
//! `TestIsQuotaExhaustedMessage` and `TestValidateUserAgent*` cases.

use artex::agent::provider::{
    is_falsy, is_quota_exhausted_message, normalize_ollama_base_url, validate_user_agent,
    DEFAULT_OLLAMA_BASE_URL, MAX_USER_AGENT_LEN,
};

#[test]
fn quota_exhausted_classification() {
    let positive = [
        r#"{"error":{"code":"insufficient_quota"}}"#,
        "RESOURCE_EXHAUSTED",
        "You exceeded your current quota, please check your plan and billing details.",
        "billing_not_active",
        "credit balance is too low",
        "insufficient funds, please top up",
    ];
    for m in positive {
        assert!(
            is_quota_exhausted_message(m),
            "expected quota classification for {m:?}"
        );
    }
    let negative = [
        "status 429: rate limit exceeded",
        "RESOURCE_EXHAUSTED: rate limit exceeded",
        "too many requests per minute",
        "quota exceeded for quota metric GenerateRequestsPerMinutePerProjectPerBaseModel",
        "RESOURCE_EXHAUSTED: TPM quota exceeded",
        "tokens per minute quota exceeded",
        "rate_limit_exceeded: requests per second",
        "status 401: invalid api key",
        "status 401: insufficient_quota",
        "HTTP 403: billing_hard_limit_reached",
        "status 500: internal server error",
        "status=503: insufficient_quota",
        "context length exceeded",
    ];
    for m in negative {
        assert!(
            !is_quota_exhausted_message(m),
            "unexpected quota classification for {m:?}"
        );
    }
}

#[test]
fn validate_user_agent_cases() {
    assert_eq!(
        validate_user_agent("artex-test-agent/1.2.3").unwrap(),
        "artex-test-agent/1.2.3"
    );
    assert_eq!(validate_user_agent("  good/1.0  ").unwrap(), "good/1.0");
    assert_eq!(validate_user_agent("").unwrap(), "");
    assert!(validate_user_agent("good/1.0\r\nX-Injected: 1").is_err());
    for bad in [
        "good/1.0\0",
        "good/1.0\u{1f}",
        "good/1.0\u{7f}",
        "good/1.0\tX",
    ] {
        assert!(
            validate_user_agent(bad).is_err(),
            "control char accepted: {bad:?}"
        );
    }
    let long = "a".repeat(MAX_USER_AGENT_LEN + 1);
    assert!(validate_user_agent(&long).is_err());
}

#[test]
fn config_string_helpers() {
    for v in ["0", " FALSE ", "off", "NO"] {
        assert!(is_falsy(v));
    }
    for v in ["1", "true", "yes", "", "nope"] {
        assert!(!is_falsy(v));
    }
    assert_eq!(normalize_ollama_base_url(""), DEFAULT_OLLAMA_BASE_URL);
    assert_eq!(
        normalize_ollama_base_url("http://localhost:11434"),
        "http://localhost:11434/v1"
    );
    assert_eq!(
        normalize_ollama_base_url("http://x:11434/v1/chat/completions"),
        "http://x:11434/v1"
    );
}
