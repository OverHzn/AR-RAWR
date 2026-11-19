//! Rust port of the pure helpers in the Go `norma/tool` package.
//!
//! `tool` is an external (read-only) module, so its *exported* pure functions are reached
//! through a standalone Go oracle (`oracle/`) rather than an in-package oracle. Only the
//! vendor-neutral, value-typed helpers are ported: the result constructors [`text`] /
//! [`errorf`], and the deferred-tools / todo-list formatters.

use llm_rs::port::{BLOCK_TEXT, ContentBlock, Message};
use serde::{Deserialize, Serialize};

/// `tool.SearchExtraToolsName` (`deferred.go`).
pub const SEARCH_EXTRA_TOOLS_NAME: &str = "SearchExtraTools";
/// `tool.ExecuteExtraToolName` (`deferred.go`).
pub const EXECUTE_EXTRA_TOOLS_NAME: &str = "ExecuteExtraTool";

/// `tool.TodoStatus` tags (`todo.go`).
pub const TODO_PENDING: &str = "pending";
/// `tool.TodoInProgress`.
pub const TODO_IN_PROGRESS: &str = "in_progress";
/// `tool.TodoCompleted`.
pub const TODO_COMPLETED: &str = "completed";

/// `tool.Todo` (`todo.go`): one item in a tool-managed todo list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

/// `tool.Result` (`result.go`): a tool's output. Go carries no JSON tags, so it marshals
/// PascalCase (`Content` / `IsError` / `Extra`). `Extra` is a nil slice for ordinary tools,
/// which Go renders as `null` — modeled as `Option` (no `skip`) so `None` → `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Result {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub extra: Option<Vec<Message>>,
}

/// `tool.Text` (`result.go`): a success result carrying a single text block.
pub fn text(s: &str) -> Result {
    Result {
        content: vec![ContentBlock {
            block_type: BLOCK_TEXT.to_string(),
            text: s.to_string(),
            ..ContentBlock::default()
        }],
        is_error: false,
        extra: None,
    }
}

/// `tool.Errorf` (`result.go`): an error result carrying a single text block.
pub fn errorf(s: &str) -> Result {
    Result {
        content: vec![ContentBlock {
            block_type: BLOCK_TEXT.to_string(),
            text: s.to_string(),
            ..ContentBlock::default()
        }],
        is_error: true,
        extra: None,
    }
}

/// `tool.RenderDeferredToolsBlock` (`deferred.go`): a model-facing block listing the
/// deferred tool names (sorted), with instructions to discover/invoke them. Empty for no
/// names. The body text mirrors the Go literals byte-for-byte (including the em-dash).
pub fn render_deferred_tools_block(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut sorted = names.to_vec();
    sorted.sort();
    let mut b = String::new();
    b.push_str("<available-deferred-tools>\n");
    for n in &sorted {
        b.push_str(n);
        b.push('\n');
    }
    b.push_str("</available-deferred-tools>\n");
    b.push_str(
        "IMPORTANT: The tools listed above are deferred \u{2014} they are NOT in your tool list. ",
    );
    b.push_str(&format!(
        "To use one you MUST first discover it via {SEARCH_EXTRA_TOOLS_NAME} (returns its schema), then invoke it via {EXECUTE_EXTRA_TOOLS_NAME}.\n"
    ));
    b.push_str(&format!(
        "Tool priority: use core tools for core tasks (e.g. bash/curl for HTTP); only use {EXECUTE_EXTRA_TOOLS_NAME} for a deferred tool when the task truly needs it."
    ));
    b
}

/// `tool.RenderTodoReminder` (`todo.go`): a numbered `[status] content` list, each line
/// `N. [status] content\n`, with trailing newlines trimmed. Empty for no todos.
pub fn render_todo_reminder(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return String::new();
    }
    let mut b = String::new();
    for (i, t) in todos.iter().enumerate() {
        b.push_str(&format!("{}. [{}] {}\n", i + 1, t.status, t.content));
    }
    // Go strings.TrimRight(s, "\n").
    while b.ends_with('\n') {
        b.pop();
    }
    b
}
