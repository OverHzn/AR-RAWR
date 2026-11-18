//! Map Rust `regex` crate compile errors to Go `regexp` error text.
//!
//! The `regex` crate is RE2-like (the same engine family Go's `regexp` uses), so
//! patterns compile identically; only the human-readable *reason* string differs.
//! [`go_regex_error`] translates the common cases so `diff` strict-matches Go's
//! `error parsing regexp: <reason>: `<expr>`` message.

/// Translate a `regex::Error` into Go's `regexp.Compile` error string.
pub fn go_regex_error(e: &regex::Error, expr: &str) -> String {
    let msg = e.to_string();
    let reason = if msg.contains("unclosed group") {
        "missing closing )"
    } else if msg.contains("unclosed character class") {
        "missing closing ]"
    } else if msg.contains("repetition operator missing expression") {
        "missing argument to repetition operator"
    } else if msg.contains("unclosed counted repetition") {
        "missing closing }"
    } else if msg.contains("unclosed capture group name") {
        "missing closing >"
    } else if msg.contains("incomplete escape sequence") {
        "trailing backslash at end of expression"
    } else {
        // Fallback: use the Rust message body (best effort).
        msg.lines().last().unwrap_or("invalid regexp").trim()
    };
    format!("error parsing regexp: {reason}: `{expr}`")
}