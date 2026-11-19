//! `transcript-rs` — behavioral-parity Rust port of the *pure* functions of the external
//! `github.com/Autumn-27/norma/transcript` package.
//!
//! `transcript` lives in the read-only Go module cache, so parity is proven with a
//! standalone oracle module (`oracle/`) that imports the real package and records its
//! outputs (see `record.sh`). Only the *exported* functions are reachable from that
//! external module, so this crate ports `Messages` and `LastUUID` — both value-typed
//! folds over `[]Record` with no time/IO/random surface. The `Store` writer/loader and
//! the `context`/`crypto/rand`-backed id helpers stay in Go.
//!
//! `Record.Timestamp` is a plain `string` in Go (not `time.Time`), so the record model
//! ports faithfully; only the field this crate reads (`uuid`, `record_type`, `message`,
//! `usage`) drives the ported functions.

#![forbid(unsafe_code)]

use llm_rs::port::{Message, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `transcript.Record`: one append-only transcript line. Only the fields read by the
/// ported functions are modelled; the rest (ids, timestamps, sidechain flag) round-trip
/// as inert strings/bools so recorded `Record` JSON decodes cleanly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub uuid: String,
    #[serde(default, rename = "parent_uuid")]
    pub parent_uuid: String,
    #[serde(default, rename = "session_id")]
    pub session_id: String,
    #[serde(default, rename = "agent_id")]
    pub agent_id: String,
    #[serde(default, rename = "is_sidechain")]
    pub is_sidechain: bool,
    #[serde(default)]
    pub timestamp: String,
    /// `"message"` | `"boundary"` (Go `Type`).
    #[serde(rename = "type")]
    pub record_type: String,
    /// The wrapped message; `nil` for boundary/empty records.
    #[serde(default)]
    pub message: Option<Message>,
    /// Assistant-turn token accounting.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Boundary metadata (only when `record_type == "boundary"`); unused by the ported
    /// functions but kept so recorded records round-trip.
    #[serde(default)]
    pub boundary: Option<Value>,
}

/// `transcript.Messages(recs) ([]llm.Message, llm.Usage)` — the conversation view of a
/// transcript: every non-boundary record that carries a message, with per-turn usage
/// summed across records that have it. Returns the collected messages and the running
/// `Usage` total.
pub fn messages(recs: &[Record]) -> (Vec<Message>, Usage) {
    let mut msgs = Vec::new();
    let mut total = Usage::default();
    for r in recs {
        if r.record_type == "boundary" {
            continue;
        }
        let Some(msg) = &r.message else {
            continue;
        };
        msgs.push(msg.clone());
        if let Some(u) = r.usage {
            total.add(u);
        }
    }
    (msgs, total)
}

/// `transcript.LastUUID(recs) string` — the `UUID` of the final record, or `""` when the
/// transcript is empty.
pub fn last_uuid(recs: &[Record]) -> String {
    match recs.last() {
        Some(r) => r.uuid.clone(),
        None => String::new(),
    }
}
