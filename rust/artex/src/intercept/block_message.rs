//! Model-facing block messages — pure slice of `guard/guard.go`.
//!
//! A block is framed as an ARTEX platform-governance decision, never as a
//! target-side defense: the agent must pivot, not try to evade a WAF.

use std::collections::HashMap;

pub const TARGET_APPROVAL_PENDING: &str = "TARGET_APPROVAL_PENDING";
pub const TARGET_DENIED: &str = "TARGET_DENIED";
pub const TARGET_RESOLVE_FAILED: &str = "TARGET_RESOLVE_FAILED";
pub const TARGET_RULE_BLOCKED: &str = "TARGET_RULE_BLOCKED";
pub const TARGET_TOO_MANY: &str = "TARGET_TOO_MANY";
pub const TARGET_GATE_UNAVAILABLE: &str = "TARGET_GATE_UNAVAILABLE";

const MAX_BLOCK_REASON_RUNES: usize = 4000;
const FRAME: &str = "[ARTEX platform control, not target defense]";

/// `TargetResult`: outcome of a target-gate request (the fields block messages use).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TargetResult {
    pub code: String,
    pub host: String,
    pub port: i32,
    pub reason: String,
}

fn target_guidance() -> HashMap<&'static str, &'static str> {
    [
        (TARGET_APPROVAL_PENDING, "The call was not executed. An approval by the operator is needed and may still arrive: retry the same call later. Do not rewrite the command, switch tools or substitute an IP address."),
        (TARGET_DENIED, "The call was not executed. The operator did not approve this target: do not retry it and do not reach it another way."),
        (TARGET_RESOLVE_FAILED, "The call was not executed and no approval was requested. This is a resolver failure on the platform side, not a block by the target. Do not rewrite the command, switch tools or substitute an IP address. Retry the same call later, or report the failure."),
        (TARGET_RULE_BLOCKED, "The call was not executed and no approval was requested. The operator's rules exclude this target: do not reach it another way."),
        (TARGET_TOO_MANY, "The call was not executed and no approval was requested. Name fewer new targets per call."),
        (TARGET_GATE_UNAVAILABLE, "The call was not executed. This is a failure on the platform side, not a block by the target. Retry the same call later, or report the failure."),
    ]
    .into_iter()
    .collect()
}

/// `capBlockReason`: cap an unbounded rule/judge reason to 4000 runes + marker.
pub fn cap_block_reason(s: &str) -> String {
    for (n, (i, _)) in s.char_indices().enumerate() {
        if n == MAX_BLOCK_REASON_RUNES {
            return format!("{} …(truncated)", &s[..i]);
        }
    }
    s.to_string()
}

/// `systemBlockMessage`: frame an intercept block as a platform decision.
pub fn system_block_message(reason: &str) -> String {
    format!(
        "{FRAME} This call was intercepted by the platform. Reason: {}. This operation is forbidden.",
        cap_block_reason(reason)
    )
}

/// `TargetBlockMessage`: the target-gate variant — code, target, reason, and the
/// guidance for that code (not always final — an approval may still arrive).
pub fn target_block_message(res: &TargetResult) -> String {
    let mut reason = cap_block_reason(&res.reason);
    if !res.host.is_empty() && !reason.contains(&res.host) {
        let target = if res.port != 0 {
            format!("{}:{}", res.host, res.port)
        } else {
            res.host.clone()
        };
        reason = format!("target {target}: {reason}");
    }
    let guidance = target_guidance();
    let g = guidance.get(res.code.as_str()).copied().unwrap_or("");
    format!("{FRAME} {}: {reason}. {g}", res.code)
}
