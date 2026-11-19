//! Rust port of the unexported, pure helpers in the Go `llmpool/` package.

use gocompat::strings::go_trim_space;
use llm_rs::port::{CompletionRequest, ContentBlock};
use serde_json::Value;

/// `trimErr` (`llmpool/pool.go`): a one-line, length-capped rendering of an error.
/// Replaces every `\n` with a space and trims surrounding whitespace. If the result
/// is longer than 300 bytes, the cut is moved back to the nearest UTF-8 rune start at
/// or before byte 300 (Go: `for cut > 0 && !utf8.RuneStart(s[cut]) { cut-- }`) so no
/// rune is split, then `…` is appended.
pub fn trim_err(err: &str) -> String {
    let replaced = err.replace('\n', " ");
    let s = go_trim_space(&replaced);
    let bytes = s.as_bytes();
    if bytes.len() > 300 {
        // `utf8.RuneStart(b)` == `b & 0xC0 != 0x80`; back off to a rune boundary.
        let mut cut = 300usize;
        while cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        // `cut` is a char boundary, so this slice is valid UTF-8.
        let prefix = std::str::from_utf8(&bytes[..cut]).unwrap_or("");
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
