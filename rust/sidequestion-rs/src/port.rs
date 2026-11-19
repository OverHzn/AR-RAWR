//! `sidequestion-rs` — behavioral-parity Rust port of the side-effect-free helpers in
//! the Go `sidequestion/` package.
//!
//! Portable so far: [`Parent::key`] (a pure method on the small value struct `Parent`),
//! `outputBudget` / `inputBudget` (struct-param functions that each read a single scalar
//! field), `messageGroups` (a pure `[]llm.Message` → `[][]llm.Message` grouping), and
//! `EstimateInputTokens` (a pure `llm.CompletionRequest` → `int` estimate over the
//! `norma/llm` model in `llm-rs` and `norma/compaction`'s estimator in `compaction-rs`).
//! The remaining functions are not portable: `Exchange::Running` is a method on a large
//! IO-laden struct, `assemble`/`exchangeMessages` need `time.Time` RFC3339 formatting, and
//! `BuildRequest` (which would use `CloneRequest` + `EstimateInputTokens`) is blocked on
//! `assemble`'s `time.Time`. Parity is proven against the real Go functions via
//! `record.sh` (oracle → `cases.jsonl`) and `tests/parity.rs`.

#![forbid(unsafe_code)]

use std::collections::HashSet;

use compaction_rs::port::estimate_tokens;
use llm_rs::port::{BLOCK_TOOL_RESULT, BLOCK_TOOL_USE, CompletionRequest, Message};

/// Port of Go `sidequestion.Parent` (sidequestion/capture.go).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parent {
    pub conversation_id: i64,
    pub task_id: i64,
    pub exploration_id: i64,
    pub intent_id: i64,
}

impl Parent {
    /// `Parent.Key` (sidequestion/capture.go).
    pub fn key(&self) -> String {
        if self.conversation_id > 0 {
            format!("conv-{}", self.conversation_id)
        } else if self.intent_id > 0 {
            format!(
                "task-{}-exp-{}-worker-i{}",
                self.task_id, self.exploration_id, self.intent_id
            )
        } else {
            format!("task-{}-exp-{}-main", self.task_id, self.exploration_id)
        }
    }
}

/// `outputBudget` (sidequestion/request.go): the configured output-token budget,
/// defaulting to `DefaultOutputTokens` (8192) when `<= 0`, capped at 32768 and at
/// the request's `MaxTokens` when set. The `llm.CompletionRequest` contributes only
/// its `MaxTokens`, so the port takes that scalar.
pub fn output_budget(max_tokens: i64, configured: i64) -> i64 {
    let mut configured = if configured <= 0 { 8192 } else { configured };
    configured = configured.min(32768);
    if max_tokens > 0 {
        configured = configured.min(max_tokens);
    }
    configured
}

/// `inputBudget` (sidequestion/request.go): the model's context window (default
/// 200000 when `<= 0`) minus the output budget and a dynamic reserve
/// (`min(8192, max(128, window/20))`). `Snapshot` contributes only
/// `Model.WindowTokens`, so the port takes that scalar.
pub fn input_budget(window_tokens: i64, output: i64) -> i64 {
    let window = if window_tokens <= 0 {
        200_000
    } else {
        window_tokens
    };
    let reserve = 8192.min(128.max(window / 20));
    window - output - reserve
}

/// `messageGroups` (sidequestion/context.go): split a conversation into groups that each
/// leave no tool call unresolved. A `tool_use` block opens a pending id; the matching
/// `tool_result` (`tool_use_id`) closes it. Whenever nothing is pending after a message,
/// that message closes a group; any trailing run with an open tool call is kept as the
/// final group. Returns `[][]Message` (empty when the input is empty).
pub fn message_groups(messages: &[Message]) -> Vec<Vec<Message>> {
    let mut groups: Vec<Vec<Message>> = Vec::new();
    let mut pending: HashSet<String> = HashSet::new();
    let mut start = 0usize;
    for (i, m) in messages.iter().enumerate() {
        for block in &m.content {
            if block.block_type == BLOCK_TOOL_USE {
                pending.insert(block.id.clone());
            }
            if block.block_type == BLOCK_TOOL_RESULT {
                pending.remove(&block.tool_use_id);
            }
        }
        if pending.is_empty() {
            groups.push(messages[start..=i].to_vec());
            start = i + 1;
        }
    }
    if start < messages.len() {
        groups.push(messages[start..].to_vec());
    }
    groups
}

/// `EstimateInputTokens` (sidequestion/request.go): a token estimate for a request,
/// following norma's byte-based block estimate with a 4/3 safety factor plus framing
/// costs. `compaction.EstimateTokens(messages)*4/3 + 32 + len(messages)*8`, then per
/// system segment `(len+2)/3 + 8` and per tool `(len(json)+2)/3 + 8`. Integer math.
pub fn estimate_input_tokens(req: &CompletionRequest) -> i64 {
    let mut tokens = estimate_tokens(&req.messages) * 4 / 3 + 32 + req.messages.len() as i64 * 8;
    for text in &req.system {
        tokens += (text.len() as i64 + 2) / 3 + 8;
    }
    for tool in &req.tools {
        // Go `json.Marshal(tool)`: ToolSchema marshals name/description/input_schema with
        // sorted map keys (serde_json's default Map is a BTreeMap). The oracle keeps tool
        // schemas compact ASCII so the byte length matches.
        let b = serde_json::to_string(tool).unwrap_or_default();
        tokens += (b.len() as i64 + 2) / 3 + 8;
    }
    tokens
}
