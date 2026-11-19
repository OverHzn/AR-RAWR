//! `state.rs` — behavioral-parity Rust port of the read-only compression-state queries
//! from the external `norma/noa` package (`state.go`, `recommend.go`): `ActiveBlocks`,
//! `CoveredMessageIDs`, `ActiveBlockSpans`.
//!
//! These never mutate the state; they filter/aggregate the block list. `ActiveBlocks`
//! preserves input order; `CoveredMessageIDs` is the deduplicated union of active
//! blocks' effective coverage (returned sorted here, since Go's `map[string]bool` has
//! no order); `ActiveBlockSpans` sorts active blocks by start ref index (stable).

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use crate::nudgetext::BlockSpan;
use crate::port::ref_to_index;

/// The subset of `noa.CompressionBlock` that the state queries read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StateBlock {
    pub block_id: String,
    pub tier: i64,
    pub start_ref: String,
    pub end_ref: String,
    pub active: bool,
    pub effective_message_ids: Vec<String>,
}

/// `noa.ActiveBlocks` — the blocks currently standing in for content (input order).
pub fn active_blocks(blocks: &[StateBlock]) -> Vec<StateBlock> {
    blocks.iter().filter(|b| b.active).cloned().collect()
}

/// `noa.CoveredMessageIDs` — the union of every active block's effective coverage.
/// Returned sorted (Go returns an unordered `map[string]bool`).
pub fn covered_message_ids(blocks: &[StateBlock]) -> BTreeSet<String> {
    let mut covered = BTreeSet::new();
    for b in blocks {
        if !b.active {
            continue;
        }
        for id in &b.effective_message_ids {
            covered.insert(id.clone());
        }
    }
    covered
}

/// `noa.ActiveBlockSpans` — active blocks as spans, sorted by start ref index (stable).
pub fn active_block_spans(blocks: &[StateBlock]) -> Vec<BlockSpan> {
    let mut out: Vec<BlockSpan> = blocks
        .iter()
        .filter(|b| b.active)
        .map(|b| BlockSpan {
            block_id: b.block_id.clone(),
            tier: b.tier,
            start_ref: b.start_ref.clone(),
            end_ref: b.end_ref.clone(),
        })
        .collect();
    // Stable sort by start ref index (Go sort.SliceStable).
    out.sort_by_key(|s| ref_to_index(&s.start_ref).0);
    out
}
