//! `agentx-rs` — behavioral-parity Rust port of a batch of unexported, side-effect-free
//! helper functions in the Go `agent/` package that the `agent-rs` bridge leaves
//! behind: integer budget math and a set of pure string/normalization helpers.
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions via `record.sh` (a temporary oracle over a
//! corpus → `cases.jsonl`) and `tests/parity.rs`. Go-stdlib string semantics come
//! from `gocompat`. Constants referenced by the originals (`Unlimited`, the
//! related-overview budget, the fact-source set, the worker chat-marker prefix) are
//! inlined with the exact values from the Go source.

#![forbid(unsafe_code)]

pub mod port;
