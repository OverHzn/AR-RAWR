//! `transcript-rs` — Rust port of the *pure* functions of the external
//! `github.com/Autumn-27/norma/transcript` package (`Messages`, `LastUUID`).
//!
//! `transcript` is built on the `llm` value model (see `llm-rs`): a `Record` wraps an
//! optional `llm.Message` plus optional `llm.Usage`, and `Messages` folds a `[]Record`
//! into the `(messages, usage)` pair the API layer consumes. Parity is proven against
//! the *real* Go `norma/transcript` package via a standalone oracle module (`oracle/`)
//! that imports it and prints recorded cases; only exported functions are reachable, so
//! the `Store` writer/loader and the `crypto/rand` id helpers stay in Go.
//!
//! `Record.Timestamp` is a Go `string` (not `time.Time`), so the record model ports
//! faithfully and these two functions are a clean, value-typed win.

#![forbid(unsafe_code)]

pub mod port;
