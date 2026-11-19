//! `tier.rs` — behavioral-parity Rust port of `noa.DecideTier` and
//! `noa.IsTerminalRewrite` from the external `norma/noa` package (`tier.go`).
//!
//! `DecideTier` classifies one resolved range: a pure message range always yields a
//! tier-1 block; a block range targets the LOWEST active tier present (absorbing only
//! those blocks, leaving higher tiers to survive) and outputs one tier above, capped at
//! `MaxTier`. `IsTerminalRewrite` reports whether the target tier is already the terminal
//! level and no new direct messages arrived. Both are pure and deterministic.
//!
//! Go's `[]string` nil-vs-empty distinction is preserved: a decision that consumes or
//! survives nothing renders those fields as JSON `null` (handled by the parity harness),
//! mirroring Go's nil slice.

#![forbid(unsafe_code)]

use crate::state::StateBlock;

/// `noa.BoundaryKind` — which kind of span a `ResolvedRange` covers.
pub const BOUNDARY_MESSAGE: &str = "message";
/// `noa.BoundaryBlock`.
pub const BOUNDARY_BLOCK: &str = "block";

/// `noa.TierDecision` — the outcome of classifying one resolved range.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TierDecision {
    pub target_tier: i64,
    pub output_tier: i64,
    /// Blocks this compression absorbs (those at `target_tier`). Empty = Go nil.
    pub consumed_block_ids: Vec<String>,
    /// Blocks inside the range NOT absorbed (above `target_tier`). Empty = Go nil.
    pub surviving_block_ids: Vec<String>,
}

/// `noa.DecideTier` — classify a resolved range. `blocks` is `state.Blocks`;
/// `FindBlock` returns the first block with a matching id.
pub fn decide_tier(
    kind: &str,
    nested_block_ids: &[String],
    max_tier_cfg: i64,
    blocks: &[StateBlock],
) -> TierDecision {
    let max_tier = if max_tier_cfg < 2 { 3 } else { max_tier_cfg };

    // A pure message range always produces a tier-1 block.
    let mut d = TierDecision {
        target_tier: 1,
        output_tier: 1,
        ..TierDecision::default()
    };
    if kind != BOUNDARY_BLOCK {
        return d;
    }

    let mut lowest: i64 = 0;
    for id in nested_block_ids {
        let Some(b) = find_block(blocks, id) else {
            continue;
        };
        if !b.active {
            continue;
        }
        if lowest == 0 || b.tier < lowest {
            lowest = b.tier;
        }
    }
    if lowest > 0 {
        d.target_tier = lowest;
    }
    d.output_tier = std::cmp::min(max_tier, d.target_tier + 1);

    for id in nested_block_ids {
        let Some(b) = find_block(blocks, id) else {
            continue;
        };
        if !b.active {
            continue;
        }
        if b.tier == d.target_tier {
            d.consumed_block_ids.push(id.clone());
        } else {
            d.surviving_block_ids.push(id.clone());
        }
    }
    d
}

/// `noa.IsTerminalRewrite` — whether the target tier is already terminal and no new
/// direct messages arrived.
pub fn is_terminal_rewrite(target_tier: i64, direct_count: i64, max_tier_cfg: i64) -> bool {
    let max_tier = if max_tier_cfg < 2 { 3 } else { max_tier_cfg };
    target_tier == max_tier && direct_count == 0
}

/// `noa.FindBlock` — the first block in `state.Blocks` whose `BlockID` equals `id`.
fn find_block<'a>(blocks: &'a [StateBlock], id: &str) -> Option<&'a StateBlock> {
    blocks.iter().find(|b| b.block_id == id)
}
