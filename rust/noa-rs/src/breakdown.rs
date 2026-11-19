//! `breakdown.rs` — behavioral-parity Rust port of `noa.ComputeContextBreakdown` from
//! the external `norma/noa` package (`nudge.go`).
//!
//! Sums the token count of every message into one of four buckets, classifying each
//! message by a fixed precedence: a summary-header prefix wins, then a tool content-type,
//! then a triple-backtick code fence, else plain text. The `TokenCountFn` callback is
//! modeled as `fn(&str) -> i64` (default `DefaultCountTokens`). A bucket key is created
//! whenever a message lands in it, even for a 0-token message (matching Go's
//! `out[k] += n` on a map). The result map is returned sorted (Go `json.Marshal` sorts
//! map keys; `BTreeMap` does the same).

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use crate::blocks::SUMMARY_HEADER;
use crate::port::{CT_TOOL_CALL, CT_TOOL_RESULT, CoreMessage};

/// The bucket names `ComputeContextBreakdown` writes to.
pub const BK_SUMMARIES: &str = "summaries";
pub const BK_TOOL: &str = "tool";
pub const BK_CODE: &str = "code";
pub const BK_TEXT: &str = "text";

/// `noa.ComputeContextBreakdown` — per-bucket token totals for the current context.
pub fn compute_context_breakdown(
    msgs: &[CoreMessage],
    count: fn(&str) -> i64,
) -> BTreeMap<String, i64> {
    let mut out: BTreeMap<String, i64> = BTreeMap::new();
    for m in msgs {
        let n = count(&m.text);
        let bucket = if m.text.starts_with(SUMMARY_HEADER) {
            BK_SUMMARIES
        } else if m.content_type == CT_TOOL_CALL || m.content_type == CT_TOOL_RESULT {
            BK_TOOL
        } else if m.text.contains("```") {
            BK_CODE
        } else {
            BK_TEXT
        };
        *out.entry(bucket.to_string()).or_insert(0) += n;
    }
    out
}
