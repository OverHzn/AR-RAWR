//! Rust port of the subset of the external `norma/harness` package that ARTEX
//! consumes.
//!
//! ARTEX never owns the agent loop; it drives completions through
//! [`QueryDeps`] — the model-call surface a harness query uses — so that the
//! sidequestion capture layer can wrap the model calls *without* a global
//! provider hook. Compaction and other auxiliary completions then cannot
//! silently replace the main conversation checkpoint. The full `Query` /
//! `QueryInput` machinery lands with the agent/server layer; only the call
//! surface sidequestion needs is ported here.
//!
//! In Go, `harness.QueryDeps` is a pair of function fields (`CallModel`,
//! `CallModelSync`) that default to a `Provider`'s `Stream` / `Complete`. The
//! Rust equivalent holds boxed callables so a caller can supply either the
//! provider-backed defaults or a wrapped variant.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream::BoxStream;

use crate::llm::{CompletionRequest, Context, Error, Message, Provider, StreamEvent, Usage};

/// `harness.QueryDeps.CallModel`: a streaming completion call. The Rust analogue
/// of Go's `func(ctx, req) iter.Seq2[llm.StreamEvent, error]`.
pub type CallModelFn = Arc<
    dyn Fn(Context, CompletionRequest) -> BoxStream<'static, Result<StreamEvent, Error>>
        + Send
        + Sync,
>;

/// The boxed future a [`CallModelSyncFn`] returns: the fully-assembled message, stop
/// reason, cumulative usage (observable even on error), and an optional error.
pub type CallModelSyncFuture =
    Pin<Box<dyn Future<Output = (Message, String, Usage, Option<Error>)> + Send>>;

/// `harness.QueryDeps.CallModelSync`: the non-streaming counterpart. Returns the
/// fully-assembled message, the stop reason, the cumulative usage (observable even
/// on error, matching Go's `Provider.Complete`), and an optional error.
pub type CallModelSyncFn =
    Arc<dyn Fn(Context, CompletionRequest) -> CallModelSyncFuture + Send + Sync>;

/// `harness.QueryDeps`: the model-call surface a harness query drives.
///
/// Either field may be `None`; [`fill_from`](QueryDeps::fill_from) populates a
/// missing field from a [`Provider`] (Go: `if d.CallModel == nil { … = in.Provider.Stream }`).
#[derive(Clone, Default)]
pub struct QueryDeps {
    pub call_model: Option<CallModelFn>,
    pub call_model_sync: Option<CallModelSyncFn>,
}

impl QueryDeps {
    /// Fill a missing `call_model` / `call_model_sync` from `provider`, exactly as
    /// the harness normalization does. Already-set fields are left untouched.
    pub fn fill_from(&mut self, provider: &Arc<dyn Provider>) {
        if self.call_model.is_none() {
            let p = provider.clone();
            self.call_model = Some(Arc::new(move |ctx, req| p.stream(ctx, req)));
        }
        if self.call_model_sync.is_none() {
            let p = provider.clone();
            self.call_model_sync = Some(Arc::new(move |ctx, req| {
                let p = p.clone();
                Box::pin(async move { p.complete_full(ctx, req).await })
            }));
        }
    }
}

impl std::fmt::Debug for QueryDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryDeps")
            .field("call_model", &self.call_model.is_some())
            .field("call_model_sync", &self.call_model_sync.is_some())
            .finish()
    }
}
