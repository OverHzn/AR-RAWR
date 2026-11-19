//! `truncate.rs` — behavioral-parity Rust port of `noa.TruncateLargeToolOutputs` and
//! its helper `truncateBody` from the external `norma/noa` package (`truncate.go`).
//!
//! This is the mechanical last resort: when the context is about to overflow, the
//! largest tool results are cut down regardless of the model's cooperation. It is
//! pure and deterministic (rune-slicing only, no IO). The `TokenCountFn` callback is
//! modeled as `fn(&str) -> i64` (the oracle resolves a nil Go callback to
//! `DefaultCountTokens`).
//!
//! Faithful details:
//! * `Messages` is the input unchanged when nothing is cut (preserving a Go nil slice
//!   as `null`); after a cut it is a non-nil copy. Modeled as `Option<Vec<_>>` so the
//!   nil-vs-array distinction round-trips.
//! * A result is a cut candidate only if it is a non-empty tool result, not already
//!   truncated (`TruncationMarker` absent), and at least `truncateMinOutputTokens`.
//! * The body guard skips results whose rune length is `<= keepPrefix+keepSuffix`.
//! * Candidates are cut largest-first (stable), stopping once the target is reached.

#![forbid(unsafe_code)]

use crate::port::CoreMessage;

/// `noa.truncateMinOutputTokens` — a result cheaper than this is not worth cutting.
const TRUNCATE_MIN_OUTPUT_TOKENS: i64 = 1000;
/// `noa.truncateKeepPrefixChars` — runes of head kept verbatim.
const TRUNCATE_KEEP_PREFIX_CHARS: usize = 2000;
/// `noa.truncateKeepSuffixChars` — runes of tail kept verbatim.
const TRUNCATE_KEEP_SUFFIX_CHARS: usize = 2000;
/// `noa.TruncationMarker` — substring identifying an already-truncated result.
pub const TRUNCATION_MARKER: &str = "[truncated for context space";

/// `noa.TruncateResult` — the truncation outcome.
#[derive(Debug, Clone, Default)]
pub struct TruncateResult {
    /// `Messages` — the (possibly shortened) view; `None` models a Go nil slice.
    pub messages: Option<Vec<CoreMessage>>,
    pub truncated_count: i64,
    pub saved_tokens: i64,
}

/// `noa.truncateBody` — keep the head and tail runes, replace the middle with a marker.
fn truncate_body(s: &str, tokens: i64) -> String {
    let r: Vec<char> = s.chars().collect();
    let head: String = r[..TRUNCATE_KEEP_PREFIX_CHARS].iter().collect();
    let tail: String = r[r.len() - TRUNCATE_KEEP_SUFFIX_CHARS..].iter().collect();
    format!(
        "{}\n\n...{} — original ~{} tokens]...\n\n{}",
        head, TRUNCATION_MARKER, tokens, tail
    )
}

/// `noa.TruncateLargeToolOutputs` — mechanically shorten the largest tool results.
///
/// `msgs_in` is `None` for a Go nil slice. `count` is the resolved `TokenCountFn`.
pub fn truncate_large_tool_outputs(
    msgs_in: Option<&[CoreMessage]>,
    token_count: i64,
    cfg: &crate::port::Config,
    count: fn(&str) -> i64,
) -> TruncateResult {
    let input_was_none = msgs_in.is_none();
    let msgs: Vec<CoreMessage> = msgs_in.map(|s| s.to_vec()).unwrap_or_default();

    // No truncation possible without a positive limit and threshold.
    if cfg.model_context_limit <= 0 || cfg.truncate.threshold <= 0.0 {
        return TruncateResult {
            messages: if input_was_none { None } else { Some(msgs) },
            truncated_count: 0,
            saved_tokens: 0,
        };
    }
    let threshold = (cfg.truncate.threshold * cfg.model_context_limit as f64) as i64;
    if token_count < threshold {
        return TruncateResult {
            messages: if input_was_none { None } else { Some(msgs) },
            truncated_count: 0,
            saved_tokens: 0,
        };
    }
    // Aim below the threshold: landing exactly on it would trigger again next turn.
    let target = (threshold as f64 * 0.9) as i64;

    let protect_recent = std::cmp::max(cfg.preserve_recent_messages, 0);
    let cutoff = msgs.len() as i64 - protect_recent;

    let mut candidates: Vec<(usize, i64)> = Vec::new(); // (index, tokens)
    for (i, m) in msgs.iter().enumerate() {
        if (i as i64) >= cutoff {
            break;
        }
        if m.content_type != crate::port::CT_TOOL_RESULT || m.text.is_empty() {
            continue;
        }
        if m.text.contains(TRUNCATION_MARKER) {
            continue; // already truncated; cutting it again reclaims nothing
        }
        let n = count(&m.text);
        if n < TRUNCATE_MIN_OUTPUT_TOKENS {
            continue;
        }
        candidates.push((i, n));
    }
    if candidates.is_empty() {
        return TruncateResult {
            messages: if input_was_none { None } else { Some(msgs) },
            truncated_count: 0,
            saved_tokens: 0,
        };
    }
    // Largest first: the fewest edits that get under the target.
    candidates.sort_by_key(|&(_, tokens)| std::cmp::Reverse(tokens));

    let mut out = msgs.clone();
    let mut saved = 0i64;
    let mut truncated_count = 0i64;
    for (idx, c_tokens) in candidates {
        if token_count - saved <= target {
            break;
        }
        let orig = out[idx].text.clone();
        if orig.chars().count() <= TRUNCATE_KEEP_PREFIX_CHARS + TRUNCATE_KEEP_SUFFIX_CHARS {
            continue;
        }
        let replaced = truncate_body(&orig, c_tokens);
        saved += c_tokens - count(&replaced);
        out[idx].text = replaced;
        truncated_count += 1;
    }
    if truncated_count == 0 {
        return TruncateResult {
            messages: if input_was_none { None } else { Some(msgs) },
            truncated_count: 0,
            saved_tokens: 0,
        };
    }
    TruncateResult {
        messages: Some(out),
        truncated_count,
        saved_tokens: saved,
    }
}
