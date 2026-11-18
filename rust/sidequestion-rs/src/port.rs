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
