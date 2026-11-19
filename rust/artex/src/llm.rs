//! LLM message/request model — the Rust substrate for the agent/llm packages.
//!
//! The authoritative port of the external `norma/llm` model lives in `llm-rs`
//! (parity-tested against the real Go `llm` package). We re-export it here so the
//! rest of the `artex` crate builds against `crate::llm` rather than reaching into
//! the parity crate directly.

pub use llm_rs::port::*;
