//! Rust port of the unexported, pure helper functions in the Go `mcphttp/` package.
//!
//! The rest of `mcphttp/`'s unexported surface is I/O-bound (`readSSEEndpoint`,
//! `parseSSE`, `readSnippet` read from `bufio.Reader`/`io.Reader`) and stays in Go.
//! Go-stdlib string/quoting semantics come from `gocompat`.

use std::collections::BTreeMap;

use gocompat::gojson::go_quote_string;
use gocompat::strings::{go_to_lower, go_trim_space};

/// `normalizeHeaders` (`mcphttp/client.go`): trim each header name; reject an empty
/// name or one containing CR/LF with `invalid HTTP header name %q` (Go `%q` quoting
/// of the *original* key). Values pass through unchanged. Returns `Err(message)` to
/// mirror Go's `error`.
///
/// Go builds the result with `range` over the input map (random order), so a case
/// with more than one offending key would be non-deterministic; the parity corpus
/// therefore keeps at most one invalid key per case (and avoids two keys that trim
/// to the same name). This port iterates in sorted order, which yields the same
/// observable result under that constraint.
pub fn normalize_headers(
    headers: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (key, value) in headers {
        let clean = go_trim_space(key);
        if clean.is_empty() || clean.contains('\r') || clean.contains('\n') {
            return Err(format!("invalid HTTP header name {}", go_quote_string(key)));
        }
        out.insert(clean.to_string(), value.clone());
    }
    Ok(out)
}

/// `isUnsupportedVersion` (`mcphttp/client.go`): whether an error message (lower-cased)
/// contains `unsupported protocol version` or `unsupported_protocol_version`. The Go
/// function takes an `error` and only reads `err.Error()`, so the harness passes the
/// message string (the oracle wraps it with `errors.New`).
pub fn is_unsupported_version(msg: &str) -> bool {
    let msg = go_to_lower(msg);
    msg.contains("unsupported protocol version") || msg.contains("unsupported_protocol_version")
}
