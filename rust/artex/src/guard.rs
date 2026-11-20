//! Guard engine foundation — port of `guard/guard.go` (pure core with stubs).
//!
//! The guard decides whether a tool call proceeds. Rules run before the fallback
//! judge; a target-only guard runs only built-in deny rules and the target gate.

use std::sync::Mutex;

use crate::intercept::block_message::system_block_message;
use crate::intercept::rule_match::{match_first, CompiledRule, Decision};

pub const MAX_AUDIT: usize = 2000;

/// The subset of a norma hook event the guard reads.
#[derive(Debug, Clone, Default)]
pub struct HookEvent {
    pub tool_name: String,
    pub input: Vec<u8>,
    pub result: Vec<u8>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookResult {
    pub decision: String,
    pub message: String,
}

impl HookResult {
    pub fn allow() -> Self {
        HookResult::default()
    }
    pub fn block(msg: impl Into<String>) -> Self {
        HookResult {
            decision: "block".to_string(),
            message: msg.into(),
        }
    }
    pub fn is_blocked(&self) -> bool {
        self.decision == "block"
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditEntry {
    pub tool: String,
    pub action: String,
    pub reason: String,
    pub command: String,
}

/// The interceptor surface the guard needs. Real adapters (DB/LLM/pending rows)
/// can be layered on later; tests use deterministic stubs.
pub trait InterceptDecider {
    fn match_rules(&self, tool: &str, input: &[u8]) -> Option<Decision>;
    fn match_builtin(&self, _tool: &str, _input: &[u8]) -> Option<Decision> {
        None
    }
    fn is_tool_enabled(&self, _tool: &str) -> bool {
        true
    }
    fn judge(&self, _tool: &str, _input: &[u8]) -> Option<Decision> {
        None
    }
    fn log(&self, _tool: &str, _status: &str) {}
    fn handle_ask(&self, _decision: &Decision, _tool: &str, _input: &[u8]) -> bool {
        true
    }
}

/// A decider that allows everything (the no-interceptor guard).
#[derive(Debug, Default)]
pub struct NullDecider;
impl InterceptDecider for NullDecider {
    fn match_rules(&self, _tool: &str, _input: &[u8]) -> Option<Decision> {
        None
    }
    fn is_tool_enabled(&self, _tool: &str) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum GuardKind {
    Passthrough,
    Full,
    TargetOnly,
}

pub struct Guard {
    kind: GuardKind,
    decider: Option<Box<dyn InterceptDecider>>,
    builtin: Vec<CompiledRule>,
    audit: Mutex<Vec<AuditEntry>>,
}

fn command_from_input(input: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(input) else {
        return String::new();
    };
    v.get("command")
        .or_else(|| v.get("text"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

impl Guard {
    /// `New`: passthrough guard that still records an audit entry per call.
    pub fn new() -> Self {
        Guard {
            kind: GuardKind::Passthrough,
            decider: None,
            builtin: Vec::new(),
            audit: Mutex::new(Vec::new()),
        }
    }

    /// `NewTargetOnly`: no user rules, no judge, no audit for passing calls.
    pub fn target_only(builtin: Vec<CompiledRule>, decider: Box<dyn InterceptDecider>) -> Self {
        Guard {
            kind: GuardKind::TargetOnly,
            decider: Some(decider),
            builtin,
            audit: Mutex::new(Vec::new()),
        }
    }

    /// `NewWithInterceptor`: full guard (rules + judge).
    pub fn full(decider: Box<dyn InterceptDecider>) -> Self {
        Guard {
            kind: GuardKind::Full,
            decider: Some(decider),
            builtin: Vec::new(),
            audit: Mutex::new(Vec::new()),
        }
    }

    pub fn audit(&self) -> Vec<AuditEntry> {
        self.audit.lock().unwrap().clone()
    }

    fn record(&self, tool: &str, action: &str, reason: &str, cmd: &str) {
        let mut a = self.audit.lock().unwrap();
        if a.len() >= MAX_AUDIT {
            a.remove(0);
        }
        a.push(AuditEntry {
            tool: tool.to_string(),
            action: action.to_string(),
            reason: reason.to_string(),
            command: cmd.to_string(),
        });
    }

    fn block(&self, tool: &str, msg: String) -> HookResult {
        self.record(tool, "block", &msg, "");
        HookResult::block(msg)
    }

    /// `PreToolUse`: audit then intercept (target-only guards skip the audit ring).
    pub fn pre_tool_use(&self, ev: &HookEvent) -> HookResult {
        let cmd = command_from_input(&ev.input);
        if self.kind != GuardKind::TargetOnly {
            self.record(&ev.tool_name, "allow", "", &cmd);
        }
        let Some(decider) = &self.decider else {
            return HookResult::allow();
        };

        // Target-only: built-in deny rules only; never touch user rules/judge.
        if self.kind == GuardKind::TargetOnly {
            if let Some(dec) = match_first(&self.builtin, &ev.tool_name, &ev.input) {
                decider.log(&ev.tool_name, "denied");
                return self.block(&ev.tool_name, system_block_message(&dec.message));
            }
            return HookResult::allow();
        }

        // Full guard: rules first, judge only if no rule matched.
        let rules_enabled = decider.is_tool_enabled(&ev.tool_name);
        let dec = if rules_enabled {
            match decider.match_rules(&ev.tool_name, &ev.input) {
                Some(d) => Some(d),
                None => decider.judge(&ev.tool_name, &ev.input),
            }
        } else {
            None
        };
        let Some(dec) = dec else {
            return HookResult::allow();
        };
        match dec.action.as_str() {
            "deny" => {
                decider.log(&ev.tool_name, "denied");
                self.block(&ev.tool_name, system_block_message(&dec.message))
            }
            "allow" => {
                decider.log(&ev.tool_name, "allowed");
                HookResult::allow()
            }
            "ask" => {
                if !decider.handle_ask(&dec, &ev.tool_name, &ev.input) {
                    self.block(
                        &ev.tool_name,
                        system_block_message(
                            "manual approval not granted (user rejected or approval timed out)",
                        ),
                    )
                } else {
                    HookResult::allow()
                }
            }
            _ => HookResult::allow(),
        }
    }
}

impl Default for Guard {
    fn default() -> Self {
        Guard::new()
    }
}
