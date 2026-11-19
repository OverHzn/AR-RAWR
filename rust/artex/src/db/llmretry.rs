//! LLM retry policy — port of `db/llmretry.go`.
//!
//! The global "attempts + interval" configuration for the retry layers, stored as
//! one JSON value in the settings table (`llm_retry_policy`). Reads fall back to
//! built-in defaults (the zero policy), so a missing key behaves exactly like the
//! hard-coded-constant era. Out-of-range values are clamped on the way in.
//!
//! `RetryOverride` is declared here because its `Clamped` lives here; `config.go`
//! (the profile CRUD) embeds it as `LLMProfile.Retry` when that group is ported.

use crate::db::error::Result;
use crate::db::DB;
use std::time::Duration;

const SETTING_LLM_RETRY_POLICY: &str = "llm_retry_policy";

/// `RetryRule` — one layer's knob pair. The zero value means "unset":
/// `Attempts` 0 = built-in default, -1 = disable, >0 = this value; `IntervalMS`
/// 0 = keep original backoff, >0 = fixed millisecond interval.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryRule {
    pub attempts: i32,
    pub interval_ms: i32,
}

/// retry knob bounds.
const MAX_RETRY_ATTEMPTS: i32 = 20;
const MAX_RETRY_INTERVAL_MS: i32 = 3_600_000; // 1h

impl RetryRule {
    /// `Interval`: the configured fixed interval, or 0 when unset.
    pub fn interval(&self) -> Duration {
        if self.interval_ms <= 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(self.interval_ms as u64)
        }
    }

    /// `Or`: fill each unset field (== 0) from `fallback`.
    pub fn or(&self, fallback: RetryRule) -> RetryRule {
        RetryRule {
            attempts: if self.attempts == 0 {
                fallback.attempts
            } else {
                self.attempts
            },
            interval_ms: if self.interval_ms == 0 {
                fallback.interval_ms
            } else {
                self.interval_ms
            },
        }
    }

    /// `Clamped`: pull out-of-range values into the sane band (attempts in
    /// [-1, 20], interval in [0, 1h]).
    pub fn clamped(&self) -> RetryRule {
        RetryRule {
            attempts: self.attempts.clamp(-1, MAX_RETRY_ATTEMPTS),
            interval_ms: self.interval_ms.clamp(0, MAX_RETRY_INTERVAL_MS),
        }
    }
}

/// `RetryOverride` — a profile's per-request retry override (connect/empty/stream).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryOverride {
    pub connect: RetryRule,
    pub empty: RetryRule,
    pub stream: RetryRule,
}

impl RetryOverride {
    /// `Clamped`: bound each layer like the global policy.
    pub fn clamped(&self) -> RetryOverride {
        RetryOverride {
            connect: self.connect.clamped(),
            empty: self.empty.clamped(),
            stream: self.stream.clamped(),
        }
    }
}

/// `LLMRetryPolicy` — the five-layer retry configuration. Connect/Empty/Stream are
/// per-request layers (a profile may override them); Breaker/Intent are global.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LLMRetryPolicy {
    pub connect: RetryRule,
    pub empty: RetryRule,
    pub stream: RetryRule,
    pub breaker: RetryRule,
    pub intent: RetryRule,
}

impl LLMRetryPolicy {
    /// `Clamped`: clamp every rule.
    pub fn clamped(&self) -> LLMRetryPolicy {
        LLMRetryPolicy {
            connect: self.connect.clamped(),
            empty: self.empty.clamped(),
            stream: self.stream.clamped(),
            breaker: self.breaker.clamped(),
            intent: self.intent.clamped(),
        }
    }
}

impl DB {
    /// `LLMRetryPolicy`: read the global policy. A missing/unparseable value yields
    /// the zero policy (every layer on its built-in default).
    pub async fn llm_retry_policy(&self) -> LLMRetryPolicy {
        let (raw, ok) = match self.get_setting(SETTING_LLM_RETRY_POLICY).await {
            Ok(v) => v,
            Err(_) => return LLMRetryPolicy::default(),
        };
        if !ok || raw.is_empty() {
            return LLMRetryPolicy::default();
        }
        match serde_json::from_str::<LLMRetryPolicy>(&raw) {
            Ok(p) => p.clamped(),
            Err(_) => LLMRetryPolicy::default(),
        }
    }

    /// `SetLLMRetryPolicy`: persist the policy (clamped first).
    pub async fn set_llm_retry_policy(&self, p: LLMRetryPolicy) -> Result<()> {
        let raw = serde_json::to_string(&p.clamped())
            .map_err(|e| crate::db::error::Error::msg(e.to_string()))?;
        self.set_setting(SETTING_LLM_RETRY_POLICY, &raw).await
    }
}
