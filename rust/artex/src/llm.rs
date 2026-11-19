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

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

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

/// Minimal port of Go's `context.Context`: the cancellation surface the LLM layer
/// reads (`ctx.Err()`) plus a type-keyed value bag mirroring `context.WithValue` /
/// `ctx.Value`.
///
/// `Background` never cancels; the other constructors model an already-cancelled
/// context for tests and for the server to map a real cancellation source onto.
/// Values are keyed by their type's [`TypeId`] — exactly how the sidequestion
/// capture threads its checkpoint sink through the provider pool, and how Go uses
/// private `struct{}` keys. [`with_value`](Context::with_value) returns a child
/// carrying one more value (copy-on-write, so cloning a `Context` stays cheap);
/// [`value`](Context::value) reads it back.
#[derive(Clone, Default)]
pub struct Context {
    err: Option<Error>,
    values: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The value bag is type-erased and not itself Debug; report only its size.
        f.debug_struct("Context")
            .field("err", &self.err)
            .field("values", &self.values.len())
            .finish()
    }
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
            values: Default::default(),
        }
    }

    /// A context already past its deadline.
    pub fn deadline_exceeded() -> Self {
        Context {
            err: Some(Error::DeadlineExceeded),
            values: Default::default(),
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

    /// `context.WithValue(ctx, key{}, value)`: a child context that also carries
    /// `value` under its type `T`. The parent's cancellation and other values are
    /// preserved. `T` is the key, so each distinct value type occupies its own slot
    /// — the same role a private `struct{}` key plays in Go.
    pub fn with_value<T: Any + Send + Sync>(&self, value: Arc<T>) -> Self {
        let mut values = (*self.values).clone();
        values.insert(TypeId::of::<T>(), value);
        Context {
            err: self.err.clone(),
            values: Arc::new(values),
        }
    }

    /// `ctx.Value(key)`: the value stored under type `T`, if this context (or a
    /// parent) carries one.
    pub fn value<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.values
            .get(&TypeId::of::<T>())
            .and_then(|v| Arc::clone(v).downcast::<T>().ok())
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

// === Stream accumulation (norma/llm accumulate.go) ===

/// `llm.Accumulator`: reassembles a complete assistant [`Message`] from a stream of
/// normalized [`StreamEvent`]s. The harness feeds every event to
/// [`add`](Accumulator::add); once the stream ends, [`message`](Accumulator::message)
/// returns the finished assistant turn (text, thinking, and tool_use blocks in
/// arrival order).
///
/// A faithful port of `norma/llm/accumulate.go`. Partial tool inputs are validated
/// on finalization; malformed or empty JSON falls back to `{}`, matching Go's
/// `json.Valid` check.
#[derive(Debug, Default)]
pub struct Accumulator {
    blocks: Vec<ContentBlock>,
    /// Per-`tool_use` partial input JSON and thought signature, positionally aligned
    /// with the `tool_use` blocks in `blocks`.
    tool_inputs: Vec<String>,
    tool_sigs: Vec<String>,
    /// `StreamEvent.ToolIndex` → position in `tool_inputs`/`tool_sigs`, so a
    /// signature frame naming index 0 lands on call 0 even after call 1 has started.
    tool_pos: HashMap<i64, usize>,
    text_buf: Option<String>,
    think_buf: Option<String>,
    think_sig: String,
    stop_reason: String,
    usage: Usage,
}

impl Accumulator {
    /// `llm.NewAccumulator`: an empty accumulator.
    pub fn new() -> Self {
        Accumulator::default()
    }

    /// `Accumulator.Add`: fold one event into the accumulator.
    pub fn add(&mut self, ev: StreamEvent) {
        match ev.ty.as_str() {
            SE_MESSAGE_START => self.usage.add(ev.usage),
            SE_TEXT_DELTA => {
                if self.text_buf.is_none() {
                    self.flush_thinking();
                    self.text_buf = Some(String::new());
                }
                self.text_buf.as_mut().unwrap().push_str(&ev.text);
            }
            SE_THINKING_DELTA => {
                if self.think_buf.is_none() {
                    self.flush_text();
                    self.think_buf = Some(String::new());
                }
                self.think_buf.as_mut().unwrap().push_str(&ev.text);
            }
            SE_THINKING_SIGNATURE => {
                // With thinking display omitted the block has a signature but no
                // thinking deltas; open it so the signed block is not lost.
                if self.think_buf.is_none() {
                    self.flush_text();
                    self.think_buf = Some(String::new());
                }
                self.think_sig = ev.text;
            }
            SE_REDACTED_THINKING => {
                self.flush_text();
                self.flush_thinking();
                self.blocks.push(ContentBlock {
                    block_type: BLOCK_REDACTED_THINKING.to_string(),
                    data: ev.text,
                    ..Default::default()
                });
            }
            SE_TOOL_USE_START => {
                self.flush_text();
                self.flush_thinking();
                self.blocks.push(ContentBlock {
                    block_type: BLOCK_TOOL_USE.to_string(),
                    id: ev.tool_id,
                    name: ev.tool_name,
                    ..Default::default()
                });
                self.tool_inputs.push(String::new());
                self.tool_sigs.push(String::new());
                let pos = self.tool_sigs.len() - 1;
                self.tool_pos.insert(ev.tool_index, pos);
            }
            SE_TOOL_INPUT_JSON => {
                if let Some(buf) = self.tool_inputs.last_mut() {
                    buf.push_str(&ev.text);
                }
            }
            SE_TOOL_USE_SIGNATURE => {
                // Prefer the provider's index; adapters that do not set one
                // (Anthropic, the Responses API) leave it 0, which for them means
                // "the call in flight" — the fallback covers that and an index we
                // never saw start.
                if let Some(&pos) = self.tool_pos.get(&ev.tool_index) {
                    self.tool_sigs[pos] = ev.text;
                } else if let Some(last) = self.tool_sigs.last_mut() {
                    *last = ev.text;
                }
            }
            SE_MESSAGE_DELTA => {
                if !ev.stop_reason.is_empty() {
                    self.stop_reason = ev.stop_reason;
                }
                self.usage.merge_echo(ev.usage);
            }
            SE_MESSAGE_STOP => {
                self.flush_text();
                self.flush_thinking();
            }
            _ => {}
        }
    }

    fn flush_text(&mut self) {
        if let Some(buf) = self.text_buf.take() {
            if !buf.is_empty() {
                self.blocks.push(text_block(&buf));
            }
        }
    }

    fn flush_thinking(&mut self) {
        if self.think_buf.is_some() {
            let buf = self.think_buf.take().unwrap();
            if !buf.is_empty() || !self.think_sig.is_empty() {
                self.blocks.push(ContentBlock {
                    block_type: BLOCK_THINKING.to_string(),
                    thinking: buf,
                    signature: self.think_sig.clone(),
                    ..Default::default()
                });
            }
            // Go resets the signature whether or not a block was emitted.
            self.think_sig.clear();
        }
    }

    /// `Accumulator.StopReason`: the stop reason reported by the last
    /// `message_delta` (empty until one arrives).
    pub fn stop_reason(&self) -> &str {
        &self.stop_reason
    }

    /// `Accumulator.Usage`: cumulative usage folded from `message_start` (added) and
    /// `message_delta` (echo-merged).
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// `Accumulator.Message`: finalize and return the assembled assistant message.
    /// Partial tool inputs are validated; malformed JSON falls back to `{}`. Mirrors
    /// Go's pointer receiver by flushing any buffered text/thinking in place.
    pub fn message(&mut self) -> Message {
        self.flush_text();
        self.flush_thinking();
        let mut ti = 0usize;
        let mut out = Vec::with_capacity(self.blocks.len());
        for block in &self.blocks {
            let mut block = block.clone();
            if block.block_type == BLOCK_TOOL_USE {
                let raw = self.tool_inputs[ti].trim();
                block.input =
                    Some(serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({})));
                block.thought_signature = self.tool_sigs[ti].clone();
                ti += 1;
            }
            out.push(block);
        }
        Message {
            role: ROLE_ASSISTANT.to_string(),
            content: out,
        }
    }
}

#[cfg(test)]
mod accumulate_tests {
    use super::*;

    fn acc(events: Vec<StreamEvent>) -> (Message, String, Usage) {
        let mut a = Accumulator::new();
        for e in events {
            a.add(e);
        }
        let stop = a.stop_reason().to_string();
        let usage = a.usage();
        (a.message(), stop, usage)
    }

    fn ev(ty: &str, text: &str) -> StreamEvent {
        StreamEvent {
            ty: ty.to_string(),
            text: text.to_string(),
            ..Default::default()
        }
    }

    // Expected JSON strings recorded from the real Go `llm.Accumulator`
    // (NewAccumulator + Add loop + Message), via a standalone oracle module.
    #[test]
    fn text_only_matches_go() {
        let (msg, stop, usage) = acc(vec![
            StreamEvent {
                ty: SE_MESSAGE_START.into(),
                usage: Usage {
                    input_tokens: 10,
                    ..Default::default()
                },
                ..Default::default()
            },
            ev(SE_TEXT_DELTA, "Hello, "),
            ev(SE_TEXT_DELTA, "world"),
            StreamEvent {
                ty: SE_MESSAGE_DELTA.into(),
                stop_reason: "end_turn".into(),
                usage: Usage {
                    output_tokens: 5,
                    ..Default::default()
                },
                ..Default::default()
            },
            ev(SE_MESSAGE_STOP, ""),
        ]);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":[{"type":"text","text":"Hello, world"}]}"#
        );
        assert_eq!(stop, "end_turn");
        assert_eq!(
            usage,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }
        );
    }

    #[test]
    fn thinking_then_text_matches_go() {
        let (msg, ..) = acc(vec![
            ev(SE_THINKING_DELTA, "reasoning..."),
            ev(SE_THINKING_SIGNATURE, "SIG1"),
            ev(SE_TEXT_DELTA, "answer"),
            ev(SE_MESSAGE_STOP, ""),
        ]);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":[{"type":"thinking","thinking":"reasoning...","signature":"SIG1"},{"type":"text","text":"answer"}]}"#
        );
    }

    #[test]
    fn signature_only_thinking_matches_go() {
        let (msg, ..) = acc(vec![
            ev(SE_THINKING_SIGNATURE, "SIG2"),
            ev(SE_MESSAGE_STOP, ""),
        ]);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"SIG2"}]}"#
        );
    }

    #[test]
    fn two_tools_indexed_matches_go() {
        let (msg, ..) = acc(vec![
            StreamEvent {
                ty: SE_TOOL_USE_START.into(),
                tool_id: "t1".into(),
                tool_name: "read".into(),
                tool_index: 0,
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_INPUT_JSON.into(),
                text: r#"{"url":"x"}"#.into(),
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_USE_SIGNATURE.into(),
                text: "SIGT1".into(),
                tool_index: 0,
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_USE_START.into(),
                tool_id: "t2".into(),
                tool_name: "grep".into(),
                tool_index: 1,
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_INPUT_JSON.into(),
                text: r#"{"pattern":"y"}"#.into(),
                ..Default::default()
            },
            ev(SE_MESSAGE_STOP, ""),
        ]);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"read","input":{"url":"x"},"thought_signature":"SIGT1"},{"type":"tool_use","id":"t2","name":"grep","input":{"pattern":"y"}}]}"#
        );
    }

    #[test]
    fn tool_bad_json_falls_back_to_empty_object() {
        let (msg, ..) = acc(vec![
            StreamEvent {
                ty: SE_TOOL_USE_START.into(),
                tool_id: "t1".into(),
                tool_name: "run".into(),
                tool_index: 0,
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_INPUT_JSON.into(),
                text: "{not valid".into(),
                ..Default::default()
            },
            ev(SE_MESSAGE_STOP, ""),
        ]);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"run","input":{}}]}"#
        );
    }

    #[test]
    fn redacted_thinking_matches_go() {
        let (msg, ..) = acc(vec![
            ev(SE_TEXT_DELTA, "before"),
            ev(SE_REDACTED_THINKING, "ENCRYPTED"),
            ev(SE_TEXT_DELTA, "after"),
            ev(SE_MESSAGE_STOP, ""),
        ]);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":[{"type":"text","text":"before"},{"type":"redacted_thinking","data":"ENCRYPTED"},{"type":"text","text":"after"}]}"#
        );
    }

    #[test]
    fn signature_index_remaps_after_second_call_starts() {
        // A late signature naming index 0 must land on the first tool_use, even
        // after call 1 has begun (the toolPos map).
        let (msg, ..) = acc(vec![
            StreamEvent {
                ty: SE_TOOL_USE_START.into(),
                tool_id: "a".into(),
                tool_name: "first".into(),
                tool_index: 0,
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_USE_START.into(),
                tool_id: "b".into(),
                tool_name: "second".into(),
                tool_index: 1,
                ..Default::default()
            },
            StreamEvent {
                ty: SE_TOOL_USE_SIGNATURE.into(),
                text: "SIG-A".into(),
                tool_index: 0,
                ..Default::default()
            },
            ev(SE_MESSAGE_STOP, ""),
        ]);
        let blocks = &msg.content;
        assert_eq!(blocks[0].thought_signature, "SIG-A");
        assert_eq!(blocks[1].thought_signature, "");
    }
}

#[cfg(test)]
mod context_value_tests {
    use super::*;

    #[derive(Debug)]
    struct Marker(u32);

    #[test]
    fn with_value_round_trips_and_preserves_cancellation() {
        let base = Context::canceled();
        let child = base.with_value(Arc::new(Marker(7)));
        assert!(child.is_cancelled(), "child must inherit cancellation");
        assert_eq!(child.value::<Marker>().unwrap().0, 7);
        assert!(base.value::<Marker>().is_none(), "parent must be unchanged");
    }

    #[test]
    fn background_has_no_values() {
        assert!(Context::background().value::<Marker>().is_none());
        assert!(!Context::background().is_cancelled());
    }
}
