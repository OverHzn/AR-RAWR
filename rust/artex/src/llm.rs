//! LLM message/request model — the Rust substrate for the agent/llm packages.
//!
//! The authoritative port of the external `norma/llm` model lives in `llm-rs`
//! (parity-tested against the real Go `llm` package). We re-export it here so the
//! rest of the `artex` crate builds against `crate::llm` rather than reaching into
//! the parity crate directly.
//!
//! On top of the data model this module also provides the minimal `context` +
//! error surface the LLM layer needs (`Context::err`, the `Canceled` /
//! `DeadlineExceeded` sentinels). Go passes a `context.Context` through every
//! provider call and `errors.Is(err, context.Canceled)` to detect cancellation;
//! `llmpool::should_failover` reproduces that. Concrete providers (Anthropic/OpenAI
//! RPC clients) land with the agent/runtime layer that drives them.

pub use llm_rs::port::*;

use std::fmt;

use async_trait::async_trait;

/// Minimal port of the sentinel errors the LLM layer branches on.
///
/// Go has package-level sentinels `context.Canceled` and `context.DeadlineExceeded`
/// matched with `errors.Is`. Everything else a provider returns is an opaque
/// `error` whose text carries the HTTP status (`"…: status <code>: <body>"`). We
/// model that split directly: two cancellation variants plus a free-form message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `context.Canceled` — the caller stopped the task.
    Canceled,
    /// `context.DeadlineExceeded` — a task-level timeout fired.
    DeadlineExceeded,
    /// The pool's `ErrExhausted` wrap: `fmt.Errorf("%w: %v", ErrExhausted, last)`.
    /// Carries the fully-formatted message text; matched with [`Error::is_exhausted`]
    /// (the Rust analogue of `errors.Is(err, ErrExhausted)`).
    Exhausted(String),
    /// Any other provider/transport error; the message keeps the Go `err.Error()`
    /// text so `status_of` can read the HTTP status out of it.
    Msg(String),
}

impl Error {
    /// Build a free-form provider error from its `err.Error()` text.
    pub fn msg(s: impl Into<String>) -> Self {
        Error::Msg(s.into())
    }

    /// The Go `err.Error()` text.
    pub fn message(&self) -> String {
        match self {
            Error::Canceled => "context canceled".to_string(),
            Error::DeadlineExceeded => "context deadline exceeded".to_string(),
            Error::Exhausted(s) | Error::Msg(s) => s.clone(),
        }
    }

    /// `errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded)`.
    pub fn is_context_err(&self) -> bool {
        matches!(self, Error::Canceled | Error::DeadlineExceeded)
    }

    /// `errors.Is(err, llmpool.ErrExhausted)`.
    pub fn is_exhausted(&self) -> bool {
        matches!(self, Error::Exhausted(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for Error {}

/// Minimal port of Go's `context.Context`, reduced to the cancellation surface the
/// LLM layer reads (`ctx.Err()`). `Background` never cancels; the other
/// constructors model an already-cancelled context for tests and for the server to
/// map a real cancellation source onto.
#[derive(Debug, Clone, Default)]
pub struct Context {
    err: Option<Error>,
}

impl Context {
    /// `context.Background()`: a context that is never cancelled (`Err() == nil`).
    pub fn background() -> Self {
        Context::default()
    }

    /// A context already cancelled with `context.Canceled`.
    pub fn canceled() -> Self {
        Context {
            err: Some(Error::Canceled),
        }
    }

    /// A context already past its deadline.
    pub fn deadline_exceeded() -> Self {
        Context {
            err: Some(Error::DeadlineExceeded),
        }
    }

    /// `ctx.Err()`: `Some` once cancelled, else `None`.
    pub fn err(&self) -> Option<&Error> {
        self.err.as_ref()
    }

    /// Whether this context is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.err.is_some()
    }
}

// === Streaming + Provider (norma/llm provider.go) ===

// `llm.StreamEventType` values. Modeled as string constants (like `llm-rs`'s block
// types) so a `StreamEvent`'s `ty` compares directly against them.
pub const SE_MESSAGE_START: &str = "message_start";
pub const SE_TEXT_DELTA: &str = "text_delta";
pub const SE_THINKING_DELTA: &str = "thinking_delta";
pub const SE_THINKING_SIGNATURE: &str = "thinking_signature";
pub const SE_TOOL_USE_START: &str = "tool_use_start";
pub const SE_TOOL_INPUT_JSON: &str = "tool_input_delta";
pub const SE_TOOL_USE_SIGNATURE: &str = "tool_use_signature";
pub const SE_MESSAGE_DELTA: &str = "message_delta";
pub const SE_MESSAGE_STOP: &str = "message_stop";
pub const SE_REDACTED_THINKING: &str = "redacted_thinking";

/// `llm.StreamEvent`: one normalized event from a streaming completion.
///
/// Which fields are populated depends on `ty` (text/thinking/tool_input JSON
/// fragment in `text`, tool id/name on tool_use_start, stop reason + usage on
/// message_delta, usage on message_start).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamEvent {
    pub ty: String,
    pub text: String,
    pub tool_id: String,
    pub tool_name: String,
    pub stop_reason: String,
    /// Provider-assigned tool_calls[] index (OpenAI parallel calls); 0 elsewhere.
    pub tool_index: i64,
    /// Usage on message_start / message_delta (fold with `Usage::merge_echo`).
    pub usage: Usage,
}

/// `llm.Provider`: produces a completion, streamed or in one non-streaming
/// round-trip. Hosts implement it to plug in private RPC backends; `llmpool::Pool`
/// is a decorator that fails over across a chain of them.
///
/// The Go `Stream` returns `iter.Seq2[StreamEvent, error]` (a push iterator whose
/// `yield` returns `false` when the consumer stops). The Rust equivalent is a
/// `Stream<Item = Result<StreamEvent, Error>>`: a terminal error is delivered as
/// `Some(Err(_))` and ends the stream, and dropping the stream is how a consumer
/// "stops early" (the analogue of `yield` returning `false`).
#[async_trait]
pub trait Provider: Send + Sync {
    /// Non-streaming completion: the fully-assembled assistant message, the
    /// provider stop reason, and cumulative usage.
    async fn complete(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error>;

    /// Streaming completion: a stream of normalized events; a terminal error is
    /// delivered as `Some(Err(_))`.
    fn stream(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> futures::stream::BoxStream<'static, Result<StreamEvent, Error>>;
}
