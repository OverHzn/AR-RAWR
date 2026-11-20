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
    gate: Option<Box<dyn TargetGate>>,
    audit: Mutex<Vec<AuditEntry>>,
    attrib: Mutex<std::collections::HashMap<String, usize>>,
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

impl<T: TargetGate + ?Sized> TargetGate for Box<T> {
    fn check_targets(&self, tool: &str, input: &[u8]) -> (Vec<TargetRequest>, TargetResult) {
        (**self).check_targets(tool, input)
    }
    fn request_targets(
        &self,
        tool: &str,
        input: &[u8],
        unpinned: &[TargetRequest],
        ask: bool,
    ) -> (TargetResult, bool) {
        (**self).request_targets(tool, input, unpinned, ask)
    }
}

impl Guard {
    /// `New`: passthrough guard that still records an audit entry per call.
    pub fn new() -> Self {
        Guard {
            kind: GuardKind::Passthrough,
            decider: None,
            builtin: Vec::new(),
            gate: None,
            audit: Mutex::new(Vec::new()),
            attrib: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// `NewTargetOnly`: no user rules, no judge, no audit for passing calls.
    pub fn target_only(builtin: Vec<CompiledRule>, decider: Box<dyn InterceptDecider>) -> Self {
        Guard {
            kind: GuardKind::TargetOnly,
            decider: Some(decider),
            builtin,
            gate: None,
            audit: Mutex::new(Vec::new()),
            attrib: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// `NewWithInterceptor`: full guard (rules + judge).
    pub fn full(decider: Box<dyn InterceptDecider>) -> Self {
        Guard {
            kind: GuardKind::Full,
            decider: Some(decider),
            builtin: Vec::new(),
            gate: None,
            audit: Mutex::new(Vec::new()),
            attrib: Mutex::new(std::collections::HashMap::new()),
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
        return self.pre_tool_use_with_scope(ev, None);
        #[allow(unreachable_code)]
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

impl Guard {
    /// `postToolUse`: Observer failure attribution (Bash only).
    pub fn post_tool_use(&self, ev: &HookEvent) -> HookResult {
        if ev.tool_name != "Bash" {
            return HookResult::allow();
        }
        let re = regex::Regex::new(
            r"(?i)\b(403|forbidden|waf|blocked|rate.?limit|429|captcha|denied)\b",
        )
        .unwrap();
        let class = if re.is_match(&String::from_utf8_lossy(&ev.result)) {
            "blocked"
        } else if ev.is_error {
            "error"
        } else {
            "ok"
        };
        *self
            .attrib
            .lock()
            .unwrap()
            .entry(class.to_string())
            .or_insert(0) += 1;
        HookResult::allow()
    }

    /// `Attributions`: failure-attribution counts (Observer / G5).
    pub fn attributions(&self) -> std::collections::HashMap<String, usize> {
        self.attrib.lock().unwrap().clone()
    }
}

// Target gate stage (guard/guard.go: requestTargets + gateScope).

use crate::intercept::block_message::{
    target_block_message, TargetResult, TARGET_APPROVAL_PENDING,
};

pub const TARGET_PASSES: usize = 3;

#[derive(Debug, Clone, Default)]
pub struct TargetScope {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TargetRequest {
    pub host: String,
    pub port: i32,
}

/// The target-gate surface the guard uses; a `fakeGate` in tests.
pub trait TargetGate {
    fn check_targets(&self, tool: &str, input: &[u8]) -> (Vec<TargetRequest>, TargetResult);
    fn request_targets(
        &self,
        tool: &str,
        input: &[u8],
        unpinned: &[TargetRequest],
        ask: bool,
    ) -> (TargetResult, bool);
}

impl Guard {
    /// Attach a target gate (builder-style, for tests and real adapters).
    pub fn with_gate(mut self, gate: Box<dyn TargetGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// `PreToolUse` with an explicit target scope (Go carries it in context).
    pub fn pre_tool_use_with_scope(
        &self,
        ev: &HookEvent,
        scope: Option<&TargetScope>,
    ) -> HookResult {
        let cmd = command_from_input(&ev.input);
        if self.kind != GuardKind::TargetOnly {
            self.record(&ev.tool_name, "allow", "", &cmd);
        }
        let gated = scope.map(|s| s.enabled).unwrap_or(false) && self.gate.is_some();
        let Some(decider) = &self.decider else {
            return if gated {
                self.apply_gate(self.gate.as_ref().unwrap(), ev)
            } else {
                HookResult::allow()
            };
        };

        // Target-only: built-in deny rules first, before the gate.
        if self.kind == GuardKind::TargetOnly {
            if let Some(dec) = match_first(&self.builtin, &ev.tool_name, &ev.input) {
                decider.log(&ev.tool_name, "denied");
                return self.block(&ev.tool_name, system_block_message(&dec.message));
            }
            if !gated {
                return HookResult::allow();
            }
            return self.apply_gate(self.gate.as_ref().unwrap(), ev);
        }

        // Full guard: rules, then judge only when no rule matched.
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
            // The gate applies even outside the enabled tool set.
            return if gated {
                self.apply_gate(self.gate.as_ref().unwrap(), ev)
            } else {
                HookResult::allow()
            };
        };
        match dec.action.as_str() {
            "deny" => {
                decider.log(&ev.tool_name, "denied");
                self.block(&ev.tool_name, system_block_message(&dec.message))
            }
            "allow" => {
                decider.log(&ev.tool_name, "allowed");
                if gated {
                    self.apply_gate(self.gate.as_ref().unwrap(), ev)
                } else {
                    HookResult::allow()
                }
            }
            "ask" => {
                let mut ask = true;
                if gated {
                    let gate = self.gate.as_ref().unwrap();
                    let (unpinned, res) = gate.check_targets(&ev.tool_name, &ev.input);
                    if !res.code.is_empty() {
                        return self.block(&ev.tool_name, target_block_message(&res));
                    }
                    if !unpinned.is_empty() {
                        let (res, merged) = self.request_targets(gate, ev, unpinned, ask);
                        if !res.code.is_empty() {
                            return self.block(&ev.tool_name, target_block_message(&res));
                        }
                        if merged {
                            ask = false;
                        }
                    }
                }
                if ask && !decider.handle_ask(&dec, &ev.tool_name, &ev.input) {
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

    fn apply_gate(&self, gate: &dyn TargetGate, ev: &HookEvent) -> HookResult {
        let (unpinned, res) = gate.check_targets(&ev.tool_name, &ev.input);
        if !res.code.is_empty() {
            return self.block(&ev.tool_name, target_block_message(&res));
        }
        if unpinned.is_empty() {
            return HookResult::allow();
        }
        let (res, _merged) = self.request_targets(gate, ev, unpinned, false);
        if !res.code.is_empty() {
            return self.block(&ev.tool_name, target_block_message(&res));
        }
        HookResult::allow()
    }

    fn request_targets(
        &self,
        gate: &dyn TargetGate,
        ev: &HookEvent,
        mut unpinned: Vec<TargetRequest>,
        mut ask: bool,
    ) -> (TargetResult, bool) {
        let mut merged = false;
        for _ in 0..TARGET_PASSES {
            let (res, was_merged) = gate.request_targets(&ev.tool_name, &ev.input, &unpinned, ask);
            if !res.code.is_empty() {
                return (res, merged);
            }
            if was_merged {
                merged = true;
                ask = false;
            }
            let (next, res) = gate.check_targets(&ev.tool_name, &ev.input);
            if !res.code.is_empty() || next.is_empty() {
                return (res, merged);
            }
            unpinned = next;
        }
        let t = &unpinned[0];
        (
            TargetResult {
                code: TARGET_APPROVAL_PENDING.to_string(),
                host: t.host.clone(), port: t.port,
                reason: format!("the target is still not covered by a pin after {TARGET_PASSES} approval rounds; its addresses changed again"),
            },
            merged,
        )
    }
}
