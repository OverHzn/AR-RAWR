//! `guard-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `guard/` package (`capBlockReason`, `systemBlockMessage`).
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions: `record.sh` runs a temporary oracle over
//! a corpus and writes `cases.jsonl`; `tests/parity.rs` replays it and asserts
//! byte-identical output. The Go original is untouched.

#![forbid(unsafe_code)]

/// guard/guard.go `maxBlockReasonRunes`.
const MAX_BLOCK_REASON_RUNES: usize = 4000;

/// `capBlockReason` (guard/guard.go): keep at most `maxBlockReasonRunes` runes,
/// appending ` …(truncated)` if longer.
pub fn cap_block_reason(s: &str) -> String {
    for (n, (i, _)) in s.char_indices().enumerate() {
        if n == MAX_BLOCK_REASON_RUNES {
            return format!("{} …(truncated)", &s[..i]);
        }
    }
    s.to_string()
}

/// `systemBlockMessage` (guard/guard.go).
pub fn system_block_message(reason: &str) -> String {
    format!(
        "[ARTEX platform control, not target defense] This call was intercepted by the platform. \
Reason: {}. This operation is forbidden.",
        cap_block_reason(reason)
    )
}
