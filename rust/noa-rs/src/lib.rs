//! `noa-rs` — Rust port of the *pure, value-typed* helpers of the external
//! `github.com/Autumn-27/norma/noa` package (context-compression engine): the
//! message-ref / block-id codec, token estimation, and token formatting. Parity is
//! proven against the *real* Go `norma/noa` package via a standalone oracle module
//! (`oracle/`) that imports it and prints recorded cases. The whole package imports
//! only the standard library, so the oracle has no heavy deps.

#![forbid(unsafe_code)]

pub mod nudgetext;
pub mod parseargs;
pub mod port;
