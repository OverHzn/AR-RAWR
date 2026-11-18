//! `server-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `server/` package.
//!
//! These functions are not reachable through `rust-anything`'s bridge (which only
//! bridges exported, pointer-free functions), so parity is proven differently:
//! a temporary Go oracle ran the *real* Go functions over a corpus and recorded
//! `cases.jsonl` (5910 cases). The `tests/parity.rs` harness replays that corpus
//! and asserts byte-identical output. The Go original was not modified.
//!
//! This crate intentionally uses `#![forbid(unsafe_code)]`.

#![forbid(unsafe_code)]
#![allow(clippy::needless_range_loop)]

pub mod port;
