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
//! `llmpool::should_failover` reproduces that. A full async cancellation source
//! (tokio) is wired in when the `Provider` runtime lands.

pub use llm_rs::port::*;

use std::fmt;

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
            Error::Msg(s) => s.clone(),
        }
    }

    /// `errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded)`.
    pub fn is_context_err(&self) -> bool {
        matches!(self, Error::Canceled | Error::DeadlineExceeded)
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
