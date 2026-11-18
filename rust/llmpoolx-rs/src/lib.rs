//! `llmpoolx-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `llmpool/` package that are reachable without the heavy
//! `norma/llm` types or regexp, verified against the Go original via a recorded
//! oracle (the bridge can't reach unexported functions).
//!
//! `llmpool`'s other unexported surface is pointer-based (`rotateGroups`), needs
//! regexp (`statusOf`/`isHardFailure` via `statusRe`), context, or the external
//! `norma/llm` structs (`estimateTokens`, `blocksLen`), and stays in Go.

#![forbid(unsafe_code)]

pub mod port;
