//! `sidequestion/service.go`: a single side-question completion. The service has no
//! harness, tool executor, transcript writer or model-failover chain — `answer` is
//! one completion; the context builder (`Respond`) adds bounded preparation and at
//! most one overflow recovery around it.

use std::sync::Arc;

use crate::llm::{
    CompletionRequest, Context, Error, Provider, SE_MESSAGE_DELTA, SE_MESSAGE_START,
    SE_MESSAGE_STOP, SE_TEXT_DELTA, SE_TOOL_USE_START,
};

use super::Answer;

/// `SideQuestionService`: a thin wrapper over one [`Provider`]. Cheap to clone
/// (the provider is shared behind an `Arc`), so the context builder can keep its own
/// copy while borrowing other state mutably.
#[derive(Clone)]
pub struct SideQuestionService {
    pub provider: Arc<dyn Provider>,
}

/// Text returned when the model tried to use a tool: side questions cannot execute
/// operations.
const TOOL_USE_REFUSAL: &str =
    "Side questions cannot execute tool operations; please send operation requests in the main session.";

impl SideQuestionService {
    /// Wrap a provider.
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        SideQuestionService { provider }
    }

    /// `SideQuestionService.Answer`: one completion, streamed or atomic.
    ///
    /// Returns the accumulated answer and any error. `update` (Go `func(Answer)`) is
    /// invoked after each streaming event with the partial answer. When the model
    /// produces no text: a tool-use response becomes the [`TOOL_USE_REFUSAL`] message;
    /// an empty one is the error `"model did not return an answer"`. A stream that
    /// ends without a `message_stop` is `"model response was interrupted, please ask
    /// again"`. Cancellation at any point surfaces `ctx.Err()`.
    pub async fn answer<F>(
        &self,
        ctx: Context,
        req: CompletionRequest,
        streaming: bool,
        mut update: Option<F>,
    ) -> (Answer, Option<Error>)
    where
        F: FnMut(Answer),
    {
        let mut out = Answer::default();
        let mut err: Option<Error> = None;

        if streaming {
            let mut complete = false;
            let mut stream = self.provider.stream(ctx.clone(), req);
            use futures::StreamExt;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(ev) => match ev.ty.as_str() {
                        SE_TEXT_DELTA => out.text.push_str(&ev.text),
                        SE_TOOL_USE_START => out.tool_use = true,
                        SE_MESSAGE_START => out.usage.add(ev.usage),
                        // message_delta may echo message_start's input side:
                        // MergeEcho, not Add.
                        SE_MESSAGE_DELTA => out.usage.merge_echo(ev.usage),
                        SE_MESSAGE_STOP => complete = true,
                        _ => {}
                    },
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                }
                if let Some(u) = update.as_mut() {
                    u(out.clone());
                }
                if ctx.is_cancelled() {
                    err = ctx.err();
                    break;
                }
            }
            if err.is_none() && !complete {
                err = Some(Error::msg(
                    "model response was interrupted, please ask again",
                ));
            }
        } else {
            // Go `msg, _, out.Usage, err = s.Provider.Complete(ctx, req)`: the usage
            // is retained even on error (complete_full), so a failed call's tokens
            // still count toward the overflow-recovery accounting.
            let (msg, _stop, usage, eopt) = self.provider.complete_full(ctx.clone(), req).await;
            out.usage = usage;
            out.text = msg.text();
            out.tool_use = !msg.tool_uses().is_empty();
            if let Some(e) = eopt {
                err = Some(e);
            }
        }

        // Any cancellation wins over whatever the provider returned, exactly as Go's
        // trailing `if ctx.Err() != nil { err = ctx.Err() }` overrides a stale error.
        if ctx.is_cancelled() {
            err = ctx.err();
        }

        if err.is_none() && out.text.trim().is_empty() {
            if out.tool_use {
                out.text = TOOL_USE_REFUSAL.to_string();
            } else {
                err = Some(Error::msg("model did not return an answer"));
            }
        }

        (out, err)
    }
}
