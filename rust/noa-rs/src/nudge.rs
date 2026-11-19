//! `nudge.rs` — behavioral-parity Rust port of `noa.DecideNudge` and its unexported
//! helpers (`resolveAdaptiveGrowth`, `growthFloorOf`, `Config.minPressureBenefit`,
//! `computeTierPending`, `tierCadenceReady`, `pendingSummary`, `decidePressure`,
//! `decideGrowth`) from the external `norma/noa` package (`nudge.go`).
//!
//! `DecideNudge` is the nudge-decision capstone: it decides whether to ask the model
//! to compress and for which tier. It is pure and deterministic. The `TokenCountFn`
//! callback is modeled as `fn(&str) -> i64` (default `DefaultCountTokens`).
//!
//! Faithful details:
//! * `ContextUsage` / `Breakdown.Usage` are `float64` in Go; `json.Marshal` prints an
//!   integral float without a decimal (`0`, not `0.0`). `go_float` reproduces that so
//!   the value matches Go's serialized form under `serde_json` equality.
//! * `CompressibleRanges` / `ProtectedRanges` pass through `in.Recommendation` verbatim
//!   (a Go nil slice serializes to `null`; a non-nil slice to an array). Modeled as
//!   `Option<Vec<_>>` where `None` → `null`.
//! * `TierTargetBlocks` is `[]CompressionBlock`, set only for a tier-2/3 injection; it
//!   is a Go nil slice (`null`) otherwise. Blocks pass through `computeTierPending`
//!   unchanged, so their internal nil slices (`DirectMessageIDs`, `DirectBlockIDs`)
//!   stay `null`.
//! * `ActiveBlockSpans` is always a non-nil slice (`[]` when empty).
//! * `Breakdown.CountT1`/`CountT2` are `tiers[2].count`/`tiers[3].count` (the count of
//!   active tier-1 / tier-2 source blocks) — the field names are deliberately faithful
//!   to the Go struct even though they read counterintuitively.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use crate::breakdown::compute_context_breakdown;
use crate::nudgetext::BlockSpan;
use crate::port::{Config, CoreMessage, round_half_up};
use crate::ranges::ProtectedRange;
use crate::recommend::RangeInfo;
use serde_json::{Value, json};

/// `noa.NudgeState` — the nudge pacing/telemetry carried on `CompressionState`.
#[derive(Debug, Clone, Default)]
pub struct NudgeState {
    pub last_per_message_nudge_tokens: i64,
    pub last_nudge_shown_tokens: i64,
    /// `LastShownByTier` — per-tier last-shown token count (tier → tokens).
    pub last_shown_by_tier: BTreeMap<i64, i64>,
}

/// `noa.CompressionBlock` — the full block model. Only the fields the decision reads
/// are consulted for logic; the whole struct is re-serialized for `TierTargetBlocks`.
/// The three id slices are `Option<Vec<_>>` so a Go nil slice round-trips to `null`.
#[derive(Debug, Clone, Default)]
pub struct FullBlock {
    pub block_id: String,
    pub tier: i64,
    pub topic: String,
    pub summary: String,
    pub direct_message_ids: Option<Vec<String>>,
    pub effective_message_ids: Option<Vec<String>>,
    pub direct_block_ids: Option<Vec<String>>,
    pub archive_path: String,
    pub archive_rel: String,
    pub compressed_tokens: i64,
    pub start_ref: String,
    pub end_ref: String,
    pub created_at: i64,
    pub active: bool,
    pub compress_call_id: String,
}

/// `noa.NudgeBreakdown` — the decision diagnostics.
#[derive(Debug, Clone, Default)]
pub struct NudgeBreakdown {
    pub usage: f64,
    pub growth: i64,
    pub growth_reference: i64,
    pub growth_floor: i64,
    pub adaptive_growth: i64,
    pub over_limit: bool,
    pub emergency: bool,
    pub min_pressure_benefit: i64,
    pub pending_t1: i64,
    pub pending_t2: i64,
    pub pending_t3: i64,
    pub count_t1: i64,
    pub count_t2: i64,
}

/// `noa.NudgeDecision` — the full decision. The range/block slices are kept as
/// `Option`/selected blocks so nil-ness round-trips exactly as Go serializes it.
#[derive(Debug, Clone, Default)]
pub struct NudgeDecision {
    pub should_inject: bool,
    pub reason: String,
    pub tier: i64,
    pub context_usage: f64,
    pub compressible_ranges: Option<Vec<RangeInfo>>,
    pub protected_ranges: Option<Vec<ProtectedRange>>,
    pub active_block_spans: Vec<BlockSpan>,
    pub tier_target_blocks: Option<Vec<FullBlock>>,
    pub breakdown: NudgeBreakdown,
    pub context_breakdown: BTreeMap<String, i64>,
    pub truncated_count: i64,
}

/// The resolved inputs `DecideNudge` reads, bundled.
pub struct DecideNudgeInput<'a> {
    pub cfg: &'a Config,
    pub messages: &'a [CoreMessage],
    /// `state.Blocks`, in input order.
    pub blocks: &'a [FullBlock],
    /// `state.Nudge`.
    pub nudge: &'a NudgeState,
    pub token_count: i64,
    /// `recommendation.CompressibleRanges` (`None` models a Go nil slice).
    pub compressible_ranges: Option<Vec<RangeInfo>>,
    /// `recommendation.ProtectedRanges` (`None` models a Go nil slice).
    pub protected_ranges: Option<Vec<ProtectedRange>>,
    pub truncated_count: i64,
}

/// `noa.resolveAdaptiveGrowth`.
fn resolve_adaptive_growth(limit: i64, n: &crate::port::NudgeConfig) -> i64 {
    if limit <= 0 {
        return n.growth_floor;
    }
    std::cmp::min(
        n.growth_cap,
        std::cmp::max(n.growth_floor, round_half_up(limit as f64 * n.growth_ratio)),
    )
}

/// `noa.growthFloorOf`. Note the `int(...)` truncation toward zero, not rounding.
fn growth_floor_of(adaptive: i64, n: &crate::port::NudgeConfig) -> i64 {
    std::cmp::max(
        n.min_growth_floor,
        (n.min_growth_ratio * adaptive as f64) as i64,
    )
}

/// `noa.Config.minPressureBenefit`.
fn min_pressure_benefit(cfg: &Config) -> i64 {
    if let Some(v) = cfg.nudge.min_pressure_benefit_tokens {
        return v;
    }
    std::cmp::max(5000, round_half_up(cfg.model_context_limit as f64 * 0.01))
}

/// One tier's reclaimable total: token sum plus the source blocks it would absorb.
#[derive(Default)]
struct TierPending {
    pending: i64,
    count: i64,
    blocks: Vec<usize>, // indices into `blocks`
}

/// `noa.computeTierPending` — measure what each tier could reclaim.
///
/// Tier 1 counts compressible raw content (ranges at or above `MinCompressRange`);
/// tiers 2 and 3 count the summary text of the active blocks they would absorb.
fn compute_tier_pending(
    in_blocks: &[FullBlock],
    compressible_ranges: &[RangeInfo],
    min_compress_range: i64,
    count: fn(&str) -> i64,
) -> [TierPending; 4] {
    let mut out: [TierPending; 4] = [
        TierPending::default(),
        TierPending::default(),
        TierPending::default(),
        TierPending::default(),
    ];

    let mut t1 = 0i64;
    for r in compressible_ranges {
        if r.chars >= min_compress_range {
            t1 += r.tokens;
        }
    }
    out[1].pending = t1;

    // tier 2 absorbs active tier-1 blocks; tier 3 absorbs active tier-2 blocks.
    for (tier, of) in [(2usize, 1i64), (3usize, 2i64)] {
        let mut tp = TierPending::default();
        for (i, b) in in_blocks.iter().enumerate() {
            if b.active && b.tier == of {
                tp.pending += count(&b.summary);
                tp.count += 1;
                tp.blocks.push(i);
            }
        }
        out[tier] = tp;
    }
    out
}

/// `noa.pendingSummary` — the one-line per-tier pending summary.
fn pending_summary(tiers: &[TierPending; 4]) -> String {
    format!(
        "ready: T1({})/T2({})/T3({})",
        crate::port::format_tokens(tiers[1].pending),
        crate::port::format_tokens(tiers[2].pending),
        crate::port::format_tokens(tiers[3].pending),
    )
}

/// `noa.tierCadenceReady` — has enough context grown since this tier was last shown.
fn tier_cadence_ready(nudge: &NudgeState, tier: i64, token_count: i64, floor: i64) -> bool {
    let last = nudge.last_shown_by_tier.get(&tier).copied().unwrap_or(0);
    last == 0 || token_count - last >= floor
}

/// `noa.decidePressure` — pick the tier with the most to reclaim, above the benefit floor.
fn decide_pressure(
    d: &mut NudgeDecision,
    tiers: &[TierPending; 4],
    cfg: &Config,
    blocks: &[FullBlock],
    min_ben: i64,
) {
    let mut candidates = vec![1usize];
    if cfg.tiers.enabled {
        candidates.push(2);
        candidates.push(3);
    }
    let mut best = 0usize;
    let mut best_pending: i64 = -1;
    for &t in &candidates {
        if tiers[t].pending > best_pending {
            best = t;
            best_pending = tiers[t].pending;
        }
    }
    if best_pending < min_ben {
        // Under pressure but nothing worth reclaiming; nudging would burn a turn.
        d.reason = format!(
            "pressure at {:.0}% but best tier reclaims only {} (< {} minimum benefit); {}",
            d.context_usage * 100.0,
            std::cmp::max(best_pending, 0),
            min_ben,
            pending_summary(tiers),
        );
        return;
    }
    d.should_inject = true;
    d.tier = best as i64;
    if best >= 2 {
        d.tier_target_blocks = Some(select_blocks(&tiers[best].blocks, blocks));
    }
    d.reason = format!(
        "pressure at {:.0}%: tier {} reclaims {}; {}",
        d.context_usage * 100.0,
        best,
        best_pending,
        pending_summary(tiers),
    );
}

/// `noa.decideGrowth` — pick a tier in the efficiency band, in priority order.
#[allow(clippy::too_many_arguments)]
fn decide_growth(
    d: &mut NudgeDecision,
    in_blocks: &[FullBlock],
    tiers: &[TierPending; 4],
    cfg: &Config,
    nudge: &NudgeState,
    token_count: i64,
    adaptive: i64,
    floor: i64,
) {
    let tier2_threshold = round_half_up(adaptive as f64 * cfg.nudge.tier2_growth_multiplier);
    let usage_floor = cfg.nudge.min_context_limit_pct;
    let t2_count_ready =
        tiers[2].count >= cfg.tiers.tier2_trigger && d.context_usage >= usage_floor;
    let t3_count_ready =
        tiers[3].count >= cfg.tiers.tier3_trigger && d.context_usage >= usage_floor;

    if tiers[1].pending >= adaptive {
        d.should_inject = true;
        d.tier = 1;
        d.reason = format!(
            "growth: tier 1 has {} pending (>= {}); {}",
            tiers[1].pending,
            adaptive,
            pending_summary(tiers),
        );
        return;
    }
    if cfg.tiers.enabled {
        if t2_count_ready
            || (tiers[2].pending >= tier2_threshold && tiers[2].pending > tiers[1].pending)
        {
            if tier_cadence_ready(nudge, 2, token_count, floor) {
                d.should_inject = true;
                d.tier = 2;
                d.tier_target_blocks = Some(select_blocks(&tiers[2].blocks, in_blocks));
                d.reason = format!(
                    "growth: tier 2 ready ({} block(s), {} pending); {}",
                    tiers[2].count,
                    tiers[2].pending,
                    pending_summary(tiers),
                );
                return;
            }
            d.reason = format!("blocked: T2 (cadence); {}", pending_summary(tiers));
            return;
        }
        if t3_count_ready
            || (tiers[3].pending >= tier2_threshold
                && tiers[3].pending > tiers[2].pending
                && tiers[3].pending > tiers[1].pending)
        {
            if tier_cadence_ready(nudge, 3, token_count, floor) {
                d.should_inject = true;
                d.tier = 3;
                d.tier_target_blocks = Some(select_blocks(&tiers[3].blocks, in_blocks));
                d.reason = format!(
                    "growth: tier 3 ready ({} block(s), {} pending); {}",
                    tiers[3].count,
                    tiers[3].pending,
                    pending_summary(tiers),
                );
                return;
            }
            d.reason = format!("blocked: T3 (cadence); {}", pending_summary(tiers));
            return;
        }
    }
    d.reason = format!(
        "growth ready but no tier qualifies; {}",
        pending_summary(tiers)
    );
}

/// Collect the blocks at the given indices into an owned vec (the tier's absorb targets).
fn select_blocks(indices: &[usize], blocks: &[FullBlock]) -> Vec<FullBlock> {
    indices.iter().map(|&i| blocks[i].clone()).collect()
}

/// `noa.DecideNudge` — decide whether to ask the model to compress, and for what.
pub fn decide_nudge(inp: &DecideNudgeInput, count: fn(&str) -> i64) -> NudgeDecision {
    // `count` is always a resolved `TokenCountFn`; the oracle resolves a nil Go
    // callback to `DefaultCountTokens` before dispatch.
    let cfg = inp.cfg;
    let limit = cfg.model_context_limit;
    let usage = if limit > 0 {
        inp.token_count as f64 / limit as f64
    } else {
        0.0
    };

    let adaptive = resolve_adaptive_growth(limit, &cfg.nudge);
    let floor = growth_floor_of(adaptive, &cfg.nudge);
    let min_ben = min_pressure_benefit(cfg);

    let over_limit = usage >= cfg.nudge.max_context_limit_pct;
    let emergency = usage >= cfg.nudge.emergency_threshold_pct;
    let pressure = over_limit || emergency;

    let baseline = inp.nudge.last_per_message_nudge_tokens;
    let mut growth_ref = inp.nudge.last_nudge_shown_tokens;
    if growth_ref == 0 {
        if baseline > 0 {
            growth_ref = baseline;
        } else {
            growth_ref = inp.token_count;
        }
    }
    let growth_since = inp.token_count - growth_ref;

    let tiers = compute_tier_pending(
        inp.blocks,
        inp.compressible_ranges.as_deref().unwrap_or(&[]),
        cfg.compress.min_compress_range,
        count,
    );

    let mut d = NudgeDecision {
        context_usage: usage,
        compressible_ranges: inp.compressible_ranges.clone(),
        protected_ranges: inp.protected_ranges.clone(),
        active_block_spans: active_block_spans(inp.blocks),
        context_breakdown: compute_context_breakdown(inp.messages, count),
        truncated_count: inp.truncated_count,
        breakdown: NudgeBreakdown {
            usage,
            growth: growth_since,
            growth_reference: growth_ref,
            growth_floor: floor,
            adaptive_growth: adaptive,
            over_limit,
            emergency,
            min_pressure_benefit: min_ben,
            pending_t1: tiers[1].pending,
            pending_t2: tiers[2].pending,
            pending_t3: tiers[3].pending,
            count_t1: tiers[2].count,
            count_t2: tiers[3].count,
        },
        ..Default::default()
    };

    if pressure {
        decide_pressure(&mut d, &tiers, cfg, inp.blocks, min_ben);
        return d;
    }

    // First sight: a session arriving with a backlog has no baseline to have grown
    // from, so the cadence gate would otherwise hold it back indefinitely.
    let first_sight = inp.nudge.last_nudge_shown_tokens == 0
        && baseline == 0
        && usage >= cfg.nudge.min_context_limit_pct
        && std::cmp::max(
            tiers[1].pending,
            std::cmp::max(tiers[2].pending, tiers[3].pending),
        ) >= adaptive;
    let growth_ready = first_sight || growth_since >= floor;
    if !growth_ready {
        d.reason = format!(
            "below cadence: grew {} of {} since last nudge; {}",
            growth_since,
            floor,
            pending_summary(&tiers),
        );
        return d;
    }
    decide_growth(
        &mut d,
        inp.blocks,
        &tiers,
        cfg,
        inp.nudge,
        inp.token_count,
        adaptive,
        floor,
    );
    d
}

/// `noa.ActiveBlockSpans` over the full block model (active blocks, stable-sorted by
/// start-ref index). Mirrors `state::active_block_spans`.
fn active_block_spans(blocks: &[FullBlock]) -> Vec<BlockSpan> {
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
    out.sort_by_key(|s| crate::port::ref_to_index(&s.start_ref).0);
    out
}

/// Format a float the way Go's `encoding/json` does: an integral value prints without a
/// decimal point (`0`, `1`), everything else uses the shortest round-trip form. This
/// matters because `serde_json` treats `0.0` (float) and `0` (int) as unequal values.
fn go_float(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

fn opt_str_vec(v: &Option<Vec<String>>) -> Value {
    match v {
        None => Value::Null,
        Some(s) => serde_json::to_value(s).expect("serialize string vec"),
    }
}

fn block_to_value(b: &FullBlock) -> Value {
    json!({
        "BlockID": b.block_id,
        "Tier": b.tier,
        "Topic": b.topic,
        "Summary": b.summary,
        "DirectMessageIDs": opt_str_vec(&b.direct_message_ids),
        "EffectiveMessageIDs": opt_str_vec(&b.effective_message_ids),
        "DirectBlockIDs": opt_str_vec(&b.direct_block_ids),
        "ArchivePath": b.archive_path,
        "ArchiveRel": b.archive_rel,
        "CompressedTokens": b.compressed_tokens,
        "StartRef": b.start_ref,
        "EndRef": b.end_ref,
        "CreatedAt": b.created_at,
        "Active": b.active,
        "CompressCallID": b.compress_call_id,
    })
}

fn opt_range_value(v: &Option<Vec<RangeInfo>>) -> Value {
    match v {
        None => Value::Null,
        Some(s) => serde_json::to_value(s).expect("serialize ranges"),
    }
}

fn opt_protected_value(v: &Option<Vec<ProtectedRange>>) -> Value {
    match v {
        None => Value::Null,
        Some(s) => serde_json::to_value(s).expect("serialize protected"),
    }
}

fn spans_value(spans: &[BlockSpan]) -> Value {
    let out: Vec<Value> = spans
        .iter()
        .map(|s| {
            json!({
                "BlockID": s.block_id,
                "Tier": s.tier,
                "StartRef": s.start_ref,
                "EndRef": s.end_ref,
            })
        })
        .collect();
    Value::Array(out)
}

fn targets_value(blocks: &Option<Vec<FullBlock>>) -> Value {
    match blocks {
        None => Value::Null,
        Some(bs) => Value::Array(bs.iter().map(block_to_value).collect()),
    }
}

fn breakdown_value(b: &NudgeBreakdown) -> Value {
    json!({
        "Usage": go_float(b.usage),
        "Growth": b.growth,
        "GrowthReference": b.growth_reference,
        "GrowthFloor": b.growth_floor,
        "AdaptiveGrowth": b.adaptive_growth,
        "OverLimit": b.over_limit,
        "Emergency": b.emergency,
        "MinPressureBenefit": b.min_pressure_benefit,
        "PendingT1": b.pending_t1,
        "PendingT2": b.pending_t2,
        "PendingT3": b.pending_t3,
        "CountT1": b.count_t1,
        "CountT2": b.count_t2,
    })
}

impl NudgeDecision {
    /// Serialize the decision to the exact JSON shape Go's `json.Marshal` produces.
    pub fn to_value(&self) -> Value {
        let ctx: serde_json::Map<String, Value> = self
            .context_breakdown
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!({
            "ShouldInject": self.should_inject,
            "Reason": self.reason,
            "Tier": self.tier,
            "ContextUsage": go_float(self.context_usage),
            "CompressibleRanges": opt_range_value(&self.compressible_ranges),
            "ProtectedRanges": opt_protected_value(&self.protected_ranges),
            "ActiveBlockSpans": spans_value(&self.active_block_spans),
            "TierTargetBlocks": targets_value(&self.tier_target_blocks),
            "Breakdown": breakdown_value(&self.breakdown),
            "ContextBreakdown": Value::Object(ctx),
            "TruncatedCount": self.truncated_count,
        })
    }
}
