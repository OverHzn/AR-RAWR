//! `recommend.rs` — behavioral-parity Rust port of the range-merging helpers from the
//! external `norma/noa` package (`recommend.go`): `MergeRangesToThreshold` and
//! `ViableRanges`. Both are pure functions over `[]RangeInfo`.
//!
//! `MergeRangesToThreshold` greedily merges adjacent ranges until each reaches a
//! character threshold, recomputing the weighted tool/text percentage of each merged
//! span. `ViableRanges` drops spans too small to be worth a compression turn
//! (`< ViableRangeMinTokens` tokens).

#![forbid(unsafe_code)]

use crate::port::round_half_up;
use serde::{Deserialize, Serialize};

/// `noa.ViableRangeMinTokens` — spans below this token count are filtered out.
pub const VIABLE_RANGE_MIN_TOKENS: i64 = 200;

/// `noa.RangeInfo` — a compressible/protected span with composition percentages.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RangeInfo {
    pub start_ref: String,
    pub end_ref: String,
    pub count: i64,
    pub tokens: i64,
    pub chars: i64,
    pub tool_pct: i64,
    pub text_pct: i64,
    pub user_msgs: i64,
    pub dangerous: bool,
}

/// `noa.MergeRangesToThreshold` — greedily merge adjacent ranges until each reaches
/// `min_chars`, recomputing each merged span's weighted tool/text split. A
/// non-positive `min_chars` or empty input returns the input unchanged (`None` for
/// empty, mirroring Go's nil slice).
pub fn merge_ranges_to_threshold(ranges: &[RangeInfo], min_chars: i64) -> Option<Vec<RangeInfo>> {
    if min_chars <= 0 || ranges.is_empty() {
        // Go returns `ranges` as-is: nil -> null, non-nil -> itself.
        return if ranges.is_empty() {
            None
        } else {
            Some(ranges.to_vec())
        };
    }

    // flush pushes `cur` (if any) into `out` with its recomputed tool/text split,
    // then resets the accumulators. Returns nothing; mutates via &mut refs.
    fn flush(
        out: &mut Vec<RangeInfo>,
        cur: &mut Option<RangeInfo>,
        weighted_tool: &mut i64,
        total_count: &mut i64,
    ) {
        if let Some(mut c) = cur.take() {
            if *total_count > 0 {
                c.tool_pct = round_half_up(*weighted_tool as f64 / *total_count as f64);
                c.text_pct = 100 - c.tool_pct;
            }
            out.push(c);
            *weighted_tool = 0;
            *total_count = 0;
        }
    }

    let mut out: Vec<RangeInfo> = Vec::new();
    let mut cur: Option<RangeInfo> = None;
    let mut weighted_tool: i64 = 0;
    let mut total_count: i64 = 0;

    for r in ranges {
        match cur.as_mut() {
            None => {
                cur = Some(r.clone());
                weighted_tool = r.tool_pct * r.count;
                total_count = r.count;
            }
            Some(c) => {
                c.end_ref = r.end_ref.clone();
                c.count += r.count;
                c.tokens += r.tokens;
                c.chars += r.chars;
                c.user_msgs += r.user_msgs;
                weighted_tool += r.tool_pct * r.count;
                total_count += r.count;
            }
        }
        if cur.as_ref().is_some_and(|c| c.chars >= min_chars) {
            flush(&mut out, &mut cur, &mut weighted_tool, &mut total_count);
        }
    }
    flush(&mut out, &mut cur, &mut weighted_tool, &mut total_count);

    if out.is_empty() { None } else { Some(out) }
}

/// `noa.ViableRanges` — keep only spans with at least `VIABLE_RANGE_MIN_TOKENS`
/// tokens. Returns an empty (non-nil) vector when nothing survives, mirroring Go's
/// `make([]RangeInfo, 0, …)`.
pub fn viable_ranges(ranges: &[RangeInfo]) -> Vec<RangeInfo> {
    ranges
        .iter()
        .filter(|r| r.tokens >= VIABLE_RANGE_MIN_TOKENS)
        .cloned()
        .collect()
}
