//! `sidequestion-rs` — behavioral-parity Rust port of the unexported,
//! side-effect-free helpers in the Go `sidequestion/` package.
//!
//! Only [`Parent::key`] is portable through the parity harness: it is a pure
//! method on the small value struct `Parent` (four `int64` fields). The other
//! pure-looking method, `Exchange::Running`, is skipped because `Exchange` is a
//! large IO-laden struct (`time.Time`, `llm.Usage`, …) that cannot be modelled as
//! a value type. Parity is proven against the real Go function via `record.sh`
//! (a temporary oracle over a corpus → `cases.jsonl`) and `tests/parity.rs`.

#![forbid(unsafe_code)]

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
