//! `nudgetext.rs` — behavioral-parity Rust port of the pure string-formatting helpers
//! from the external `norma/noa` package's nudge renderer (`nudgetext.go`):
//! `FormatBreakdown`, `FormatBlockMap`, `FormatTierTargetBlocks`.
//!
//! These turn structured nudge diagnostics into the text shown to the model. They
//! are pure functions of value-typed inputs. `FormatRanges` (the merge-heavy range
//! list) is intentionally deferred — its merged-range struct is large and the merge
//! logic is exercised more naturally through the higher-level recommendation path.

#![forbid(unsafe_code)]

use crate::port::{default_count_tokens, format_tokens};

/// `noa.blockMapMaxShown` — bounds how many block lines the block map shows.
const BLOCK_MAP_MAX_SHOWN: usize = 8;

/// `noa.BlockSpan` — a block's tier and ref span (subset used by `FormatBlockMap`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BlockSpan {
    pub block_id: String,
    pub tier: i64,
    pub start_ref: String,
    pub end_ref: String,
}

/// `noa.FormatBreakdown` — render the context breakdown line (system/tool/summaries/
/// code/text buckets in that fixed order, non-zero only) plus an optional growth line.
pub fn format_breakdown(context_breakdown: &[(String, i64)], growth: i64) -> String {
    const ORDER: [&str; 5] = ["system", "tool", "summaries", "code", "text"];
    let mut parts: Vec<String> = Vec::new();
    for k in ORDER {
        // The map has one value per key; look it up.
        if let Some(&n) = context_breakdown
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v)
            && n > 0
        {
            parts.push(format!("{} {}", format_tokens(n), k));
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    let mut out = format!("Context breakdown: {}", parts.join(" | "));
    if growth > 0 {
        out += &format!("\n+{} since last nudge", format_tokens(growth));
    }
    out
}

/// `noa.FormatBlockMap` — render the active-block map, oldest blocks collapsed into a
/// `…+N older` prefix when more than `BLOCK_MAP_MAX_SHOWN` spans exist.
pub fn format_block_map(spans: &[BlockSpan]) -> String {
    if spans.is_empty() {
        return String::new();
    }
    let hidden = spans.len().saturating_sub(BLOCK_MAP_MAX_SHOWN);
    let shown: &[BlockSpan] = if hidden > 0 {
        &spans[spans.len() - BLOCK_MAP_MAX_SHOWN..]
    } else {
        spans
    };
    let items: Vec<String> = shown
        .iter()
        .map(|s| format!("{}(T{})={}–{}", s.block_id, s.tier, s.start_ref, s.end_ref))
        .collect();
    let prefix = if hidden > 0 {
        format!("…+{} older · ", hidden)
    } else {
        String::new()
    };
    format!(
        "Active blocks ({}): {}{}",
        spans.len(),
        prefix,
        items.join(" · ")
    )
}

/// `noa.CompressionBlock` subset used by `FormatTierTargetBlocks`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TierTargetBlock {
    pub block_id: String,
    pub tier: i64,
    pub topic: String,
    pub summary: String,
    pub effective_message_ids: Vec<String>,
    pub compressed_tokens: i64,
}

/// `noa.FormatTierTargetBlocks` — list what a tier nudge asks to consolidate: each
/// block's id, message count, token delta (`compressed→summary`), and optional quoted
/// topic.
pub fn format_tier_target_blocks(blocks: &[TierTargetBlock]) -> String {
    if blocks.is_empty() {
        return "Target blocks: (none — no tier blocks found)".to_string();
    }
    let mut lines: Vec<String> = Vec::with_capacity(blocks.len());
    for b in blocks {
        let summary_tokens = default_count_tokens(&b.summary);
        let topic = if b.topic.is_empty() {
            String::new()
        } else {
            // Go %q double-quotes and escapes; for the ASCII topics in the corpus a
            // simple quoted form matches. Use Rust's {:?} on the string for parity
            // with Go's %q on these inputs.
            format!("  {:?}", b.topic)
        };
        lines.push(format!(
            "  {}  {} msgs  {}→{}{}",
            b.block_id,
            b.effective_message_ids.len(),
            format_tokens(b.compressed_tokens),
            format_tokens(summary_tokens),
            topic
        ));
    }
    format!(
        "Target tier-{} blocks to distill ({}):\n{}",
        blocks[0].tier,
        blocks.len(),
        lines.join("\n")
    )
}
