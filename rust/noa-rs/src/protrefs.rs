//! `protrefs.rs` — behavioral-parity Rust port of `noa.ComputeProtectedRefs` (and its
//! `isRenderedSummaryMessage` helper) from the external `norma/noa` package
//! (`recommend.go`, `prune.go`).
//!
//! The soft protected zone at the tail of the conversation is the union of two windows:
//! the last `PreserveRecentMessages` visible messages and a token-budget window walked
//! backwards until `PreserveRecentTokens` is reached. "Visible" excludes covered messages
//! (already represented by an active block), rendered-summary messages, blocked refs,
//! messages with no ref, and inherently re-obtainable tool output (`IsNeverPreserveRecentTool`).
//! The result set is returned sorted (Go returns an unordered `map[string]bool`).

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use crate::blocks::SUMMARY_HEADER;
use crate::port::{
    BLOCKED_REF, CT_TEXT, CoreMessage, ROLE_USER, SUMMARY_ID_PREFIX, default_count_tokens,
};
use crate::protection::is_never_preserve_recent_tool;
use crate::state::{StateBlock, covered_message_ids};

/// `noa.isRenderedSummaryMessage` — a synthetic summary message the model should not
/// treat as live conversation.
pub(crate) fn is_rendered_summary_message(m: &CoreMessage) -> bool {
    m.id.starts_with(SUMMARY_ID_PREFIX)
        && m.role == ROLE_USER
        && m.content_type == CT_TEXT
        && m.text.starts_with(SUMMARY_HEADER)
}

/// A message's position in the recency window. Built only for visible messages.
struct VisibleMsg {
    reference: String,
    tokens: i64,
    is_user: bool,
}

/// `noa.ComputeProtectedRefs` — the set of refs inside the soft protected tail zone.
///
/// `blocks` is `state.Blocks` (for `CoveredMessageIDs`); the ref lookup is passed
/// separately as `by_raw` (host id → ref or `BlockedRef`) to avoid pulling the whole
/// state model. Returns the protected refs sorted.
pub fn compute_protected_refs(
    msgs: &[CoreMessage],
    blocks: &[StateBlock],
    by_raw: &std::collections::HashMap<String, String>,
    preserve_recent_messages: i64,
    preserve_recent_tokens: i64,
) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let covered = covered_message_ids(blocks);

    let mut visible: Vec<VisibleMsg> = Vec::new();
    for m in msgs {
        if m.id.is_empty() || covered.contains(&m.id) || is_rendered_summary_message(m) {
            continue;
        }
        if m.text.starts_with(SUMMARY_HEADER) {
            continue;
        }
        let Some(reference) = by_raw.get(&m.id) else {
            continue;
        };
        if reference == BLOCKED_REF {
            continue;
        }
        // Bulky, inherently re-obtainable output does not occupy a recency slot.
        if is_never_preserve_recent_tool(&m.tool_name) {
            continue;
        }
        visible.push(VisibleMsg {
            reference: reference.clone(),
            tokens: default_count_tokens(&m.text),
            is_user: m.role == ROLE_USER,
        });
    }

    if preserve_recent_messages > 0 {
        let from = std::cmp::max(visible.len() as i64 - preserve_recent_messages, 0) as usize;
        for v in &visible[from..] {
            out.insert(v.reference.clone());
        }
    }
    if preserve_recent_tokens > 0 {
        let mut acc: i64 = 0;
        for v in visible.iter().rev() {
            if acc >= preserve_recent_tokens {
                break;
            }
            out.insert(v.reference.clone());
            acc += v.tokens;
        }
    }
    if preserve_recent_messages > 0 {
        // The most recent user message carries the live instruction; compressing it
        // changes the task.
        for v in visible.iter().rev() {
            if v.is_user {
                out.insert(v.reference.clone());
                break;
            }
        }
    }

    out
}
