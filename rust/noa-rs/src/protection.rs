//! `protection.rs` — behavioral-parity Rust port of the tool-protection predicates
//! from the external `norma/noa` package (`protected.go`): `MatchToolPattern`,
//! `IsNeverPreserveRecentTool`, `IsMessageProtected`, plus the
//! `AlwaysProtectedTools` / `NeverPreserveRecentTools` tables.
//!
//! These decide which tools are HARD-excluded from every compression range. The Go
//! `Config.IsToolProtected func(toolName string) bool` host predicate is modeled by
//! its already-resolved accept-set (`Option<&[String]>`): a tool is "host protected"
//! iff it appears in that set. `ctx` is not involved, so the whole predicate stays
//! pure and deterministic.

#![forbid(unsafe_code)]

use crate::port::COMPRESS_TOOL_NAME;

/// `noa.AlwaysProtectedTools` — tools never compressible regardless of config.
/// (`[]string{CompressToolName}`.)
pub const ALWAYS_PROTECTED_TOOLS: &[&str] = &[COMPRESS_TOOL_NAME];

/// `noa.NeverPreserveRecentTools` — tools excluded from the "most recent N"
/// protected-zone accounting (case-insensitive).
pub const NEVER_PRESERVE_RECENT_TOOLS: &[&str] = &["Read", "Grep", "Glob", "Bash", "LS"];

/// `noa.MatchToolPattern` — match a tool name against a configured pattern: exact
/// (case-insensitive), or a single trailing `*` prefix wildcard.
pub fn match_tool_pattern(pattern: &str, tool_name: &str) -> bool {
    if pattern.is_empty() || tool_name.is_empty() {
        return false;
    }
    let p = pattern.to_lowercase();
    let n = tool_name.to_lowercase();
    if let Some(prefix) = p.strip_suffix('*') {
        return n.starts_with(prefix);
    }
    p == n
}

/// `noa.IsNeverPreserveRecentTool` — whether a message's tool is excluded from the
/// "most recent N" protected-zone accounting (case-insensitive against
/// `NEVER_PRESERVE_RECENT_TOOLS`).
pub fn is_never_preserve_recent_tool(tool_name: &str) -> bool {
    if tool_name.is_empty() {
        return false;
    }
    NEVER_PRESERVE_RECENT_TOOLS
        .iter()
        .any(|n| n.eq_ignore_ascii_case(tool_name))
}

/// `noa.IsMessageProtected` — whether a message is HARD-protected: excluded from
/// every compression range and never assigned a ref.
///
/// `protected_tools` is `cfg.ProtectedTools` (pattern list). `host_protected` models
/// the resolved `cfg.IsToolProtected` predicate as the set of tool names it accepts
/// (`None` when no predicate is set).
pub fn is_message_protected(
    tool_name: &str,
    protected_tools: &[String],
    host_protected: Option<&[String]>,
) -> bool {
    if tool_name.is_empty() {
        return false;
    }
    // AlwaysProtectedTools membership is exact (Go slices.Contains, case-sensitive).
    if ALWAYS_PROTECTED_TOOLS.contains(&tool_name) {
        return true;
    }
    for pat in protected_tools {
        if match_tool_pattern(pat, tool_name) {
            return true;
        }
    }
    if let Some(hp) = host_protected
        && hp.iter().any(|t| t == tool_name)
    {
        return true;
    }
    false
}
