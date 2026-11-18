//! Rust port of the unexported, pure helpers in the Go `llmpool/` package.

use gocompat::strings::go_trim_space;
use llm_rs::port::{CompletionRequest, ContentBlock};
use serde_json::Value;

/// `trimErr` (`llmpool/pool.go`): a one-line, length-capped rendering of an error.
/// Replaces every `\n` with a space, trims surrounding whitespace, and if the result
/// is longer than 300 bytes keeps the first 300 bytes and appends `…`. Go slices bytes
/// (`s[:300]`), which can split a UTF-8 rune; this port mirrors Go's lossy rendering
/// with `from_utf8_lossy` (exact for the ASCII corpus, which is where truncation is
/// exercised).
pub fn trim_err(err: &str) -> String {
    let replaced = err.replace('\n', " ");
    let s = go_trim_space(&replaced);
    if s.len() > 300 {
        let prefix = String::from_utf8_lossy(&s.as_bytes()[..300]);
        format!("{prefix}\u{2026}")
    } else {
        s.to_string()
    }
}

/// `blocksLen` (`llmpool/pool.go`): total weight of a content-block slice. Sums
/// `len(Text) + len(Thinking) + len(Input) + len(Name)` per block and recurses into
/// nested `Content`. `len(Input)` is the byte length of the raw JSON; the harness keeps
/// tool inputs compact and ASCII so re-serializing the decoded value reproduces Go's
/// `len([]byte)`.
pub fn blocks_len(bs: &[ContentBlock]) -> i64 {
    let mut n: i64 = 0;
    for b in bs {
        n += b.text.len() as i64
            + b.thinking.len() as i64
            + b.name.len() as i64
            + input_len(b.input.as_ref());
        if !b.content.is_empty() {
            n += blocks_len(&b.content);
        }
    }
    n
}

/// `estimateTokens` (`llmpool/pool.go`): rough token estimate for a request —
/// `len(System) + blocksLen(Messages) + Σ(len(Name)+len(Description)+120*#schema-props)`,
/// then `* 2 / 7` (≈ len/3.5). Integer math, matching Go `int`.
pub fn estimate_tokens(req: &CompletionRequest) -> i64 {
    let mut n: i64 = 0;
    for s in &req.system {
        n += s.len() as i64;
    }
    for m in &req.messages {
        n += blocks_len(&m.content);
    }
    for t in &req.tools {
        n += t.name.len() as i64 + t.description.len() as i64 + (t.input_schema_len() as i64) * 120;
    }
    n * 2 / 7
}

/// Go `len(json.RawMessage)` — the byte length of the raw JSON. The harness keeps tool
/// inputs compact and ASCII, so re-serializing the decoded value reproduces that length.
fn input_len(input: Option<&Value>) -> i64 {
    match input {
        Some(v) => serde_json::to_string(v).map_or(0, |s| s.len()) as i64,
        None => 0,
    }
}
