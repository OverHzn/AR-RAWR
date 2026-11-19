//! `schema.rs` — behavioral-parity Rust port of `noa.CompressToolSchema` (the Compress
//! tool's JSON Schema) from the external `norma/noa` package (`schema.go`).
//!
//! The schema deliberately omits `"type": "array"` on `content` so providers without
//! strict tool support can stringify nested array arguments and let the lenient parser
//! recover them. This module reproduces the exact `map[string]any` structure; the parity
//! test compares it as a `serde_json::Value` (whose object keys are sorted, matching
//! Go's `json.Marshal` map-key ordering).

#![forbid(unsafe_code)]

use serde_json::{Value, json};

/// `noa.CompressToolSchema` — the Compress tool's JSON Schema.
pub fn compress_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": "Optional short title applied to ranges that don't carry their own.",
            },
            "summaryMaxChars": {
                "type": "number",
                "description": "Raise the per-summary limit above the 20000-char default. Use it when the content genuinely needs more detail — do not truncate critical information just to fit.",
            },
            "content": {
                "description": "One or more ranges to compress into separate summary blocks. Ranges must be disjoint.",
                "items": {
                    "type": "object",
                    "properties": {
                        "startId": {
                            "type": "string",
                            "description": "mNNNNN message ref, or bN block id, at the start of the range.",
                        },
                        "endId": {
                            "type": "string",
                            "description": "mNNNNN message ref, or bN block id, at the end of the range. Inclusive.",
                        },
                        "summary": {
                            "type": "string",
                            "description": "Self-contained summary that replaces the range in context.",
                        },
                        "topic": {
                            "type": "string",
                            "description": "Short 3-5 word label for THIS range.",
                        },
                    },
                    "required": ["startId", "endId", "summary"],
                },
            },
        },
        "required": ["content"],
    })
}
