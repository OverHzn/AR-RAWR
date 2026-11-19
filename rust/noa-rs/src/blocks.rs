//! `blocks.rs` — behavioral-parity Rust port of `noa.RenderSummaryMessage` (the
//! summary-message renderer) from the external `norma/noa` package (`prune.go`),
//! plus the `CompressionBlock` subset it reads.
//!
//! A compressed range is replaced in the conversation view by a single synthetic
//! user message: the `[Compressed conversation section]` header, an optional topic,
//! the model's summary, and a trailing `<noa-archive …/>` pointer. The topic is
//! emitted verbatim; only the summary is whitespace-trimmed (matching Go's
//! `strings.TrimSpace`). Pure and deterministic.

#![forbid(unsafe_code)]

use crate::port::{CT_TEXT, CoreMessage, ROLE_USER, summary_message_id};
use crate::refsmap::{ArchiveBlock, archive_tag};

/// `noa.SummaryHeader` — the leading line of every rendered summary message.
pub const SUMMARY_HEADER: &str = "[Compressed conversation section]";

/// The subset of `noa.CompressionBlock` that `RenderSummaryMessage` reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CompressionBlock {
    pub block_id: String,
    pub tier: i64,
    pub topic: String,
    pub summary: String,
    pub start_ref: String,
    pub end_ref: String,
    pub archive_path: String,
}

/// `noa.RenderSummaryMessage` — build the synthetic user message that stands in for
/// a compressed range: header, optional ` — topic`, the trimmed summary, then the
/// `<noa-archive …/>` pointer.
pub fn render_summary_message(b: &CompressionBlock) -> CoreMessage {
    let mut sb = String::from(SUMMARY_HEADER);
    if !b.topic.is_empty() {
        sb.push_str(" — ");
        sb.push_str(&b.topic);
    }
    let summary = b.summary.trim();
    if !summary.is_empty() {
        sb.push('\n');
        sb.push_str(summary);
    }
    sb.push_str("\n\n");
    let tag = archive_tag(&ArchiveBlock {
        block_id: b.block_id.clone(),
        tier: b.tier,
        start_ref: b.start_ref.clone(),
        end_ref: b.end_ref.clone(),
        archive_path: b.archive_path.clone(),
    });
    sb.push_str(&tag);
    CoreMessage {
        id: summary_message_id(&b.block_id),
        role: ROLE_USER.to_string(),
        content_type: CT_TEXT.to_string(),
        text: sb,
        tool_name: String::new(),
        tool_call_id: String::new(),
    }
}
