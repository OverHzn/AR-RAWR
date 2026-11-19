//! `permission-rs` — behavioral-parity Rust port of the *pure* decision logic of the
//! external `github.com/Autumn-27/norma/permission` package.
//!
//! `permission` is the agent's safety boundary: it models tool-call decisions
//! (allow / deny / ask), six permission modes, allow/deny rule lists with
//! deny-priority, and the four-stage `Evaluate` pipeline that combines a tool's own
//! verdict with policy + mode.
//!
//! The only impure input to `Evaluate` is `Request.Ask` — the host's interactive
//! approval *callback* (`func(ctx, toolName, input, pc) Decision`). The Go `ctx
//! context.Context` argument is threaded *only* into that callback and never read
//! otherwise, so we model the callback by its **already-resolved return value**:
//! `Request::ask = Some(decision)` means "the host callback is configured and would
//! return this", `None` means "no callback configured" (Go `req.Ask == nil`). With
//! that substitution the entire deny-priority pipeline is a pure, deterministic
//! function of value-typed inputs, which is what this crate ports and proves.
//!
//! The package imports only the standard library, so the parity oracle is a small
//! standalone Go module (no module-cache / `llm-rs` entanglement).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

// `permission.Behavior` values.
pub const BEHAVIOR_ALLOW: &str = "allow";
pub const BEHAVIOR_DENY: &str = "deny";
pub const BEHAVIOR_ASK: &str = "ask";

// `permission.Mode` values.
pub const MODE_DEFAULT: &str = "default";
pub const MODE_ACCEPT_EDITS: &str = "acceptEdits";
pub const MODE_BYPASS: &str = "bypassPermissions";
pub const MODE_PLAN: &str = "plan";
pub const MODE_DONT_ASK: &str = "dontAsk";
pub const MODE_AUTO: &str = "auto";

/// `permission.Update`: a persistence suggestion ("always allow this") surfaced to the
/// host. Go json tags are lowercase `type` / `rule`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Update {
    #[serde(rename = "type")]
    pub update_type: String,
    #[serde(rename = "rule")]
    pub rule: String,
}

/// `permission.Decision`: the outcome of a permission check.
///
/// Go declares this struct with **no json tags**, so it marshals with its Go field
/// names (`Behavior`, `UpdatedInput`, `Message`, `Suggestions`) — reproduced here with
/// explicit `rename`s so recorded `Decision` JSON round-trips byte-identically.
/// `UpdatedInput` (Go `json.RawMessage`) is `null` when unset; `Suggestions` (Go nil
/// slice) is `null` when unset.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Decision {
    #[serde(rename = "Behavior", default)]
    pub behavior: String,
    #[serde(rename = "UpdatedInput", default)]
    pub updated_input: Option<Value>,
    #[serde(rename = "Message", default)]
    pub message: String,
    #[serde(rename = "Suggestions", default)]
    pub suggestions: Option<Vec<Update>>,
}

impl Decision {
    fn new(behavior: &str) -> Self {
        Decision {
            behavior: behavior.to_string(),
            ..Decision::default()
        }
    }
}

/// `permission.Allowed()` — allow with no message.
pub fn allowed() -> Decision {
    Decision::new(BEHAVIOR_ALLOW)
}

/// `permission.Denied(msg)` — deny with a reason.
pub fn denied(msg: &str) -> Decision {
    let mut d = Decision::new(BEHAVIOR_DENY);
    d.message = msg.to_string();
    d
}

/// `permission.AskUser(msg)` — ask the user with a prompt.
pub fn ask_user(msg: &str) -> Decision {
    let mut d = Decision::new(BEHAVIOR_ASK);
    d.message = msg.to_string();
    d
}

/// `permission.Request`: everything `Evaluate` needs, without importing the tool
/// package. `ask` models the host callback by its resolved return value (see the
/// module docs); `working_dir` (Go `Context.WorkingDir`) is omitted because the pure
/// pipeline never reads it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Request {
    pub tool_name: String,
    pub input: Option<Value>,
    pub is_read_only: bool,
    pub tool_decision: Decision,
    pub mode: String,
    pub allowed: Vec<String>,
    pub disallowed: Vec<String>,
    pub ask: Option<Decision>,
}

/// `permission.matchRule` (unexported; exercised transitively through `evaluate`):
/// whether `tool_name` matches any rule. A rule is a bare tool name, `"*"` (any), or
/// `"Name(...)"` whose `Name` part (the bytes before the first `(`, when that index is
/// > 0) equals the tool name. Each rule is trimmed of surrounding whitespace first.
pub fn match_rule(rules: &[String], tool_name: &str) -> bool {
    for rule in rules {
        let r = rule.trim();
        if r == "*" || r == tool_name {
            return true;
        }
        if let Some(i) = r.find('(')
            && i > 0
            && &r[..i] == tool_name
        {
            return true;
        }
    }
    false
}

/// `permission`'s `editTools` set (auto-allowed under `ModeAcceptEdits`).
fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Write" | "Edit" | "NotebookEdit")
}

/// `permission.Evaluate` — the four-stage deny-priority pipeline. Returns the final
/// `Decision`. Order encodes deny-priority: an explicit deny rule (or a tool
/// hard-deny) wins over every mode, including bypass.
pub fn evaluate(req: &Request) -> Decision {
    // Stage 1 — deny rules (absolute priority) and tool hard-deny.
    if match_rule(&req.disallowed, &req.tool_name) {
        return denied(&format!(
            "denied: tool '{}' is disallowed by policy",
            req.tool_name
        ));
    }
    if req.tool_decision.behavior == BEHAVIOR_DENY {
        return req.tool_decision.clone();
    }

    // Stage 2 — mode short-circuits.
    if req.mode == MODE_BYPASS {
        return allowed();
    }
    if req.mode == MODE_PLAN {
        if !req.is_read_only {
            return denied("denied: plan mode permits read-only tools only");
        }
        return allowed();
    }

    // Stage 3 — allow rules and mode-based auto-allows.
    if match_rule(&req.allowed, &req.tool_name) {
        return allowed();
    }
    if req.mode == MODE_ACCEPT_EDITS && is_edit_tool(&req.tool_name) {
        return allowed();
    }
    if req.is_read_only {
        return allowed();
    }
    if req.tool_decision.behavior == BEHAVIOR_ALLOW {
        return req.tool_decision.clone();
    }

    // Stage 4 — interactive confirmation (only if the mode permits prompting).
    if req.mode == MODE_DONT_ASK {
        return denied(&format!(
            "denied: '{}' requires approval but mode is dontAsk",
            req.tool_name
        ));
    }
    if req.mode == MODE_AUTO {
        return denied(&format!(
            "denied: '{}' not permitted by rules in auto mode",
            req.tool_name
        ));
    }
    if let Some(d) = &req.ask {
        if d.behavior.is_empty() {
            return denied("denied by approval callback");
        }
        return d.clone();
    }
    denied(&format!(
        "denied: no approval handler configured for '{}'",
        req.tool_name
    ))
}
