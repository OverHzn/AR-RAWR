//! Rust port of the capture specs in `sidequestion/sidequestion_test.go`:
//! `TestCheckpointDeepCopyAndBoundaries`,
//! `TestSnapshotExcludesPartialStreamAndSelectsPoolMember`, and
//! `TestMainSideConcurrencyAndIndependentCancellation`.
//!
//! They pin the capture layer's contract: published checkpoints are deep copies
//! (no aliasing of the main conversation's structured input); the final assistant
//! turn is folded back only on a clean, non-cancelled completion and never mid-stream;
//! the routing pool's failover is captured under the selected member's model identity;
//! auxiliary completions (no attempt marker) never replace the checkpoint; `finish`
//! re-adds post-request tool results while preserving the host reminder prefix; and a
//! side question runs concurrently with the main loop under independent cancellation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use artex::harness::QueryDeps;
use artex::llm::{
    text_block, tool_result_text, user_text, CompletionRequest, ContentBlock, Context, Error,
    Message, Provider, StreamEvent, ToolSchema, Usage, BLOCK_THINKING, BLOCK_TOOL_USE,
    ROLE_ASSISTANT, ROLE_USER, SE_MESSAGE_START, SE_MESSAGE_STOP, SE_TEXT_DELTA,
};
use artex::llmpool::{Member, Pool};
use artex::sidequestion::{
    attach, bind, finish, with_publisher, Answer, Model, Parent, Publisher, SideQuestionService,
    Snapshot,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;

// === Scriptable provider (Go `fakeProvider`): one provider, both paths ===

type StreamFn = Box<
    dyn Fn(Context, CompletionRequest) -> BoxStream<'static, Result<StreamEvent, Error>>
        + Send
        + Sync,
>;
type CompleteFn = Box<
    dyn Fn(
            Context,
            CompletionRequest,
        ) -> futures::future::BoxFuture<'static, Result<(Message, String, Usage), Error>>
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

// === Go test helpers ===

fn assistant(text: &str) -> Message {
    Message {
        role: ROLE_ASSISTANT.to_string(),
        content: vec![text_block(text)],
    }
}

/// `fixture()`: a system prompt, options, and a thinking+tool_use / tool_result pair.
fn fixture() -> CompletionRequest {
    CompletionRequest {
        system: vec!["system".to_string()],
        temperature: Some(0.3),
        max_tokens: 128,
        stop: vec!["END".to_string()],
        messages: vec![
            user_text("<system-reminder>date</system-reminder>"),
            user_text("read asset"),
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
                content: vec![tool_result_text("tool-1", "verified asset", false)],
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

fn snap_json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

// === TestCheckpointDeepCopyAndBoundaries ===

#[tokio::test]
async fn checkpoint_deep_copy_and_boundaries() {
    let snapshots = Arc::new(Mutex::new(Vec::<Snapshot>::new()));
    let snaps_c = snapshots.clone();
    let p = bind(
        Arc::new(FakeProvider {
            complete: Box::new(|_c, _r| {
                Box::pin(async {
                    Ok((
                        assistant("finished"),
                        "end_turn".to_string(),
                        Usage::default(),
                    ))
                })
            }),
            stream: Box::new(|_c, _r| {
                Box::pin(async_stream::stream! {
                    yield Err(Error::msg("stream unused"));
                })
            }),
        }),
        Model {
            model: "actual".to_string(),
            ..Default::default()
        },
    );
    let ctx = with_publisher(
        Context::background(),
        Publisher::new(move |s| snaps_c.lock().unwrap().push(s)),
    );
    let (ctx, deps) = attach(
        ctx,
        Parent {
            conversation_id: 1,
            ..Default::default()
        },
        QueryDeps::default(),
        p.clone(),
    );

    let call = deps.call_model_sync.clone().unwrap();
    let req = fixture();
    let (_m, _s, _u, e) = (call.as_ref())(ctx.clone(), req.clone()).await;
    assert!(e.is_none(), "{e:?}");

    {
        let snaps = snapshots.lock().unwrap();
        assert_eq!(snaps.len(), 2, "checkpoints: {}", snaps.len());
        assert_eq!(snaps[1].request.messages.last().unwrap().text(), "finished");
    }

    // Mutating the caller's request after the call must not change the published
    // checkpoints: `save` stores a deep copy, never an alias.
    let before = serde_json::to_string(&*snapshots.lock().unwrap()).unwrap();
    let mut req = req;
    req.system[0] = "mutated".to_string();
    req.temperature = Some(2.0);
    req.stop[0] = "bad".to_string();
    req.messages[2].content[1].input = Some(json!({ "url": "MUTATED" }));
    req.messages[3].content[0].content[0].text = "mutated".to_string();
    req.tools[0].input_schema["properties"]["url"]["type"] = json!("number");
    let after = serde_json::to_string(&*snapshots.lock().unwrap()).unwrap();
    assert_eq!(
        before, after,
        "published checkpoint aliases structured input"
    );

    // Auxiliary / compaction calls lack the model-attempt marker: no capture.
    let _ = p
        .complete(
            ctx.clone(),
            CompletionRequest {
                messages: vec![user_text("summary request")],
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        snapshots.lock().unwrap().len(),
        2,
        "auxiliary completion replaced snapshot"
    );

    // Finish folds the terminal messages (incl. the paired tool result) back onto the
    // last checkpoint, preserving the leading host reminder.
    let mut terminal = fixture().messages[1..].to_vec();
    terminal.push(assistant("terminal answer"));
    finish(ctx.clone(), &terminal);
    {
        let snaps = snapshots.lock().unwrap();
        let last = snaps.last().unwrap();
        assert_eq!(
            last.request.messages[0].text(),
            "<system-reminder>date</system-reminder>",
            "terminal/reminder duplication"
        );
        assert_eq!(last.request.messages.len(), 5, "terminal message count");
        assert!(
            snap_json(last).contains("verified asset"),
            "terminal lost paired tool result"
        );
    }
}

// === TestSnapshotExcludesPartialStreamAndSelectsPoolMember ===

#[tokio::test]
async fn snapshot_excludes_partial_stream_and_selects_pool_member() {
    let snapshots = Arc::new(Mutex::new(Vec::<Snapshot>::new()));

    let failed = bind(
        Arc::new(FakeProvider {
            complete: Box::new(|_c, _r| Box::pin(async { unreachable!("atomic unused") })),
            stream: Box::new(|_c, _r| {
                Box::pin(async_stream::stream! {
                    yield Err(Error::msg("HTTP 503 unavailable"));
                })
            }),
        }),
        Model {
            model: "failed".to_string(),
            ..Default::default()
        },
    );
    let good = bind(
        Arc::new(FakeProvider {
            complete: Box::new(|_c, _r| Box::pin(async { unreachable!("atomic unused") })),
            stream: Box::new(|_c, _r| {
                Box::pin(async_stream::stream! {
                    yield Ok(StreamEvent { ty: SE_TEXT_DELTA.to_string(), text: "complete".to_string(), ..Default::default() });
                    yield Ok(StreamEvent { ty: SE_MESSAGE_STOP.to_string(), ..Default::default() });
                })
            }),
        }),
        Model {
            model: "selected".to_string(),
            ..Default::default()
        },
    );

    let pool: Arc<dyn Provider> = Arc::new(
        Pool::new(
            vec![
                Member {
                    id: 1,
                    name: "failed".to_string(),
                    rank: 2,
                    prov: failed,
                    ..member_rest()
                },
                Member {
                    id: 2,
                    name: "selected".to_string(),
                    rank: 1,
                    prov: good,
                    ..member_rest()
                },
            ],
            None,
        )
        .unwrap(),
    );

    let snaps_c = snapshots.clone();
    let ctx = with_publisher(
        Context::background(),
        Publisher::new(move |s| snaps_c.lock().unwrap().push(s)),
    );
    let (ctx, deps) = attach(
        ctx,
        Parent {
            conversation_id: 2,
            ..Default::default()
        },
        QueryDeps::default(),
        pool,
    );
    let call_model = deps.call_model.clone().unwrap();

    let mut stream = (call_model.as_ref())(ctx.clone(), fixture());
    while let Some(item) = stream.next().await {
        let ev = item.expect("failover error surfaced to consumer");
        if ev.ty == SE_TEXT_DELTA {
            let last_json = snap_json(snapshots.lock().unwrap().last().unwrap());
            assert!(
                !last_json.contains("complete"),
                "partial reply published: {last_json}"
            );
        }
    }
    {
        let snaps = snapshots.lock().unwrap();
        let last = snaps.last().unwrap();
        assert_eq!(
            last.model.model, "selected",
            "wrong selected model/checkpoint"
        );
        assert_eq!(
            last.request.messages.last().unwrap().text(),
            "complete",
            "wrong selected checkpoint"
        );
    }

    let count = snapshots.lock().unwrap().len();
    // A consumer that stops after the first event must not trigger the post-stream
    // fold-back: exactly the two per-attempt request checkpoints are added.
    {
        let mut stream = (call_model.as_ref())(ctx.clone(), fixture());
        let _ = stream.next().await;
        // dropped here -> early stop
    }
    {
        let snaps = snapshots.lock().unwrap();
        assert_eq!(
            snaps.len(),
            count + 2,
            "consumer stop published partial reply"
        );
        assert_eq!(
            snaps.last().unwrap().request.messages.len(),
            fixture().messages.len(),
            "early-stop checkpoint should hold the unmodified request"
        );
    }
}

/// Default tail for a pool [`Member`] in these tests: no window cap, no priority.
fn member_rest() -> Member {
    Member {
        id: 0,
        name: String::new(),
        model: String::new(),
        format: String::new(),
        priority: 0,
        active: false,
        rank: 0,
        window_tokens: 0,
        prov: Arc::new(FakeProvider {
            complete: Box::new(|_c, _r| Box::pin(async { unreachable!("replaced") })),
            stream: Box::new(|_c, _r| {
                Box::pin(async_stream::stream! {
                    yield Err(Error::msg("replaced"));
                })
            }),
        }),
    }
}

// === TestMainSideConcurrencyAndIndependentCancellation ===

/// A provider that announces it started, emits an input-token `message_start`, then
/// blocks until its context is cancelled and yields the cancellation error.
struct BlockingProvider {
    started: mpsc::Sender<()>,
}

#[async_trait]
impl Provider for BlockingProvider {
    async fn complete(
        &self,
        _ctx: Context,
        _req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error> {
        unreachable!("blocking provider streams only")
    }
    fn stream(
        &self,
        ctx: Context,
        _req: CompletionRequest,
    ) -> BoxStream<'static, Result<StreamEvent, Error>> {
        let started = self.started.clone();
        Box::pin(async_stream::stream! {
            let _ = started.send(()).await;
            yield Ok(StreamEvent {
                ty: SE_MESSAGE_START.to_string(),
                usage: Usage { input_tokens: 7, ..Default::default() },
                ..Default::default()
            });
            ctx.cancelled().await;
            yield Err(ctx.err().unwrap_or(Error::Canceled));
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn main_side_concurrency_and_independent_cancellation() {
    for cancel_main in [false, true] {
        let (started_tx, mut started_rx) = mpsc::channel::<()>(2);
        let bound = bind(
            Arc::new(BlockingProvider {
                started: started_tx,
            }),
            Model {
                model: "blocking".to_string(),
                ..Default::default()
            },
        );

        let (main_ctx, stop_main) = Context::with_cancel();
        let (side_ctx, stop_side) = Context::with_cancel();

        let mu = Arc::new(Mutex::new(()));
        let mu_pub = mu.clone();
        let main_ctx = with_publisher(
            main_ctx,
            Publisher::new(move |_s| {
                let _g = mu_pub.lock().unwrap();
            }),
        );
        let (main_ctx, deps) = attach(
            main_ctx,
            Parent {
                conversation_id: 3,
                ..Default::default()
            },
            QueryDeps::default(),
            bound.clone(),
        );

        let call_model = deps.call_model.clone().unwrap();
        let main_task = tokio::spawn({
            let main_ctx = main_ctx.clone();
            async move {
                let mut s = (call_model.as_ref())(main_ctx, fixture());
                while s.next().await.is_some() {}
            }
        });
        let side_task = tokio::spawn({
            let svc = SideQuestionService::new(bound.clone());
            async move {
                let (a, _e) = svc
                    .answer(side_ctx, fixture(), true, None::<fn(Answer)>)
                    .await;
                a
            }
        });

        // Both main and side must reach the provider before we cancel anything.
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("main and side did not execute concurrently")
                .expect("started channel closed early");
        }

        if cancel_main {
            stop_main.cancel();
            tokio::time::timeout(Duration::from_secs(1), main_task)
                .await
                .expect("main cancellation stuck")
                .unwrap();
            assert!(
                !side_task.is_finished(),
                "main cancellation stopped the side question"
            );
            stop_side.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(1), side_task).await;
        } else {
            stop_side.cancel();
            let a = tokio::time::timeout(Duration::from_secs(1), side_task)
                .await
                .expect("side cancellation stuck")
                .unwrap();
            assert_eq!(a.usage.input_tokens, 7, "cancelled usage lost");
            assert!(
                !main_task.is_finished(),
                "side cancellation stopped the main loop"
            );
            stop_main.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(1), main_task).await;
        }
    }
}
