//! Rust port of `sidequestion/context_test.go` — the specs for
//! `SideQuestionService::respond`: bounded context preparation (history folding,
//! snapshot compaction, the summary-call cap), at most one overflow recovery that
//! counts the failed attempt's usage, and the rule that cancellation / a failed
//! summary never advances the persisted memory cursor. The pure `BuildRequest`
//! spec lives in `sidequestion_request_test.rs`; the `Answer` spec in
//! `sidequestion_service_test.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use artex::llm::{
    text_block, tool_result_text, user_text, CompletionRequest, ContentBlock, Context, Error,
    Message, Provider, StreamEvent, ToolSchema, Usage, BLOCK_THINKING, BLOCK_TOOL_USE,
    ROLE_ASSISTANT, ROLE_USER, SE_TEXT_DELTA,
};
use artex::sidequestion::{
    estimate_input_tokens, is_context_budget, ContextInfo, ContextOptions, Exchange, GoTime,
    Memory, Model, Replay, SideQuestionService, Snapshot, SUMMARY_INSTRUCTION,
};
use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::json;

// === Scriptable provider (Go `fakeProvider`) ===

type CompleteFullFn = Box<
    dyn Fn(
            Context,
            CompletionRequest,
        ) -> BoxFuture<'static, (Message, String, Usage, Option<Error>)>
        + Send
        + Sync,
>;
type StreamFn = Box<
    dyn Fn(Context, CompletionRequest) -> BoxStream<'static, Result<StreamEvent, Error>>
        + Send
        + Sync,
>;

struct FakeProvider {
    complete_full: CompleteFullFn,
    stream: StreamFn,
}

#[async_trait]
impl Provider for FakeProvider {
    async fn complete(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error> {
        let (m, s, u, e) = (self.complete_full)(ctx, req).await;
        match e {
            Some(e) => Err(e),
            None => Ok((m, s, u)),
        }
    }
    fn stream(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> BoxStream<'static, Result<StreamEvent, Error>> {
        (self.stream)(ctx, req)
    }
    async fn complete_full(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> (Message, String, Usage, Option<Error>) {
        (self.complete_full)(ctx, req).await
    }
}

/// A provider whose atomic + streaming paths are both unused-but-required; build the
/// one you need with `provider_complete` / `provider_stream`.
fn provider_complete(f: CompleteFullFn) -> FakeProvider {
    let stream: StreamFn = Box::new(|_c, _r| {
        Box::pin(async_stream::stream! {
            yield Err(Error::msg("stream unused"));
        })
    });
    FakeProvider {
        complete_full: f,
        stream,
    }
}

fn provider_stream(f: StreamFn) -> FakeProvider {
    let complete_full: CompleteFullFn =
        Box::new(|_c, _r| Box::pin(async { unreachable!("atomic unused") }));
    FakeProvider {
        complete_full,
        stream: f,
    }
}

// === Go test helpers ===

fn assistant(text: &str) -> Message {
    Message {
        role: ROLE_ASSISTANT.to_string(),
        content: vec![text_block(text)],
    }
}

fn assistant_tool_use(id: &str, name: &str) -> Message {
    Message {
        role: ROLE_ASSISTANT.to_string(),
        content: vec![ContentBlock {
            block_type: BLOCK_TOOL_USE.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            ..Default::default()
        }],
    }
}

/// Go's zero `llm.Message{}` (nil role + nil content), returned by the failure-mode
/// provider so the answer path sees no text and no tool use.
fn empty_message() -> Message {
    Message {
        role: String::new(),
        content: Vec::new(),
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

/// `replayFixture`: pages completed exchanges (max 20) after the cursor and persists
/// memory through the shared handle. `Replay.memory` seeds from the current value.
fn replay_fixture(history: Vec<Exchange>, memory: Arc<Mutex<Memory>>) -> Replay {
    let initial = memory.lock().unwrap().clone();
    let load: artex::sidequestion::LoadFn = Arc::new(move |_ctx: Context, after: i64| {
        let mut page: Vec<Exchange> = Vec::new();
        for e in &history {
            if e.ordinal > after && e.status == "completed" {
                page.push(e.clone());
            }
            if page.len() == 20 {
                break;
            }
        }
        Ok(page)
    });
    let save: artex::sidequestion::SaveFn = {
        let m = memory.clone();
        Arc::new(move |_ctx: Context, value: Memory| {
            *m.lock().unwrap() = value;
            Ok(())
        })
    };
    Replay {
        memory: initial,
        load: Some(load),
        save: Some(save),
    }
}

fn exchange(ordinal: i64, question: &str, answer: &str) -> Exchange {
    Exchange {
        ordinal,
        question: question.to_string(),
        answer: answer.to_string(),
        status: "completed".to_string(),
        snapshot_at: GoTime::unix(ordinal),
        ..Default::default()
    }
}

async fn respond(
    p: Arc<FakeProvider>,
    snap: Snapshot,
    question: &str,
    replay: Replay,
) -> (artex::sidequestion::Answer, ContextInfo, Option<Error>) {
    SideQuestionService::new(p)
        .respond(
            Context::background(),
            snap,
            question,
            replay,
            ContextOptions::default(),
            None,
        )
        .await
}

// === TestSideTwentyExchangeBoundaryAndRestart ===

#[tokio::test]
async fn twenty_exchange_boundary_and_restart() {
    for n in [19usize, 20, 21, 50] {
        let mut history: Vec<Exchange> = (1..=n as i64)
            .map(|i| exchange(i, &format!("question-{i}"), "historical answer"))
            .collect();
        history[0].answer = "EARLY-DECISION-ALPHA".to_string();

        let requests: Arc<Mutex<Vec<CompletionRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let summaries = Arc::new(AtomicUsize::new(0));
        let req_c = requests.clone();
        let sum_c = summaries.clone();
        let p = Arc::new(provider_complete(Box::new(
            move |_ctx, r: CompletionRequest| {
                let reqs = req_c.clone();
                let sums = sum_c.clone();
                Box::pin(async move {
                    if r.tools.is_empty()
                        && !r.system.is_empty()
                        && r.system[0] == SUMMARY_INSTRUCTION
                    {
                        sums.fetch_add(1, Ordering::SeqCst);
                        if !r.messages[0].text().contains("EARLY-DECISION-ALPHA") {
                            panic!("rolling update lost prior summary");
                        }
                        return (
                            assistant(
                                "EARLY-DECISION-ALPHA; historical discussion, not current evidence",
                            ),
                            "end_turn".to_string(),
                            Usage {
                                input_tokens: 11,
                                ..Default::default()
                            },
                            None,
                        );
                    }
                    reqs.lock().unwrap().push(r);
                    (
                        assistant("answer"),
                        "end_turn".to_string(),
                        Usage {
                            input_tokens: 7,
                            ..Default::default()
                        },
                        None,
                    )
                })
            },
        )));

        let snap = Snapshot {
            request: fixture(),
            model: Model {
                window_tokens: 200_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let memory = Arc::new(Mutex::new(Memory::default()));
        let (out, info, err) = respond(
            p.clone(),
            snap.clone(),
            "what was my first decision?",
            replay_fixture(history.clone(), memory.clone()),
        )
        .await;
        assert!(err.is_none(), "n={n}: {err:?}");
        assert_eq!(
            info.recent_exchanges,
            (n as i64).min(20),
            "n={n}: recent_exchanges"
        );
        assert_eq!(
            memory.lock().unwrap().through,
            (n as i64 - 20).max(0),
            "n={n}: persisted cursor"
        );
        let s = summaries.load(Ordering::SeqCst) as i64;
        assert_eq!(
            out.usage.input_tokens,
            11 * s + 7,
            "n={n}: summary usage missing"
        );
        let first = serde_json::to_string(&requests.lock().unwrap()[0]).unwrap();
        assert!(
            first.contains("EARLY-DECISION-ALPHA"),
            "n={n}: first-round decision absent after round 20"
        );

        let before = summaries.load(Ordering::SeqCst);
        let (_o2, _i2, err2) = respond(
            p.clone(),
            snap.clone(),
            "continue after restart",
            replay_fixture(history.clone(), memory.clone()),
        )
        .await;
        assert!(
            err2.is_none() && summaries.load(Ordering::SeqCst) == before,
            "n={n}: persisted summary not reused"
        );
    }
}

// === TestSideBudgetCountsContentNotJSONCharacters ===

#[test]
fn budget_counts_content_not_json_characters() {
    let mut snap = Snapshot {
        request: fixture(),
        model: Model {
            window_tokens: 200_000,
            ..Default::default()
        },
        ..Default::default()
    };
    snap.request.max_tokens = 32768;
    snap.request.messages.push(user_text(
        &"<script>const result = api.response;</script>\n".repeat(5500),
    ));

    let before = serde_json::to_string(&snap).unwrap();
    assert!(
        before.len() >= 200_000,
        "fixture too small to exercise regression"
    );
    let req = artex::sidequestion::build_request(&snap, &[], "progress?")
        .expect("valid code-heavy snapshot rejected");
    assert_eq!(req.max_tokens, 8192, "output budget");
    assert!(estimate_input_tokens(&req) < 200_000, "budget");
    assert_eq!(
        serde_json::to_string(&snap).unwrap(),
        before,
        "snapshot mutated"
    );

    let baseline = estimate_input_tokens(&CompletionRequest {
        messages: vec![user_text("x")],
        ..Default::default()
    });
    let with_sys = CompletionRequest {
        messages: vec![user_text("x")],
        system: vec!["\u{4e2d}".repeat(3000)],
        tools: fixture().tools,
        ..Default::default()
    };
    assert!(
        estimate_input_tokens(&with_sys) > baseline + 3000,
        "system/schema/CJK omitted"
    );
}

// === TestSideLongExchangeAndSnapshotCompactionAreBounded ===

#[tokio::test]
async fn long_exchange_and_snapshot_compaction_are_bounded() {
    for huge_history in [true, false] {
        let mut snap = Snapshot {
            request: fixture(),
            model: Model {
                window_tokens: 32_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let history: Vec<Exchange> = if huge_history {
            vec![Exchange {
                ordinal: 1,
                question: "earlier".to_string(),
                answer: "historical evidence ABC ".repeat(14000),
                status: "completed".to_string(),
                ..Default::default()
            }]
        } else {
            let mut msgs = vec![user_text(&"old tool evidence text ".repeat(9000))];
            msgs.extend(snap.request.messages.clone());
            snap.request.messages = msgs;
            Vec::new()
        };
        let original = serde_json::to_string(&snap).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let answers = Arc::new(AtomicUsize::new(0));
        let final_req: Arc<Mutex<Option<CompletionRequest>>> = Arc::new(Mutex::new(None));
        let c_c = calls.clone();
        let a_c = answers.clone();
        let f_c = final_req.clone();
        let p = Arc::new(provider_complete(Box::new(
            move |_ctx, r: CompletionRequest| {
                let calls = c_c.clone();
                let answers = a_c.clone();
                let final_req = f_c.clone();
                Box::pin(async move {
                    if r.thinking == "disabled" {
                        calls.fetch_add(1, Ordering::SeqCst);
                        assert!(r.tools.is_empty(), "unsafe summary request: tools");
                        assert!(
                            estimate_input_tokens(&r) + r.max_tokens <= 32_000,
                            "unsafe summary request: budget"
                        );
                        assert!(
                            r.messages[0].text().chars().all(|c| c != '\u{FFFD}'),
                            "unsafe summary request: invalid utf8"
                        );
                        return (
                            assistant("goal, evidence and unresolved questions"),
                            "end_turn".to_string(),
                            Usage {
                                output_tokens: 3,
                                ..Default::default()
                            },
                            None,
                        );
                    }
                    answers.fetch_add(1, Ordering::SeqCst);
                    *final_req.lock().unwrap() = Some(r);
                    (
                        assistant("done"),
                        "end_turn".to_string(),
                        Usage {
                            output_tokens: 5,
                            ..Default::default()
                        },
                        None,
                    )
                })
            },
        )));

        let memory = Arc::new(Mutex::new(Memory::default()));
        let (out, info, err) = respond(
            p.clone(),
            snap.clone(),
            "progress?",
            replay_fixture(history.clone(), memory.clone()),
        )
        .await;
        let c = calls.load(Ordering::SeqCst);
        assert!(err.is_none(), "huge={huge_history}: {err:?}");
        assert!((2..=12).contains(&c), "huge={huge_history}: calls={c}");
        assert_eq!(
            answers.load(Ordering::SeqCst),
            1,
            "huge={huge_history}: answers"
        );
        assert_eq!(
            out.usage.output_tokens,
            3 * c as i64 + 5,
            "huge={huge_history}: output usage"
        );
        assert!(
            info.estimated_input_tokens <= info.input_budget,
            "huge={huge_history}: oversized final request {}>{}",
            info.estimated_input_tokens,
            info.input_budget
        );
        let fin = final_req.lock().unwrap().clone().unwrap();
        assert!(
            fin.messages == artex::llm::messages_for_api(&fin.messages),
            "huge={huge_history}: tool pairing broken"
        );
        assert!(
            serde_json::to_string(&fin)
                .unwrap()
                .contains("verified asset"),
            "huge={huge_history}: recent tool evidence discarded"
        );
        assert_eq!(
            serde_json::to_string(&snap).unwrap(),
            original,
            "huge={huge_history}: main snapshot modified"
        );

        let before = calls.load(Ordering::SeqCst);
        let (_o, _i, err) = respond(
            p.clone(),
            snap.clone(),
            "another question",
            replay_fixture(history.clone(), memory.clone()),
        )
        .await;
        assert!(
            err.is_none() && calls.load(Ordering::SeqCst) == before,
            "huge={huge_history}: summary cache not reused"
        );

        if !huge_history {
            snap.version += 1;
            let (_o, _i, err) = respond(
                p.clone(),
                snap.clone(),
                "new snapshot",
                replay_fixture(history.clone(), memory.clone()),
            )
            .await;
            assert!(
                err.is_none() && calls.load(Ordering::SeqCst) != before,
                "stale snapshot cache reused"
            );
        }
    }
}

// === TestSideOverflowRecoveryOnceAndFailureUsage ===

#[tokio::test]
async fn overflow_recovery_once_and_failure_usage() {
    for twice in [false, true] {
        let answers = Arc::new(AtomicUsize::new(0));
        let summaries = Arc::new(AtomicUsize::new(0));
        let a_c = answers.clone();
        let s_c = summaries.clone();
        let p = Arc::new(provider_complete(Box::new(
            move |_ctx, r: CompletionRequest| {
                let answers = a_c.clone();
                let summaries = s_c.clone();
                Box::pin(async move {
                    if r.thinking == "disabled" {
                        summaries.fetch_add(1, Ordering::SeqCst);
                        return (
                            assistant("summary"),
                            "end_turn".to_string(),
                            Usage {
                                input_tokens: 3,
                                ..Default::default()
                            },
                            None,
                        );
                    }
                    let n = answers.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 1 || twice {
                        return (
                            empty_message(),
                            String::new(),
                            Usage {
                                input_tokens: 5,
                                ..Default::default()
                            },
                            Some(Error::msg("context_length_exceeded")),
                        );
                    }
                    (
                        assistant("recovered"),
                        "end_turn".to_string(),
                        Usage {
                            input_tokens: 7,
                            ..Default::default()
                        },
                        None,
                    )
                })
            },
        )));

        let mut snap = Snapshot {
            request: fixture(),
            model: Model {
                window_tokens: 32_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut msgs = vec![user_text(&"old context ".repeat(2000))];
        msgs.extend(snap.request.messages.clone());
        snap.request.messages = msgs;

        let (out, info, err) = respond(p.clone(), snap, "question", Replay::default()).await;
        assert_eq!(answers.load(Ordering::SeqCst), 2, "twice={twice}: answers");
        assert_eq!(
            summaries.load(Ordering::SeqCst),
            1,
            "twice={twice}: summaries"
        );
        assert!(info.overflow_retried, "twice={twice}: not retried");
        assert_eq!(
            err.is_some(),
            twice,
            "twice={twice}: unexpected err {err:?}"
        );
        let want = if twice { 13 } else { 15 };
        assert_eq!(out.usage.input_tokens, want, "twice={twice}: usage");
    }

    // A stream that already exposed text must never replay it as a fresh answer.
    let count = Arc::new(AtomicUsize::new(0));
    let cnt = count.clone();
    let p = Arc::new(provider_stream(Box::new(move |_ctx, _r| {
        let count = cnt.clone();
        Box::pin(async_stream::stream! {
            count.fetch_add(1, Ordering::SeqCst);
            yield Ok(StreamEvent { ty: SE_TEXT_DELTA.to_string(), text: "partial".to_string(), ..Default::default() });
            yield Err(Error::msg("context_length_exceeded"));
        })
    })));
    let snap = Snapshot {
        request: fixture(),
        model: Model {
            streaming: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let (out, _info, err) = respond(p.clone(), snap, "q", Replay::default()).await;
    assert!(err.is_some(), "partial stream: err missing");
    assert_eq!(out.text, "partial", "partial stream: text");
    assert_eq!(count.load(Ordering::SeqCst), 1, "partial stream retried");
}

// === TestSideSummaryCancellationAndFailureDoNotAdvanceMemory ===

#[tokio::test]
async fn summary_cancellation_and_failure_do_not_advance_memory() {
    for mode in [
        "cancel",
        "error",
        "empty",
        "truncated",
        "too-large",
        "tool",
        "save-failed",
    ] {
        // Go: `ctx, cancel := context.WithCancel(t.Context())`. The provider cancels
        // the context from inside the (first) summary call in "cancel" mode.
        let (ctx, cancel) = Context::with_cancel();
        let calls = Arc::new(AtomicUsize::new(0));
        let c_c = calls.clone();
        let cancel_for_provider = cancel.clone();
        let p = Arc::new(provider_complete(Box::new(
            move |_ctx, r: CompletionRequest| {
                let calls = c_c.clone();
                let cancel = cancel_for_provider.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let usage = Usage {
                        input_tokens: 9,
                        ..Default::default()
                    };
                    assert!(
                        r.thinking == "disabled",
                        "failed summary continued to answer"
                    );
                    let out: (Message, String, Option<Error>) = match mode {
                        "cancel" => {
                            cancel.cancel();
                            (assistant("late"), "end_turn".to_string(), None)
                        }
                        "error" => (
                            empty_message(),
                            String::new(),
                            Some(Error::msg("unavailable")),
                        ),
                        "empty" => (assistant(""), "end_turn".to_string(), None),
                        "truncated" => (assistant("partial"), "length".to_string(), None),
                        "too-large" => (
                            assistant(&"\u{4e2d}".repeat(5000)),
                            "end_turn".to_string(),
                            None,
                        ),
                        "tool" => (
                            assistant_tool_use("forbidden", "Bash"),
                            "tool_use".to_string(),
                            None,
                        ),
                        // "save-failed" falls through to a normal summary; the failure is
                        // driven through the replay's Save callback below.
                        _ => (assistant("summary"), "end_turn".to_string(), None),
                    };
                    (out.0, out.1, usage, out.2)
                })
            },
        )));

        let history: Vec<Exchange> = (1..=21)
            .map(|i| Exchange {
                ordinal: i,
                question: "q".to_string(),
                answer: "a".to_string(),
                status: "completed".to_string(),
                ..Default::default()
            })
            .collect();
        let memory = Arc::new(Mutex::new(Memory::default()));
        let mut replay = replay_fixture(history, memory.clone());
        if mode == "save-failed" {
            let save: artex::sidequestion::SaveFn =
                Arc::new(|_ctx: Context, _v: Memory| Err(Error::msg("cleared")));
            replay.save = Some(save);
        }

        let (out, _info, err) = SideQuestionService::new(p.clone())
            .respond(
                ctx.clone(),
                Snapshot {
                    request: fixture(),
                    ..Default::default()
                },
                "q",
                replay,
                ContextOptions::default(),
                None,
            )
            .await;
        assert!(err.is_some(), "{mode}: err missing");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{mode}: calls");
        assert_eq!(memory.lock().unwrap().through, 0, "{mode}: cursor advanced");
        assert_eq!(out.usage.input_tokens, 9, "{mode}: failure usage missing");
    }
}

// === TestSideSummaryCallCapAndStaticBudget ===

#[tokio::test]
async fn summary_call_cap_and_static_budget() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c_c = calls.clone();
    let p = Arc::new(provider_complete(Box::new(
        move |_ctx, r: CompletionRequest| {
            let calls = c_c.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                assert!(r.thinking == "disabled", "oversized request answered");
                (
                    assistant("bounded summary"),
                    "end_turn".to_string(),
                    Usage {
                        output_tokens: 1,
                        ..Default::default()
                    },
                    None,
                )
            })
        },
    )));

    let mut snap = Snapshot {
        request: fixture(),
        model: Model {
            window_tokens: 32_000,
            ..Default::default()
        },
        ..Default::default()
    };
    snap.request.messages = vec![user_text(&"x".repeat(1_200_000))];
    let (out, _info, err) = respond(p.clone(), snap.clone(), "q", Replay::default()).await;
    assert!(err.is_some(), "expected cap error");
    assert_eq!(calls.load(Ordering::SeqCst), 12, "summary cap calls");
    assert_eq!(out.usage.output_tokens, 12, "summary cap output usage");

    calls.store(0, Ordering::SeqCst);
    snap.request.system = vec!["x".repeat(200_000)];
    let (_o, _i, err) = respond(p.clone(), snap, "q", Replay::default()).await;
    assert!(
        err.as_ref().is_some_and(is_context_budget),
        "static oversized budget: wrong error {err:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "static oversized budget: calls"
    );
}

// === TestSideTwentyLongRepliesRespectSmallWindow ===

#[tokio::test]
async fn twenty_long_replies_respect_small_window() {
    let history: Vec<Exchange> = (1..=20)
        .map(|i| Exchange {
            ordinal: i,
            question: format!("question-{i}"),
            answer: "evidence ".repeat(3000),
            status: "completed".to_string(),
            ..Default::default()
        })
        .collect();
    let calls = Arc::new(AtomicUsize::new(0));
    let c_c = calls.clone();
    let p = Arc::new(provider_complete(Box::new(
        move |_ctx, req: CompletionRequest| {
            let calls = c_c.clone();
            Box::pin(async move {
                assert!(
                    estimate_input_tokens(&req) + req.max_tokens <= 32_000,
                    "request exceeds small model window"
                );
                if req.thinking == "disabled" {
                    calls.fetch_add(1, Ordering::SeqCst);
                    return (
                        assistant("historical objectives and evidence"),
                        "end_turn".to_string(),
                        Usage::default(),
                        None,
                    );
                }
                (
                    assistant("answer"),
                    "end_turn".to_string(),
                    Usage::default(),
                    None,
                )
            })
        },
    )));

    let memory = Arc::new(Mutex::new(Memory::default()));
    let (_out, info, err) = respond(
        p.clone(),
        Snapshot {
            request: fixture(),
            model: Model {
                window_tokens: 32_000,
                ..Default::default()
            },
            ..Default::default()
        },
        "current status?",
        replay_fixture(history, memory.clone()),
    )
    .await;
    assert!(err.is_none(), "{err:?}");
    assert!(info.history_summarized, "history not summarized: {info:?}");
    assert!(calls.load(Ordering::SeqCst) <= 12, "too many summary calls");
    assert_eq!(memory.lock().unwrap().through, 20, "cursor");
}

// === TestSideOverflowWithoutReductionDoesNotRepeatAnswer ===

#[tokio::test]
async fn overflow_without_reduction_does_not_repeat_answer() {
    let answers = Arc::new(AtomicUsize::new(0));
    let summaries = Arc::new(AtomicUsize::new(0));
    let a_c = answers.clone();
    let s_c = summaries.clone();
    let p = Arc::new(provider_complete(Box::new(
        move |_ctx, req: CompletionRequest| {
            let answers = a_c.clone();
            let summaries = s_c.clone();
            Box::pin(async move {
                if req.thinking == "disabled" {
                    summaries.fetch_add(1, Ordering::SeqCst);
                    return (
                        assistant(&"longer summary ".repeat(100)),
                        "end_turn".to_string(),
                        Usage {
                            input_tokens: 3,
                            ..Default::default()
                        },
                        None,
                    );
                }
                answers.fetch_add(1, Ordering::SeqCst);
                (
                    empty_message(),
                    String::new(),
                    Usage {
                        input_tokens: 5,
                        ..Default::default()
                    },
                    Some(Error::msg("context_length_exceeded")),
                )
            })
        },
    )));

    let snapshot = Snapshot {
        request: CompletionRequest {
            max_tokens: 128,
            messages: vec![user_text("tiny")],
            ..Default::default()
        },
        ..Default::default()
    };
    let (out, _info, err) = respond(p.clone(), snapshot, "question", Replay::default()).await;
    assert!(err.is_some(), "expected no-progress error");
    assert!(
        err.as_ref()
            .unwrap()
            .message()
            .contains("could not further reduce"),
        "wrong error: {}",
        err.as_ref().unwrap().message()
    );
    assert_eq!(answers.load(Ordering::SeqCst), 1, "answers repeated");
    assert_eq!(summaries.load(Ordering::SeqCst), 1, "summaries");
    assert_eq!(out.usage.input_tokens, 8, "usage");
}
