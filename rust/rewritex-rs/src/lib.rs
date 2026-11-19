//! `rewritex-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helper functions in the Go `traffic/rewrite/` package that the `rewrite-rs`
//! bridge leaves behind: header-name token validation and canonicalisation
//! (`net/http.CanonicalHeaderKey` reproduced byte-for-byte over
//! `net/textproto.CanonicalMIMEHeaderKey`), the NUL-in-head scan, the action
//! execution-class table, and the body-allowed-for-status predicate.
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions via `record.sh` (a temporary oracle over a
//! corpus → `cases.jsonl`) and `tests/parity.rs`.

#![forbid(unsafe_code)]

pub mod port;
