//! `guard-rs` — behavioral-parity Rust port of the pure helpers in the Go
//! `guard/` package (`capBlockReason`, `systemBlockMessage`, `TargetBlockMessage`).
//!
//! The first two are unexported, so `rust-anything`'s bridge cannot reach them;
//! `TargetBlockMessage` is exported but takes a struct (`intercept.TargetResult`)
//! the bridge also skips. Parity is proven against the real Go functions:
//! `record.sh` runs a temporary oracle over a corpus and writes `cases.jsonl`;
//! `tests/parity.rs` replays it and asserts byte-identical output. The Go original
//! is untouched.

#![forbid(unsafe_code)]

use gocompat::neturl::join_host_port;

/// guard/guard.go `maxBlockReasonRunes`.
const MAX_BLOCK_REASON_RUNES: usize = 4000;

/// `capBlockReason` (guard/guard.go): keep at most `maxBlockReasonRunes` runes,
/// appending ` …(truncated)` if longer.
pub fn cap_block_reason(s: &str) -> String {
    for (n, (i, _)) in s.char_indices().enumerate() {
        if n == MAX_BLOCK_REASON_RUNES {
            return format!("{} …(truncated)", &s[..i]);
        }
    }
    s.to_string()
}

/// `systemBlockMessage` (guard/guard.go).
pub fn system_block_message(reason: &str) -> String {
    format!(
        "[ARTEX platform control, not target defense] This call was intercepted by the platform. \
Reason: {}. This operation is forbidden.",
        cap_block_reason(reason)
    )
}

/// `targetGuidance` (guard/guard.go): the per-code sentence appended after the
/// reason (a `map[string]string`; an unknown code yields `""`, Go's zero value).
fn target_guidance(code: &str) -> &'static str {
    match code {
        "TARGET_APPROVAL_PENDING" => {
            "The call was not executed. An approval by the operator is needed and may still arrive: retry the same call later. Do not rewrite the command, switch tools or substitute an IP address."
        }
        "TARGET_DENIED" => {
            "The call was not executed. The operator did not approve this target: do not retry it and do not reach it another way."
        }
        "TARGET_RESOLVE_FAILED" => {
            "The call was not executed and no approval was requested. This is a resolver failure on the platform side, not a block by the target. Do not rewrite the command, switch tools or substitute an IP address. Retry the same call later, or report the failure."
        }
        "TARGET_RULE_BLOCKED" => {
            "The call was not executed and no approval was requested. The operator's rules exclude this target: do not reach it another way."
        }
        "TARGET_TOO_MANY" => {
            "The call was not executed and no approval was requested. Name fewer new targets per call."
        }
        "TARGET_GATE_UNAVAILABLE" => {
            "The call was not executed. This is a failure on the platform side, not a block by the target. Retry the same call later, or report the failure."
        }
        _ => "",
    }
}

/// `TargetBlockMessage` (guard/guard.go): the model-facing block text for the
/// target gate. Reads `Code`, `Host`, `Port`, `Reason` of `intercept.TargetResult`.
/// Builds `"[ARTEX platform control, not target defense] <code>: <reason>. <guidance>"`
/// where `<reason>` is the capped reason, prefixed with `"target <host[:port]>: "`
/// when the host is set and not already named in it.
pub fn target_block_message(code: &str, host: &str, port: i64, reason: &str) -> String {
    let mut reason = cap_block_reason(reason);
    if !host.is_empty() && !reason.contains(host) {
        let target = if port != 0 {
            join_host_port(host, &port.to_string())
        } else {
            host.to_string()
        };
        reason = format!("target {target}: {reason}");
    }
    format!(
        "[ARTEX platform control, not target defense] {code}: {reason}. {}",
        target_guidance(code)
    )
}
