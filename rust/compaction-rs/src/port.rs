//! Rust port of the pure functions in the Go `norma/compaction` package.
//!
//! `compaction` is an external (read-only) module, so its *exported* pure functions are
//! reached through a standalone Go oracle (`oracle/`) rather than an in-package oracle.
//! Unexported helpers (`blockChars`, …) are not reachable that way; [`block_chars`] is
//! ported as a private helper and exercised transitively through [`estimate_tokens`].

use llm_rs::port::{
    BLOCK_TEXT, BLOCK_THINKING, BLOCK_TOOL_RESULT, BLOCK_TOOL_USE, ContentBlock, Message,
};
use serde_json::Value;

/// `compaction.blockChars` (`compaction.go`): per-block character weight used by
/// [`estimate_tokens`]. text → `len(Text)`, thinking → `len(Thinking)`, tool_use →
/// `len(Name) + len(Input)`, tool_result → sum over nested `Content`, default → `len(Text)`.
///
/// `len(Input)` is Go's raw-JSON byte length; the oracle keeps tool inputs compact and
/// ASCII so re-serializing the decoded value reproduces it.
fn block_chars(b: &ContentBlock) -> i64 {
    let t = b.block_type.as_str();
    if t == BLOCK_TEXT {
        b.text.len() as i64
    } else if t == BLOCK_THINKING {
        b.thinking.len() as i64
    } else if t == BLOCK_TOOL_USE {
        b.name.len() as i64 + input_len(b.input.as_ref())
    } else if t == BLOCK_TOOL_RESULT {
        b.content.iter().map(block_chars).sum()
    } else {
        b.text.len() as i64
    }
}

/// `compaction.EstimateTokens` (`compaction.go`): sum `blockChars` over every content
/// block of every message, then integer-divide by 4 (a rough chars→tokens heuristic).
pub fn estimate_tokens(msgs: &[Message]) -> i64 {
    let mut chars: i64 = 0;
    for m in msgs {
        for b in &m.content {
            chars += block_chars(b);
        }
    }
    chars / 4
}

/// `compaction.IsOverflow` (`compaction.go`): does an error string signal a
/// prompt-too-long / context-overflow condition? `None` models a Go `nil` error (false).
/// Case-insensitive substring match against the known phrasings.
pub fn is_overflow(err: Option<&str>) -> bool {
    let Some(e) = err else {
        return false;
    };
    let s = e.to_lowercase();
    s.contains("too long")
        || s.contains("context length")
        || s.contains("context_length")
        || s.contains("maximum context")
        || s.contains("status 413")
}

/// Go `len(json.RawMessage)` — the byte length of the raw JSON. The oracle keeps tool
/// inputs compact and ASCII, so re-serializing the decoded value reproduces that length.
fn input_len(input: Option<&Value>) -> i64 {
    match input {
        Some(v) => serde_json::to_string(v).map_or(0, |s| s.len()) as i64,
        None => 0,
    }
}

/// `microClearedMessage` (`compaction.go`): the placeholder a micro-compaction writes in
/// place of old tool-result content. Ported for callers that need to recognise it.
pub const MICRO_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";
