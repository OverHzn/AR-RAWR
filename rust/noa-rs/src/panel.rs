//! `panel.rs` — behavioral-parity Rust port of `noa.FormatPanel` (the Compress tool's
//! result renderer) and its `spanHasUncovered` helper from the external `norma/noa`
//! package (`panel.go`).
//!
//! The panel reports the span each created block ACTUALLY covers (its effective
//! coverage), flags a span with a trailing `*` when it leaves live refs uncovered
//! (the protected zone or a higher-tier block), and lists surviving higher-tier blocks
//! that were not absorbed. Pure and deterministic: the only map walk
//! (`state.message_refs.by_ref`) feeds an order-independent OR.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use crate::port::{format_tokens, ref_to_index};

/// The subset of `noa.CompressionBlock` that `FormatPanel` / `spanHasUncovered` read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PanelBlock {
    pub block_id: String,
    pub tier: i64,
    pub start_ref: String,
    pub end_ref: String,
    pub archive_path: String,
    pub effective_message_ids: Vec<String>,
}

/// The subset of `noa.CompressionState` that `FormatPanel` reads: the post-compression
/// blocks (for `FindBlock`) and the ref→rawID index (for `spanHasUncovered`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PanelState {
    /// `(blockID, tier)` for every block in `state.Blocks` (FindBlock reads only these).
    pub blocks: Vec<(String, i64)>,
    /// `state.MessageRefs.ByRef`: ref → rawID.
    pub by_ref: HashMap<String, String>,
}

/// The subset of `noa.ApplyResult` that `FormatPanel` reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ApplyResult {
    pub blocks_created: Vec<PanelBlock>,
    pub surviving_block_ids: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// `noa.FormatPanel` — render the Compress tool's result panel.
pub fn format_panel(before: i64, after: i64, r: &ApplyResult, state: &PanelState) -> String {
    let mut b = String::new();

    if r.blocks_created.is_empty() {
        b.push_str("▣ noa | 0 blocks created");
        if !r.errors.is_empty() {
            b.push('\n');
            for e in &r.errors {
                b.push_str(&format!("  error: {e}\n"));
            }
        }
        for w in &r.warnings {
            b.push_str(&format!("  warn: {w}\n"));
        }
        return b.trim_end_matches('\n').to_string();
    }

    let reclaimed = before - after;
    b.push_str(&format!(
        "▣ noa | {} → {} tokens (~{} reclaimed)\n",
        format_tokens(before),
        format_tokens(after),
        format_tokens(std::cmp::max(reclaimed, 0))
    ));

    // covered = union of every created block's effective message ids.
    let mut covered: HashSet<&str> = HashSet::new();
    for blk in &r.blocks_created {
        for id in &blk.effective_message_ids {
            covered.insert(id);
        }
    }
    for blk in &r.blocks_created {
        let star = if span_has_uncovered(blk, state, &covered) {
            "*"
        } else {
            ""
        };
        b.push_str(&format!(
            "  {}(T{})={}–{}{} → {}\n",
            blk.block_id, blk.tier, blk.start_ref, blk.end_ref, star, blk.archive_path
        ));
    }

    if !r.surviving_block_ids.is_empty() {
        let parts: Vec<String> = r
            .surviving_block_ids
            .iter()
            .map(|id| match state.blocks.iter().find(|(bid, _)| bid == id) {
                Some((_, tier)) => format!("{id}(T{tier})"),
                None => id.clone(),
            })
            .collect();
        b.push_str(&format!(
            "  note: {} was inside the requested range but was not consumed\n",
            parts.join(", ")
        ));
        b.push_str(
            "        (only the lowest tier present is absorbed). Compress that layer separately.\n",
        );
    }
    for e in &r.errors {
        b.push_str(&format!("  error: {e}\n"));
    }
    for w in &r.warnings {
        b.push_str(&format!("  warn: {w}\n"));
    }
    b.trim_end_matches('\n').to_string()
}

/// `noa.spanHasUncovered` — whether the block's ref span contains a ref that is still
/// live and not covered by this batch. Unparseable endpoints yield false.
fn span_has_uncovered(blk: &PanelBlock, state: &PanelState, covered: &HashSet<&str>) -> bool {
    let (lo, ok_lo) = ref_to_index(&blk.start_ref);
    let (hi, ok_hi) = ref_to_index(&blk.end_ref);
    if !ok_lo || !ok_hi {
        return false;
    }
    for (r, raw_id) in &state.by_ref {
        let (n, ok) = ref_to_index(r);
        if !ok || n < lo || n > hi {
            continue;
        }
        if !covered.contains(raw_id.as_str()) {
            return true;
        }
    }
    false
}
