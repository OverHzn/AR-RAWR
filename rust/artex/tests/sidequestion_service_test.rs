//! Rust port of `sidequestion/sidequestion_test.go::TestServiceNoToolsAndUsageOnFailure`
//! and `sidequestion/usage_echo_test.go::TestAnswerStreamUsageEchoCountedOnce` — the
//! spec for `SideQuestionService::Answer`: no tool execution (a tool-use reply becomes
//! a refusal), usage on failure is preserved, the main context is never mutated, and a
//! streamed `message_delta` echo of the input side is counted once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use artex::llm::{
    CompletionRequest, ContentBlock, Context, Error, Message, Provider, StreamEvent, ToolSchema,
    Usage, BLOCK_THINKING, BLOCK_TOOL_USE, ROLE_ASSISTANT, ROLE_USER, SE_MESSAGE_DELTA,
    SE_MESSAGE_START, SE_MESSAGE_STOP, SE_TEXT_DELTA, SE_TOOL_USE_START,
};
use artex::sidequestion::{Answer, SideQuestionService};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::{json, Value};

// === Scriptable provider ===

type StreamFn = Box<
    dyn Fn(Context, CompletionRequest) -> BoxStream<'static, Result<StreamEvent, Error>>
        + Send
        + Sync,
>;
type CompleteFn = Box<
    dyn Fn(
            Context,
            CompletionRequest,
        ) -> BoxFuture<'static, Result<(Message, String, Usage), Error>>
        + Send
        + Sync,
>;

struct FakeProvider {
    stream: StreamFn,
    complete: CompleteFn,
}

#[async_trait]
impl Provider for FakeProvider {
    async fn complete(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error> {
        (self.complete)(ctx, req).await
    }
    fn stream(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> BoxStream<'static, Result<StreamEvent, Error>> {
        (self.stream)(ctx, req)
    }
}

/// Shared call counter + the last request the provider observed (for the
/// "service mutated main context" assertion).
#[derive(Clone, Default)]
struct Shared {
    calls: Arc<AtomicUsize>,
    captured: Arc<Mutex<Option<CompletionRequest>>>,
}
impl Shared {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn take_captured(&self) -> Option<CompletionRequest> {
        self.captured.lock().unwrap().take()
    }
}

fn assistant_tool_use(id: &str, name: &str, input: Value) -> Message {
    Message {
        role: ROLE_ASSISTANT.to_string(),
        content: vec![ContentBlock {
            block_type: BLOCK_TOOL_USE.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            input: Some(input),
            ..Default::default()
        }],
    }
}

fn ev(ty: &str, text: &str) -> StreamEvent {
    StreamEvent {
        ty: ty.to_string(),
        text: text.to_string(),
        ..Default::default()
    }
}

fn start(usage: Usage) -> StreamEvent {
    StreamEvent {
        ty: SE_MESSAGE_START.to_string(),
        usage,
        ..Default::default()
    }
}

fn delta(stop: &str, usage: Usage) -> StreamEvent {
    StreamEvent {
        ty: SE_MESSAGE_DELTA.to_string(),
        stop_reason: stop.to_string(),
        usage,
        ..Default::default()
    }
}

/// `fixture()` from the Go test: a request with a system prompt, options, and a
/// thinking+tool_use/tool_result message pair.
fn fixture() -> CompletionRequest {
    CompletionRequest {
        system: vec!["system".to_string()],
        temperature: Some(0.3),
        max_tokens: 128,
        stop: vec!["END".to_string()],
        messages: vec![
            artex::llm::user_text("<system-reminder>date</system-reminder>"),
            artex::llm::user_text("read asset"),
            Message {
                role: ROLE_ASSISTANT.to_string(),
                content: vec![
                    ContentBlock {
                        block_type: BLOCK_THINKING.to_string(),
                        thinking: "reason".to_string(),
                        signature: "signed".to_string(),
                        ..Default::default()
                    },
                    ContentBlock {
                        block_type: BLOCK_TOOL_USE.to_string(),
                        id: "tool-1".to_string(),
                        name: "read".to_string(),
                        input: Some(json!({ "url": "fixture" })),
                        ..Default::default()
                    },
                ],
            },
            Message {
                role: ROLE_USER.to_string(),
                content: vec![artex::llm::tool_result_text(
                    "tool-1",
                    "verified asset",
                    false,
                )],
            },
        ],
        tools: vec![ToolSchema {
            name: "read".to_string(),
            description: String::new(),
            input_schema: json!({ "properties": { "url": { "type": "string" } } }),
        }],
        ..Default::default()
    }
}

// === TestServiceNoToolsAndUsageOnFailure ===

#[tokio::test]
async fn service_no_tools_and_usage_on_failure() {
    for streaming in [false, true] {
        let shared = Shared::default();
        let sh_c = shared.clone();
        let sh_s = shared.clone();

        let complete: CompleteFn = Box::new(move |_ctx: Context, req: CompletionRequest| {
            let sh = sh_c.clone();
            Box::pin(async move {
                sh.calls.fetch_add(1, Ordering::SeqCst);
                *sh.captured.lock().unwrap() = Some(req);
                Ok((
                    assistant_tool_use("new", "run_shell", json!({ "command": "touch forbidden" })),
                    "tool_use".to_string(),
                    Usage {
                        input_tokens: 11,
                        ..Default::default()
                    },
                ))
            })
        });
        let stream: StreamFn = Box::new(move |_ctx: Context, req: CompletionRequest| {
            let sh = sh_s.clone();
            sh.calls.fetch_add(1, Ordering::SeqCst);
            *sh.captured.lock().unwrap() = Some(req);
            Box::pin(async_stream::stream! {
                yield Ok(start(Usage { input_tokens: 11, ..Default::default() }));
                yield Ok(ev(SE_TOOL_USE_START, ""));
                yield Ok(ev(SE_MESSAGE_STOP, ""));
            })
        });

        let svc = SideQuestionService::new(Arc::new(FakeProvider { stream, complete }));
        let before = fixture();
        let (out, err) = svc
            .answer(
                Context::background(),
                before.clone(),
                streaming,
                None::<fn(Answer)>,
            )
            .await;

        let label = if streaming { "stream" } else { "atomic" };
        assert!(err.is_none(), "{label}: unexpected err {err:?}");
        assert_eq!(shared.calls(), 1, "{label}: provider called once");
        assert!(out.tool_use, "{label}: tool_use flagged");
        assert!(
            out.text.contains("cannot execute tool"),
            "{label}: refusal text, got {:?}",
            out.text
        );
        assert_eq!(out.usage.input_tokens, 11, "{label}: input usage");
        // The service must pass the main context through unmodified.
        assert_eq!(
            shared.take_captured(),
            Some(before),
            "{label}: service mutated main context"
        );
    }

    // A stream that errors mid-flight keeps the partial text and the usage already
    // reported by message_start.
    let stream: StreamFn = Box::new(|_ctx: Context, _req: CompletionRequest| {
        Box::pin(async_stream::stream! {
            yield Ok(start(Usage { input_tokens: 12, ..Default::default() }));
            yield Ok(ev(SE_TEXT_DELTA, "partial"));
            yield Err(Error::msg("broken"));
        })
    });
    let complete: CompleteFn =
        Box::new(|_ctx, _req| Box::pin(async { unreachable!("atomic unused") }));
    let svc = SideQuestionService::new(Arc::new(FakeProvider { stream, complete }));
    let (out, err) = svc
        .answer(Context::background(), fixture(), true, None::<fn(Answer)>)
        .await;
    assert!(err.is_some(), "stream error surfaced");
    assert_eq!(out.text, "partial", "partial text kept");
    assert_eq!(out.usage.input_tokens, 12, "failure usage kept");
}

// === TestAnswerStreamUsageEchoCountedOnce ===

#[tokio::test]
async fn answer_stream_usage_echo_counted_once() {
    let cases: [(&str, Usage, Usage, Usage); 3] = [
        (
            "full_echo",
            Usage {
                input_tokens: 23,
                cache_read_tokens: 5,
                cache_write_tokens: 3,
                ..Default::default()
            },
            Usage {
                input_tokens: 23,
                cache_read_tokens: 5,
                cache_write_tokens: 3,
                output_tokens: 5,
            },
            Usage {
                input_tokens: 23,
                output_tokens: 5,
                cache_read_tokens: 5,
                cache_write_tokens: 3,
            },
        ),
        (
            "partial_echo",
            Usage {
                input_tokens: 740,
                cache_read_tokens: 200,
                cache_write_tokens: 40,
                ..Default::default()
            },
            Usage {
                input_tokens: 500,
                output_tokens: 77,
                ..Default::default()
            },
            Usage {
                input_tokens: 740,
                output_tokens: 77,
                cache_read_tokens: 200,
                cache_write_tokens: 40,
            },
        ),
        (
            "no_echo",
            Usage {
                input_tokens: 23,
                cache_read_tokens: 5,
                cache_write_tokens: 3,
                ..Default::default()
            },
            Usage {
                output_tokens: 5,
                ..Default::default()
            },
            Usage {
                input_tokens: 23,
                output_tokens: 5,
                cache_read_tokens: 5,
                cache_write_tokens: 3,
            },
        ),
    ];

    for (name, start_usage, delta_usage, want) in cases {
        let stream: StreamFn = Box::new(move |_ctx: Context, _req: CompletionRequest| {
            Box::pin(async_stream::stream! {
                yield Ok(start(start_usage));
                yield Ok(ev(SE_TEXT_DELTA, "answer"));
                yield Ok(delta("end_turn", delta_usage));
                yield Ok(ev(SE_MESSAGE_STOP, ""));
            })
        });
        let complete: CompleteFn =
            Box::new(|_ctx, _req| Box::pin(async { unreachable!("atomic unused") }));
        let svc = SideQuestionService::new(Arc::new(FakeProvider { stream, complete }));
        let (out, err) = svc
            .answer(
                Context::background(),
                CompletionRequest::default(),
                true,
                None::<fn(Answer)>,
            )
            .await;
        assert!(err.is_none(), "{name}: {err:?}");
        assert_eq!(out.text, "answer", "{name}: text");
        assert_eq!(out.usage, want, "{name}: usage echo counted once");
    }
}
