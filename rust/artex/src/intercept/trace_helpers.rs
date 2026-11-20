//! Pure trace helpers — slice of `intercept/trace.go`.
//!
//! `digestInput` canonicalizes a JSON body before hashing so semantically equal
//! requests get one digest; `bounded` truncates on a UTF-8 rune boundary.

use sha2::Digest;

/// `digestInput`: canonicalize JSON (decode + re-encode) then sha256 hex.
pub fn digest_input(input: &[u8]) -> String {
    let bytes = if let Ok(v) = serde_json::from_slice::<serde_json::Value>(input) {
        serde_json::to_vec(&v).unwrap_or_else(|_| input.to_vec())
    } else {
        input.to_vec()
    };
    let sum = sha2::Sha256::digest(&bytes);
    let mut out = String::with_capacity(sum.len() * 2);
    for b in sum {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `bounded`: truncate to `limit` bytes on a rune boundary; returns (text, cut).
pub fn bounded(s: &str, limit: usize) -> (String, bool) {
    if s.len() <= limit {
        return (s.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}
