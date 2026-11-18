//! `llmrecx-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helpers in the Go `llmrec/` package that are reachable without the external
//! `norma/llm` types or `context`, verified against the Go original via a recorded
//! oracle (the bridge can't reach unexported functions).
//!
//! `llmrec`'s other unexported surface needs `context` (`workerFrom`, `reportStop`)
//! or the external `norma/llm.Message` (`thinkingText`) and stays in Go.

#![forbid(unsafe_code)]

pub mod port;
