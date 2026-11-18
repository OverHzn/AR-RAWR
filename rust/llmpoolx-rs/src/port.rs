//! Rust port of the unexported, pure helpers in the Go `llmpool/` package.

use gocompat::strings::go_trim_space;

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
