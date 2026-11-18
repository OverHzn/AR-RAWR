//! `mcphttpx-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helper functions in the Go `mcphttp/` package that the `mcphttp-rs` bridge
//! leaves behind: header-map normalization and the unsupported-protocol-version
//! check.
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions via `record.sh` (a temporary oracle over a
//! corpus → `cases.jsonl`) and `tests/parity.rs`. Go-stdlib string/quoting
//! semantics come from `gocompat`.

#![forbid(unsafe_code)]

pub mod port;
