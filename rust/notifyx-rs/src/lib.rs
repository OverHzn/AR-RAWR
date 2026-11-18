//! `notifyx-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! config helpers in the Go `notify/` package (`cfgString`, `cfgInt`, `cfgBool`,
//! `cfgStrings`, `cfgMap`, `ValidKind`, `Kinds`).
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. They take a
//! `map[string]any` (the notification config) — a composite map parameter — so the
//! parity harness passes the config as a JSON object and reads keys from it. Parity
//! is proven against the real Go functions via `record.sh` + `tests/parity.rs`.
//! Go-stdlib string semantics come from `gocompat::strings`.

#![forbid(unsafe_code)]

pub mod port;
