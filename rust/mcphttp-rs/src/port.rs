//! The Rust port of `mcphttp/`.
//!
//! Faithful port of the pure `ToolName` (`mcphttp/client.go:35`). The pointer/`context.Context`
//! constructors (`New`, `NewSSE`) stay in Go. `rust-anything diff` checks behavior against
//! what the source recorded.

#![forbid(unsafe_code)]

/// Port of `ToolName` (`mcphttp/client.go:35`).
///
/// `ToolName` mirrors `mcp.ToolName` so remote tools share the `mcp__server__tool` scheme.
pub fn tool_name(server: String, name: String) -> Result<String, String> {
    Ok(format!("mcp__{server}__{name}"))
}
