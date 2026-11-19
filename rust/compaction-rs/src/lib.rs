//! `compaction-rs` — pure functions of the external `norma/compaction` package
//! (`EstimateTokens`, `IsOverflow`), parity-tested against cases recorded from the Go
//! original. Not reachable by `rust-anything`'s bridge (external package / error-typed
//! param), so parity uses a standalone Go oracle module (see `oracle/`) + `tests/parity.rs`.

#![forbid(unsafe_code)]

pub mod port;
