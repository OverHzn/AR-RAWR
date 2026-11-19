//! `nudge_text.rs` — behavioral-parity Rust port of `noa.RenderNudgeText` and its
//! unexported helpers (`renderTierNudge`, `renderEmergencyNudge`, `renderGentleNudge`,
//! `truncationNotice`, `sectionOr`) from the external `norma/noa` package
//! (`nudgetext.go` + `prompts_rules.go`).
//!
//! `RenderNudgeText` turns a `NudgeDecision` into the message the model reads. It is
//! pure and deterministic (string assembly only). The four load-bearing rule texts
//! and the section defaults are ported verbatim into [`crate::nudge_text_consts`].
//!
//! Faithful details:
//! * `FormatBreakdown` reads `d.ContextBreakdown` (a `map[string]int`) and
//!   `d.Breakdown.Growth`; the ported `format_breakdown` takes a sorted slice, so we
//!   materialize the `BTreeMap` in its (already sorted) order.
//! * `FormatTierTargetBlocks` reads `[]CompressionBlock`; we convert the decision's
//!   `Option<Vec<FullBlock>>` into the `TierTargetBlock` view the formatter expects.
//! * `sectionOr` returns the override pointer's value when set, else the default.
//! * `compact` drops leading blank lines left by a removed/empty section.
//! * The voice is `emergency` when the breakdown flags `Emergency || OverLimit`,
//!   `gentle` otherwise; a tier>=2 injection always goes through `renderTierNudge`
//!   (which may still switch to the emergency voice).

#![forbid(unsafe_code)]

use crate::nudge::NudgeDecision;
use crate::nudge_text_consts::{
    COMPRESS_PHILOSOPHY, DEFAULT_EFFICIENCY_NOTE, DEFAULT_EMERGENCY_HEADER, DEFAULT_T2_GUIDANCE,
    DEFAULT_T3_GUIDANCE, HOW_TO_COMPRESS_RULES, SECTION_COMPRESS_TOOL, SECTION_MULTI_TIER,
    SECTION_WHEN_NOT_TO_COMPRESS, SECTION_WHEN_TO_COMPRESS, TIER2_RULES, TIER3_RULES,
};
use crate::nudgetext::{
    TierTargetBlock, format_block_map, format_breakdown, format_tier_target_blocks,
};
use crate::ranges::format_ranges;
use crate::truncate::TRUNCATION_MARKER;

/// `noa.NudgeVoice` — the register a nudge is written in.
pub const VOICE_GENTLE: &str = "gentle";
pub const VOICE_EMERGENCY: &str = "emergency";

/// `noa.NudgeSections` — the surface-class fragments a host may replace freely.
/// Each field is `Option<String>` (`SectionOverride = *string`): `None` means "use
/// the default", `Some(s)` overrides it.
#[derive(Debug, Clone, Default)]
pub struct NudgeSections {
    pub efficiency_note: Option<String>,
    pub emergency_header: Option<String>,
    pub t2_guidance: Option<String>,
    pub t3_guidance: Option<String>,
}

/// `noa.sectionOr` — the override's value when set, else the default.
fn section_or(o: &Option<String>, def: &str) -> String {
    match o {
        Some(s) => s.clone(),
        None => def.to_string(),
    }
}

/// `noa.truncationNotice` — why some tool output looks cut, and what not to do.
fn truncation_notice(n: i64) -> String {
    format!(
        "{n} tool result(s) were mechanically truncated to keep this request valid — look for\n\"{marker}\". Do NOT re-run those tools: that re-inflates the\ncontext you are about to overflow. Compress instead. The truncation is applied only\nto what is sent, never to the stored history, so it is undone automatically once\nusage drops back below the threshold.",
        n = n,
        marker = TRUNCATION_MARKER,
    )
}

/// `noa.compact` — drop leading blank lines left by a removed/empty section.
fn compact(parts: &mut Vec<String>) {
    while parts.first().is_some_and(|s| s.is_empty()) {
        parts.remove(0);
    }
}

/// Convert the decision's sorted `context_breakdown` map into the slice form
/// `format_breakdown` expects. The map is already in sorted (BTree) order.
fn breakdown_slice(d: &NudgeDecision) -> Vec<(String, i64)> {
    d.context_breakdown
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

/// Build the `TierTargetBlock` view from the decision's optional full blocks.
fn tier_target_view(d: &NudgeDecision) -> Vec<TierTargetBlock> {
    match &d.tier_target_blocks {
        None => Vec::new(),
        Some(blocks) => blocks
            .iter()
            .map(|b| TierTargetBlock {
                block_id: b.block_id.clone(),
                tier: b.tier,
                topic: b.topic.clone(),
                summary: b.summary.clone(),
                effective_message_ids: b.effective_message_ids.clone().unwrap_or_default(),
                compressed_tokens: b.compressed_tokens,
            })
            .collect(),
    }
}

/// `noa.RenderNudgeText` — turn a decision into the message the model reads.
/// Returns `(voice, text)`.
pub fn render_nudge_text(d: &NudgeDecision, s: &NudgeSections) -> (String, String) {
    let breakdown = format_breakdown(&breakdown_slice(d), d.breakdown.growth);
    let block_map = format_block_map(&d.active_block_spans);
    let is_emergency = d.breakdown.emergency || d.breakdown.over_limit;

    if d.tier >= 2 {
        return render_tier_nudge(d, s, &breakdown, is_emergency);
    }
    if is_emergency {
        return (
            VOICE_EMERGENCY.to_string(),
            render_emergency_nudge(d, s, &breakdown, &block_map),
        );
    }
    (
        VOICE_GENTLE.to_string(),
        render_gentle_nudge(d, s, &breakdown, &block_map),
    )
}

/// `noa.renderTierNudge` — a tier>=2 consolidation nudge.
fn render_tier_nudge(
    d: &NudgeDecision,
    s: &NudgeSections,
    breakdown: &str,
    is_emergency: bool,
) -> (String, String) {
    let is_t2 = d.tier == 2;
    let word = if is_t2 {
        "DISTILLATION"
    } else {
        "CONDENSATION"
    };
    let mut trigger = format!("[TIER {} {} TRIGGER]", d.tier, word);
    let mut voice = VOICE_GENTLE.to_string();
    if is_emergency {
        voice = VOICE_EMERGENCY.to_string();
        trigger = format!(
            "[EMERGENCY — TIER {} {}] Context limit reached — distill NOW into a denser summary to reclaim tokens.",
            d.tier, word
        );
    }
    let (guidance, rules) = if is_t2 {
        (section_or(&s.t2_guidance, DEFAULT_T2_GUIDANCE), TIER2_RULES)
    } else {
        (section_or(&s.t3_guidance, DEFAULT_T3_GUIDANCE), TIER3_RULES)
    };

    let (start_id, end_id) = match &d.tier_target_blocks {
        Some(blocks) if !blocks.is_empty() => (
            blocks[0].block_id.clone(),
            blocks[blocks.len() - 1].block_id.clone(),
        ),
        _ => ("b1".to_string(), "b5".to_string()),
    };

    let example = format!(
        "Example: Compress({{ content: [{{ startId: {:?}, endId: {:?}, summary: \"...\" }}] }})",
        start_id, end_id
    );

    // A tier nudge omits WHEN TO / WHEN NOT TO COMPRESS: it consolidates blocks, not
    // raw messages, so the timing guidance for raw content does not apply.
    let mut parts: Vec<String> = vec![
        section_or(&s.efficiency_note, DEFAULT_EFFICIENCY_NOTE),
        String::new(),
        COMPRESS_PHILOSOPHY.to_string(),
        String::new(),
        breakdown.to_string(),
        String::new(),
        trigger,
        guidance,
        String::new(),
        SECTION_MULTI_TIER.to_string(),
        String::new(),
        format_tier_target_blocks(&tier_target_view(d)),
        String::new(),
        SECTION_COMPRESS_TOOL.to_string(),
        example,
        String::new(),
        HOW_TO_COMPRESS_RULES.to_string(),
        String::new(),
        rules.to_string(),
    ];
    if d.truncated_count > 0 {
        let mut prefix = vec![truncation_notice(d.truncated_count), String::new()];
        prefix.append(&mut parts);
        parts = prefix;
    }
    compact(&mut parts);
    (voice, parts.join("\n"))
}

/// `noa.renderEmergencyNudge` — the high-pressure nudge.
fn render_emergency_nudge(
    d: &NudgeDecision,
    s: &NudgeSections,
    breakdown: &str,
    block_map: &str,
) -> String {
    let mut parts: Vec<String> = vec![
        section_or(&s.emergency_header, DEFAULT_EMERGENCY_HEADER),
        String::new(),
    ];
    if d.truncated_count > 0 {
        parts.push(truncation_notice(d.truncated_count));
        parts.push(String::new());
    }
    let ranges = format_ranges(
        d.compressible_ranges.as_deref().unwrap_or(&[]),
        d.protected_ranges.as_deref().unwrap_or(&[]),
    );
    parts.extend([
        COMPRESS_PHILOSOPHY.to_string(),
        String::new(),
        breakdown.to_string(),
        String::new(),
        // At this pressure "should I compress" is settled; "what must I not
        // compress" is the question that still matters.
        SECTION_WHEN_NOT_TO_COMPRESS.to_string(),
        String::new(),
        SECTION_COMPRESS_TOOL.to_string(),
        "{ \"topic\": \"...\", \"content\": [{ \"startId\": \"<ID>\", \"endId\": \"<ID>\", \"summary\": \"...\" }] }".to_string(),
        "Only use IDs from visible messages above. Compress older work first.".to_string(),
        String::new(),
        HOW_TO_COMPRESS_RULES.to_string(),
        String::new(),
        ranges,
    ]);
    if !block_map.is_empty() {
        parts.push(String::new());
        parts.push(block_map.to_string());
    }
    compact(&mut parts);
    parts.join("\n")
}

/// `noa.renderGentleNudge` — the low-pressure, advisory nudge.
fn render_gentle_nudge(
    d: &NudgeDecision,
    s: &NudgeSections,
    breakdown: &str,
    block_map: &str,
) -> String {
    let ranges = format_ranges(
        d.compressible_ranges.as_deref().unwrap_or(&[]),
        d.protected_ranges.as_deref().unwrap_or(&[]),
    );
    let mut parts: Vec<String> = vec![
        section_or(&s.efficiency_note, DEFAULT_EFFICIENCY_NOTE),
        String::new(),
        COMPRESS_PHILOSOPHY.to_string(),
        String::new(),
        breakdown.to_string(),
        String::new(),
        SECTION_WHEN_TO_COMPRESS.to_string(),
        String::new(),
        SECTION_WHEN_NOT_TO_COMPRESS.to_string(),
        String::new(),
        SECTION_COMPRESS_TOOL.to_string(),
        String::new(),
        HOW_TO_COMPRESS_RULES.to_string(),
        String::new(),
        ranges,
    ];
    if !block_map.is_empty() {
        parts.push(String::new());
        parts.push(block_map.to_string());
    }
    parts.push(String::new());
    parts.push(
        "💡 Compress all ranges in one call (pass multiple content entries: `content: [{...}, {...}]`)."
            .to_string(),
    );
    compact(&mut parts);
    parts.join("\n")
}
