//! `buildranges.rs` — behavioral-parity Rust port of `noa.BuildCompressibleRanges` (and
//! its `groupCompressible` helper) from the external `norma/noa` package (`recommend.go`).
//!
//! The function walks the message view once, grouping contiguous compressible messages
//! into `RangeInfo` spans and breaking the run wherever a protected message, a protected
//! ref, a covered/summary message, or a user message (after ≥3) interrupts it. Protected
//! runs accumulate into `ProtectedRange` entries. The `TokenCountFn count` callback is
//! modeled as a plain `fn(&str) -> i64` (the default `DefaultCountTokens` is the only
//! deterministic path). The `Config.IsToolProtected` host predicate is modeled as resolved
//! (`None` here). Both outputs are Go nil slices when empty (rendered as JSON `null`).

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::blocks::SUMMARY_HEADER;
use crate::port::{
    BLOCKED_REF, CT_TOOL_CALL, CT_TOOL_RESULT, CoreMessage, ROLE_USER, round_half_up,
};
use crate::protection::is_message_protected;
use crate::protrefs::is_rendered_summary_message;
use crate::ranges::ProtectedRange;
use crate::recommend::RangeInfo;
use crate::state::{StateBlock, covered_message_ids};

/// One scanned compressible message (the internal `scanned` struct).
struct Scanned {
    reference: String,
    tokens: i64,
    chars: i64,
    is_tool: bool,
    is_user: bool,
    gap_before: bool,
}

/// `noa.BuildCompressibleRanges` — group compressible messages into ranges and collect
/// protected runs. `by_raw` is `state.MessageRefs.ByRaw`; `blocks` is `state.Blocks`
/// (for `CoveredMessageIDs`); `protected_refs` is the resolved protected-ref set.
pub fn build_compressible_ranges(
    msgs: &[CoreMessage],
    blocks: &[StateBlock],
    by_raw: &HashMap<String, String>,
    protected_tools: &[String],
    protected_refs: &BTreeSet<String>,
    count: fn(&str) -> i64,
) -> (Vec<RangeInfo>, Vec<ProtectedRange>) {
    let covered = covered_message_ids(blocks);
    let protected_call_ids = collect_protected_tool_call_ids(msgs, protected_tools);

    let mut compressible: Vec<Scanned> = Vec::new();
    let mut protected_runs: Vec<ProtectedRange> = Vec::new();
    let mut cur_protected: Option<ProtectedRange> = None;

    let mut gap_since_compressible = false;

    // flush pushes the in-progress protected run (if any) and resets it.
    fn flush_protected(runs: &mut Vec<ProtectedRange>, cur: &mut Option<ProtectedRange>) {
        if let Some(c) = cur.take() {
            runs.push(c);
        }
    }

    for m in msgs {
        let Some(reference) = by_raw.get(&m.id) else {
            // No ref: unaddressable, interrupts nothing.
            continue;
        };
        if m.id.is_empty() || reference == BLOCKED_REF {
            continue;
        }
        let reference = reference.clone();

        if covered.contains(&m.id)
            || is_rendered_summary_message(m)
            || m.text.starts_with(SUMMARY_HEADER)
        {
            gap_since_compressible = true;
            flush_protected(&mut protected_runs, &mut cur_protected);
        } else if is_message_protected_with_pairing(m, protected_tools, &protected_call_ids) {
            gap_since_compressible = true;
            let tk = count(&m.text);
            match cur_protected.as_mut() {
                None => {
                    cur_protected = Some(ProtectedRange {
                        start_ref: reference.clone(),
                        end_ref: reference,
                        count: 1,
                        tokens: tk,
                        tools: Vec::new(),
                    });
                }
                Some(cp) => {
                    cp.end_ref = reference;
                    cp.count += 1;
                    cp.tokens += tk;
                }
            }
            if !m.tool_name.is_empty()
                && let Some(cp) = cur_protected.as_mut()
                && !cp.tools.iter().any(|t| t == &m.tool_name)
            {
                cp.tools.push(m.tool_name.clone());
            }
        } else if protected_refs.contains(&reference) {
            gap_since_compressible = true;
            flush_protected(&mut protected_runs, &mut cur_protected);
        } else {
            flush_protected(&mut protected_runs, &mut cur_protected);
            compressible.push(Scanned {
                reference,
                tokens: count(&m.text),
                chars: m.text.chars().count() as i64,
                is_tool: m.content_type == CT_TOOL_CALL || m.content_type == CT_TOOL_RESULT,
                is_user: m.role == ROLE_USER,
                gap_before: gap_since_compressible,
            });
            gap_since_compressible = false;
        }
    }
    flush_protected(&mut protected_runs, &mut cur_protected);

    (group_compressible(compressible), protected_runs)
}

/// `noa.isMessageProtectedWithPairing` — hard protection, extended to a tool_result whose
/// originating tool_call is protected.
fn is_message_protected_with_pairing(
    m: &CoreMessage,
    protected_tools: &[String],
    protected_call_ids: &HashSet<String>,
) -> bool {
    if is_message_protected(&m.tool_name, protected_tools, None) {
        return true;
    }
    !m.tool_call_id.is_empty() && protected_call_ids.contains(&m.tool_call_id)
}

/// `noa.collectProtectedToolCallIDs` — the call IDs of every protected tool-call message.
fn collect_protected_tool_call_ids(
    msgs: &[CoreMessage],
    protected_tools: &[String],
) -> HashSet<String> {
    let mut ids = HashSet::new();
    for m in msgs {
        if !m.tool_call_id.is_empty() && is_message_protected(&m.tool_name, protected_tools, None) {
            ids.insert(m.tool_call_id.clone());
        }
    }
    ids
}

/// `noa.groupCompressible` — group contiguous scanned messages into `RangeInfo` spans,
/// breaking a run at a leading gap or a user message once the run already has ≥3 messages.
fn group_compressible(items: Vec<Scanned>) -> Vec<RangeInfo> {
    let mut out: Vec<RangeInfo> = Vec::new();
    let mut cur: Option<RangeInfo> = None;
    let mut cur_tool_count: i64 = 0;

    for it in items {
        let should_break = cur
            .as_ref()
            .is_some_and(|c| it.gap_before || (it.is_user && c.count >= 3));
        if should_break {
            flush_range(&mut out, &mut cur, &mut cur_tool_count);
        }
        if cur.is_none() {
            cur = Some(RangeInfo {
                start_ref: it.reference.clone(),
                end_ref: it.reference.clone(),
                ..RangeInfo::default()
            });
        }
        if let Some(c) = cur.as_mut() {
            c.end_ref = it.reference.clone();
            c.count += 1;
            c.tokens += it.tokens;
            c.chars += it.chars;
        }
        if it.is_tool {
            cur_tool_count += 1;
        }
        if let Some(c) = cur.as_mut()
            && it.is_user
        {
            c.user_msgs += 1;
        }
    }
    flush_range(&mut out, &mut cur, &mut cur_tool_count);
    out
}

/// `groupCompressible`'s `flush`: finalize the in-progress span's tool/text split.
fn flush_range(out: &mut Vec<RangeInfo>, cur: &mut Option<RangeInfo>, cur_tool_count: &mut i64) {
    if let Some(mut c) = cur.take() {
        if c.count > 0 {
            c.tool_pct = round_half_up(*cur_tool_count as f64 * 100.0 / c.count as f64);
            c.text_pct = 100 - c.tool_pct;
        }
        out.push(c);
    }
    *cur_tool_count = 0;
}
