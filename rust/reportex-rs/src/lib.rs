//! `reportex-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! string helpers in the Go `report/` package (`nz`, `oneLine`, `unbalancedFence`,
//! `staleReportNotice`, `reportStale`, `verificationLabel`, `detailedReportLimit`).
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions via `record.sh` (a temporary oracle over a
//! corpus → `cases.jsonl`) and `tests/parity.rs`. Shared Go-stdlib string helpers
//! come from the `gocompat` crate.

#![forbid(unsafe_code)]

pub mod port;
