//! Rust port of the host-side value surface of the external
//! `norma/permission` package.
//!
//! The pure decision pipeline (`Evaluate`, `Decision`, `Allowed`/`Denied`/
//! `AskUser`, rule matching, the mode ladder) is parity-tested in `permission-rs`
//! and re-exported here so `artex` builds against `crate::permission`. The one
//! piece `permission-rs` deliberately omits is the [`Context`] input value (it
//! models `Evaluate` by the already-resolved `Request` instead); [`CoreTool`]'s
//! permission self-check still takes it, so it is defined here.

// Re-export the pure decision logic from the parity crate.
pub use permission_rs::port::{
    allowed, ask_user, denied, evaluate, match_rule, Decision, Request, Update, BEHAVIOR_ALLOW,
    BEHAVIOR_ASK, BEHAVIOR_DENY, MODE_ACCEPT_EDITS, MODE_AUTO, MODE_BYPASS, MODE_DEFAULT,
    MODE_DONT_ASK, MODE_PLAN,
};

/// `permission.Mode` string type.
pub type Mode = String;

/// `permission.Context`: the host permission state threaded into a tool's
/// `CheckPermissions` self-check. `permission-rs` omits it (its pure `Evaluate`
/// takes a flat `Request`); tools still receive it, so it lives here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    /// `Context.Mode`.
    pub mode: Mode,
    /// `Context.WorkingDir`.
    pub working_dir: String,
    /// `Context.Allowed` — allow rules.
    pub allowed: Vec<String>,
    /// `Context.Disallowed` — deny rules (absolute priority).
    pub disallowed: Vec<String>,
}
