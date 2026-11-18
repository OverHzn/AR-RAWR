//! `dbx-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helper functions in the Go `db/` package that the primitive sweep and the
//! `db-rs` bridge both leave behind: Postgres-array marshalling, the DSL
//! identifier byte predicates, and the nil-able scalar helpers.
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions via `record.sh` (a temporary oracle over a
//! corpus → `cases.jsonl`) and `tests/parity.rs`.

#![forbid(unsafe_code)]

pub mod port;
