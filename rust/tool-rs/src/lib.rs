//! `tool-rs` — pure helpers of the external `norma/tool` package (`Text`, `Errorf`,
//! `RenderDeferredToolsBlock`, `RenderTodoReminder`), parity-tested against cases recorded
//! from the Go original. The rest of `norma/tool` (bash/web/grep/ripgrep/shell) is IO-bound
//! and stays in Go.

#![forbid(unsafe_code)]

pub mod port;
