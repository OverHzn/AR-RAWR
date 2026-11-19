//! Rust port of `sidequestion/capture.go`: capture immutable main-agent
//! checkpoints by wrapping the model-call surface ([`crate::harness::QueryDeps`])
//! rather than a global provider hook.
//!
//! The package never owns an agent session or a tool executor. [`attach`] threads a
//! per-attach [`Capture`] sink through the provider pool via the [`Context`] value
//! bag: calls routed through the returned [`QueryDeps`] carry an *attempt* marker so
//! [`bind`] (placed immediately around a concrete provider, inside any routing pool)
//! can checkpoint the request and, on a clean completion, the response. Auxiliary /
//! compaction calls (which lack the attempt marker) never replace the main
//! conversation checkpoint. [`finish`] folds the tool results that landed after the
//! final model request back onto the last checkpoint.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::harness::{CallModelSyncFuture, QueryDeps};
use crate::llm::{
    messages_for_api, Accumulator, CompletionRequest, Context, Error, Message, Provider,
    StreamEvent, Usage, SE_MESSAGE_STOP,
};

use super::{clone_request, GoTime, Model, Parent, Snapshot};

/// `sidequestion.Publisher`: the sink every captured [`Snapshot`] is published to.
/// Observability only — a publisher must never break the main run.
#[derive(Clone)]
pub struct Publisher(Arc<dyn Fn(Snapshot) + Send + Sync>);

impl Publisher {
    /// Wrap a publish callback.
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(Snapshot) + Send + Sync + 'static,
    {
        Publisher(Arc::new(f))
    }

    fn call(&self, s: Snapshot) {
        let f: &(dyn Fn(Snapshot) + Send + Sync) = self.0.as_ref();
        f(s)
    }
}

impl std::fmt::Debug for Publisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Publisher(..)")
    }
}

// The Context value bag is keyed by the value's type (Go uses private `struct{}`
// keys). `publisherKey`, `captureKey` and `attemptKey` are three distinct slots: the
// capture sink stored under `captureKey` is what `finish` reads, while the one under
// `attemptKey` is set only on the per-call context and is what `bind` reads — so an
// auxiliary completion that bypasses the wrapped deps (no attempt marker) is never
// captured.

/// `publisherKey` value.
///
/// Stored in the context by [`with_publisher`]; read by [`attach`].
pub type PublisherKey = Publisher;

/// `captureKey` value: the capture sink `finish` reads. Set on the context returned
/// by [`attach`], never on the per-call context handed to a provider.
#[derive(Clone)]
pub struct CaptureKey(pub Arc<Capture>);

/// `attemptKey` value: the capture sink `bind` reads. Set only on the per-call
/// context by the [`QueryDeps`] returned from [`attach`].
#[derive(Clone)]
pub struct AttemptKey(pub Arc<Capture>);

/// `WithPublisher`: a child context carrying `publish` under the publisher key.
pub fn with_publisher(ctx: Context, publish: Publisher) -> Context {
    ctx.with_value(Arc::new(publish))
}

// === Capture ===

#[derive(Default)]
struct CaptureState {
    version: i64,
    last: Option<Snapshot>,
}

/// `sidequestion.Capture`: the per-attach mutable checkpoint state. `parent`,
/// `run_id` and `publish` are fixed at construction; `version` and `last` are mutated
/// under the lock.
pub struct Capture {
    parent: Parent,
    run_id: i64,
    publish: Publisher,
    state: Mutex<CaptureState>,
}

impl Capture {
    fn new(parent: Parent, run_id: i64, publish: Publisher) -> Self {
        Capture {
            parent,
            run_id,
            publish,
            state: Mutex::new(CaptureState::default()),
        }
    }

    /// `Capture.save`: deep-copy the request, stamp a new version + capture time,
    /// record it as `last`, then publish. Observability must not break the main run,
    /// so the clone is infallible (Rust's owned `CompletionRequest` clones deeply).
    fn save(&self, req: &CompletionRequest, model: &Model) {
        // CloneRequest: Go round-trips through JSON to break the slice/map aliasing;
        // Rust's `clone` is already a deep copy.
        let copy = match clone_request(req) {
            Ok(c) => c,
            Err(_) => return,
        };
        let s = {
            let mut st = self.state.lock().unwrap();
            st.version += 1;
            let s = Snapshot {
                parent: self.parent,
                run_id: self.run_id,
                version: st.version,
                captured_at: GoTime::now(),
                model: model.clone(),
                request: copy,
            };
            st.last = Some(s.clone());
            s
        };
        self.publish.call(s);
    }
}

fn now_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// `sidequestion.Attach`: thread a capture sink through the call surface so
/// compaction and other auxiliary completions cannot replace the main conversation
/// checkpoint. Returns the child context (carrying the capture sink for [`finish`])
/// and the wrapped [`QueryDeps`] (whose calls carry the per-call attempt marker).
///
/// If the context carries no [`Publisher`], it is returned unchanged together with
/// `deps`.
pub fn attach(
    ctx: Context,
    parent: Parent,
    mut deps: QueryDeps,
    provider: Arc<dyn Provider>,
) -> (Context, QueryDeps) {
    let publish = match ctx.value::<PublisherKey>() {
        Some(p) => (*p).clone(),
        None => return (ctx, deps),
    };
    let c = Arc::new(Capture::new(parent, now_unix_nanos(), publish));

    // Fill any missing call surface from the provider (Go: `if stream == nil { … }`).
    deps.fill_from(&provider);

    // Wrap both calls to inject the per-call attempt marker.
    let inner_stream = deps.call_model.take().expect("call_model filled");
    let c_stream = c.clone();
    deps.call_model = Some(Arc::new(move |ctx: Context, req: CompletionRequest| {
        let inner = inner_stream.clone();
        let ctx = ctx.with_value(Arc::new(AttemptKey(c_stream.clone())));
        (inner.as_ref())(ctx, req)
    }));

    let inner_sync = deps.call_model_sync.take().expect("call_model_sync filled");
    let c_sync = c.clone();
    deps.call_model_sync = Some(Arc::new(
        move |ctx: Context, req: CompletionRequest| -> CallModelSyncFuture {
            let inner = inner_sync.clone();
            let ctx = ctx.with_value(Arc::new(AttemptKey(c_sync.clone())));
            (inner.as_ref())(ctx, req)
        },
    ));

    let ctx = ctx.with_value(Arc::new(CaptureKey(c)));
    (ctx, deps)
}

/// `sidequestion.Finish`: add the tool results that landed after the final model
/// request. Norma's first API message is its host reminder, absent from the terminal
/// messages, so it is preserved as a prefix.
pub fn finish(ctx: Context, messages: &[Message]) {
    let c = match ctx.value::<CaptureKey>() {
        Some(h) => h.0.clone(),
        None => return,
    };
    let last = { c.state.lock().unwrap().last.clone() };
    let s = match last {
        Some(s) => s,
        None => return,
    };
    let mut req = s.request;
    let mut prefix: Vec<Message> = Vec::new();
    if let Some(first) = req.messages.first() {
        if first.text().starts_with("<system-reminder>") {
            prefix.push(first.clone());
        }
    }
    req.messages = prefix;
    req.messages.extend(messages_for_api(messages));
    c.save(&req, &s.model);
}

// === boundProvider / Bind ===

struct BoundProvider {
    inner: Arc<dyn Provider>,
    model: Model,
}

/// `sidequestion.Bind`: wrap a concrete provider so it checkpoints through the
/// per-call [`AttemptKey`] capture. Belongs immediately around the provider, inside
/// any routing pool, so every member reports its own model identity.
pub fn bind(inner: Arc<dyn Provider>, model: Model) -> Arc<dyn Provider> {
    Arc::new(BoundProvider { inner, model })
}

fn attempt_capture(ctx: &Context) -> Option<Arc<Capture>> {
    ctx.value::<AttemptKey>().map(|h| h.0.clone())
}

/// `append(append([]llm.Message{}, req.Messages...), msg)` then `MessagesForAPI`.
fn append_message(req: &CompletionRequest, msg: Message) -> CompletionRequest {
    let mut out = req.clone();
    let mut msgs = req.messages.clone();
    msgs.push(msg);
    out.messages = messages_for_api(&msgs);
    out
}

#[async_trait::async_trait]
impl Provider for BoundProvider {
    async fn complete(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error> {
        let (msg, stop, usage, eopt) = self.complete_inner(ctx, req).await;
        match eopt {
            Some(e) => Err(e),
            None => Ok((msg, stop, usage)),
        }
    }

    fn stream(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> futures::stream::BoxStream<'static, Result<StreamEvent, Error>> {
        let cap = attempt_capture(&ctx);
        match cap {
            None => self.inner.stream(ctx, req),
            Some(c) => {
                let inner = self.inner.clone();
                let model = self.model.clone();
                Box::pin(async_stream::stream! {
                    c.save(&req, &model);
                    let mut acc = Accumulator::new();
                    let mut failed = false;
                    let mut complete = false;
                    let mut s = inner.stream(ctx.clone(), req.clone());
                    use futures::StreamExt;
                    while let Some(item) = s.next().await {
                        match &item {
                            Ok(ev) => {
                                acc.add(ev.clone());
                                if ev.ty == SE_MESSAGE_STOP {
                                    complete = true;
                                }
                            }
                            Err(_) => failed = true,
                        }
                        yield item;
                    }
                    // Only a clean, non-cancelled completion is folded back onto the
                    // checkpoint; a consumer that stops early (drops the stream) never
                    // reaches here, matching Go's `yield` returning false.
                    if !failed && complete && ctx.err().is_none() {
                        let msg = acc.message();
                        let saved = append_message(&req, msg);
                        c.save(&saved, &model);
                    }
                })
            }
        }
    }

    async fn complete_full(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> (Message, String, Usage, Option<Error>) {
        self.complete_inner(ctx, req).await
    }
}

impl BoundProvider {
    /// The shared body of `complete` / `complete_full`: checkpoint the request, run
    /// the inner completion, and on a clean non-cancelled result checkpoint the
    /// response. The inner's usage is returned even on error (Go `Provider.Complete`).
    async fn complete_inner(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> (Message, String, Usage, Option<Error>) {
        let cap = attempt_capture(&ctx);
        if let Some(c) = &cap {
            c.save(&req, &self.model);
        }
        let (msg, stop, usage, eopt) = self.inner.complete_full(ctx.clone(), req.clone()).await;
        if eopt.is_none() && ctx.err().is_none() {
            if let Some(c) = &cap {
                let saved = append_message(&req, msg.clone());
                c.save(&saved, &self.model);
            }
        }
        (msg, stop, usage, eopt)
    }
}
