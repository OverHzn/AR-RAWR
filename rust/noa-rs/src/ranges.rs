//! `ranges.rs` — behavioral-parity Rust port of `noa.FormatRanges` (the compressible/
//! protected range list renderer) and its `formatRangeLine` helper from the external
//! `norma/noa` package (`nudgetext.go`).
//!
//! Compressible and protected spans are sorted by start index (stable), then adjacent
//! or overlapping spans are merged into one entry whose tool/text split and protected
//! tools are unioned. Each merged entry becomes one line; a protected-only span is
//! flagged `[PROTECTED: … — not compressible]`, a mixed span shows both token counts,
//! and a pure compressible span shows its tool/text percentage split. Pure and
//! deterministic (Rust's `sort_by` is stable, matching Go's `sort.SliceStable`).

#![forbid(unsafe_code)]

use crate::port::{format_tokens, ref_to_index};
use crate::recommend::RangeInfo;

/// `noa.ProtectedRange` — a span excluded from compression, reported so the model can
/// see why a gap exists.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ProtectedRange {
    pub start_ref: String,
    pub end_ref: String,
    pub count: i64,
    pub tokens: i64,
    pub tools: Vec<String>,
}

/// The internal `mergedRange` accumulator (a compressible and/or protected span
/// after merging).
#[derive(Debug, Clone, Default)]
struct MergedRange {
    start_ref: String,
    end_ref: String,
    start_num: i64,
    end_num: i64,
    count: i64,
    tokens: i64,
    user_msgs: i64,
    compressible_tokens: i64,
    protected_tokens: i64,
    protected_count: i64,
    protected_tools: Vec<String>,
    tool_pct: i64,
    text_pct: i64,
    dangerous: bool,
}

/// `noa.FormatRanges` — render the compressible/protected range list, oldest first.
pub fn format_ranges(compressible: &[RangeInfo], protected: &[ProtectedRange]) -> String {
    if compressible.is_empty() && protected.is_empty() {
        return "[No specific ranges detected — compress any consumed content.]".to_string();
    }

    let mut entries: Vec<MergedRange> = Vec::new();
    for r in compressible {
        let (s, _) = ref_to_index(&r.start_ref);
        let (e, _) = ref_to_index(&r.end_ref);
        entries.push(MergedRange {
            start_ref: r.start_ref.clone(),
            end_ref: r.end_ref.clone(),
            start_num: s,
            end_num: e,
            count: r.count,
            tokens: r.tokens,
            user_msgs: r.user_msgs,
            compressible_tokens: r.tokens,
            tool_pct: r.tool_pct,
            text_pct: r.text_pct,
            dangerous: r.dangerous,
            ..MergedRange::default()
        });
    }
    for r in protected {
        let (s, _) = ref_to_index(&r.start_ref);
        let (e, _) = ref_to_index(&r.end_ref);
        entries.push(MergedRange {
            start_ref: r.start_ref.clone(),
            end_ref: r.end_ref.clone(),
            start_num: s,
            end_num: e,
            count: r.count,
            tokens: r.tokens,
            protected_tokens: r.tokens,
            protected_count: r.count,
            protected_tools: r.tools.clone(),
            ..MergedRange::default()
        });
    }

    // Stable sort by start index (Go sort.SliceStable).
    entries.sort_by_key(|e| e.start_num);

    let mut merged: Vec<MergedRange> = Vec::new();
    for e in entries {
        if let Some(last) = merged.last_mut()
            && e.start_num <= last.end_num + 1
        {
            last.end_ref = e.end_ref;
            last.end_num = std::cmp::max(last.end_num, e.end_num);
            last.count += e.count;
            last.tokens += e.tokens;
            last.user_msgs += e.user_msgs;
            last.compressible_tokens += e.compressible_tokens;
            last.protected_tokens += e.protected_tokens;
            last.protected_count += e.protected_count;
            last.dangerous = last.dangerous || e.dangerous;
            for tl in &e.protected_tools {
                if !last.protected_tools.iter().any(|x| x == tl) {
                    last.protected_tools.push(tl.clone());
                }
            }
            continue;
        }
        merged.push(e);
    }

    let lines: Vec<String> = merged.iter().map(format_range_line).collect();
    format!(
        "Compressible ranges ({}, oldest first):\n{}",
        merged.len(),
        lines.join("\n")
    )
}

/// `noa.formatRangeLine` — render one merged range as a single line.
fn format_range_line(e: &MergedRange) -> String {
    let suffix = if e.dangerous && e.compressible_tokens > 0 {
        "  ⚠️ NOT recommended unless you are certain."
    } else {
        ""
    };
    let user_note = if e.user_msgs > 0 {
        let s = if e.user_msgs > 1 { "s" } else { "" };
        format!(" · {} user msg{}", e.user_msgs, s)
    } else {
        String::new()
    };

    if e.protected_tokens > 0 && e.compressible_tokens == 0 {
        format!(
            "  {}–{}  {} msgs  {} [PROTECTED: {} — not compressible]{}",
            e.start_ref,
            e.end_ref,
            e.count,
            format_tokens(e.tokens),
            e.protected_tools.join(", "),
            suffix
        )
    } else if e.protected_tokens > 0 && e.compressible_tokens > 0 {
        format!(
            "  {}–{}  {} msgs  {} [{} compressible | {} protected: {}]{}{}",
            e.start_ref,
            e.end_ref,
            e.count,
            format_tokens(e.tokens),
            format_tokens(e.compressible_tokens),
            format_tokens(e.protected_tokens),
            e.protected_tools.join(", "),
            user_note,
            suffix
        )
    } else {
        format!(
            "  {}–{}  {} msgs  {} [tool {}% | text {}%]{}{}",
            e.start_ref,
            e.end_ref,
            e.count,
            format_tokens(e.tokens),
            e.tool_pct,
            e.text_pct,
            user_note,
            suffix
        )
    }
}
