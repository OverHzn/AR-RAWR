//! `recommend_build.rs` — behavioral-parity Rust port of `noa.BuildRecommendation`
//! from the external `norma/noa` package (`recommend.go`).
//!
//! This is the full pure recommendation pass: it composes `ComputeProtectedRefs`,
//! `BuildCompressibleRanges`, `MergeRangesToThreshold`, and `ViableRanges`. The
//! `TokenCountFn` callback is modeled as a `fn(&str) -> i64` (default `DefaultCountTokens`).
//! `CompressibleRanges` is a non-nil slice (`[]` when empty, from `ViableRanges`'s
//! `make(...,0,…)`); `ProtectedRanges` is a Go nil slice (`null` when empty).

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};

use crate::buildranges::build_compressible_ranges;
use crate::port::CoreMessage;
use crate::protrefs::compute_protected_refs;
use crate::ranges::ProtectedRange;
use crate::recommend::{RangeInfo, merge_ranges_to_threshold, viable_ranges};
use crate::state::StateBlock;

/// `noa.Recommendation` — the full recommendation pass output.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Recommendation {
    /// `CompressibleRanges` — viable compressible spans (non-nil; `[]` when empty).
    pub compressible_ranges: Vec<RangeInfo>,
    /// `ProtectedRanges` — protected runs (Go nil slice; `null` when empty).
    pub protected_ranges: Vec<ProtectedRange>,
    /// `NothingToCompress` — true iff no viable range survived.
    pub nothing_to_compress: bool,
}

/// The resolved config knobs `BuildRecommendation` reads, bundled to keep the call
/// signature small. Mirrors the Go `Config` fields actually consulted.
#[derive(Debug, Clone, Copy)]
pub struct RecConfig {
    pub min_compress_range: i64,
    pub preserve_recent_messages: i64,
    pub preserve_recent_tokens: i64,
}

/// `noa.BuildRecommendation` — the full recommendation pass.
///
/// `by_raw` is `state.MessageRefs.ByRaw`; `blocks` is `state.Blocks`. The protected-ref
/// set is computed internally (the caller need not pass it). `protected_refs` from the
/// earlier pass is recomputed here exactly as Go does.
pub fn build_recommendation(
    msgs: &[CoreMessage],
    blocks: &[StateBlock],
    by_raw: &HashMap<String, String>,
    protected_tools: &[String],
    cfg: RecConfig,
    count: fn(&str) -> i64,
) -> Recommendation {
    let protected_refs: BTreeSet<String> = compute_protected_refs(
        msgs,
        blocks,
        by_raw,
        cfg.preserve_recent_messages,
        cfg.preserve_recent_tokens,
    );
    let (ranges, protected_runs) = build_compressible_ranges(
        msgs,
        blocks,
        by_raw,
        protected_tools,
        &protected_refs,
        count,
    );
    // MergeRangesToThreshold returns a non-nil slice when it returns ranges unchanged
    // (min<=0 or empty), and a possibly-empty non-nil slice otherwise. We model the
    // result as a plain Vec (the nil-vs-[] distinction only matters for the final
    // CompressibleRanges, which ViableRanges always makes non-nil).
    let merged = merge_ranges_to_threshold(&ranges, cfg.min_compress_range).unwrap_or_default();
    let viable = viable_ranges(&merged);
    let nothing_to_compress = viable.is_empty();
    Recommendation {
        compressible_ranges: viable,
        protected_ranges: protected_runs,
        nothing_to_compress,
    }
}
