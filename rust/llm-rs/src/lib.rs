//! `llm-rs` — behavioral-parity Rust port of the *pure* functions of the external
//! `github.com/Autumn-27/norma/llm` message model (`Message.Text`, `Message.ToolUses`,
//! `Usage.Add`, `Usage.MergeEcho`) together with the underlying data types
//! (`ContentBlock`, `Message`, `Usage`, the `Role`/`BlockType` string tags).
//!
//! Parity is proven against the *real* Go `norma/llm` package. Unlike the in-repo
//! packages, `norma/llm` lives in the read-only Go module cache, so `record.sh` cannot
//! copy an oracle into it; instead it runs a standalone oracle module (`oracle/`) that
//! `import`s `norma/llm` and prints the recorded cases. This is the foundation unit that
//! later unblocks the ARTEX packages built on the LLM model (`llmpool.blocksLen` /
//! `estimateTokens`, `llmrec.thinkingText`, …).
//!
//! The wire adapters (Anthropic/OpenAI request builders) and streaming state machines in
//! `norma/llm` are I/O-bound and stay in Go; only the vendor-neutral value model is ported.

#![forbid(unsafe_code)]

pub mod port;
