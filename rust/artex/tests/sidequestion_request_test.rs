//! Rust port of `sidequestion/sidequestion_test.go`'s
//! `TestBuildRequestCompactionToolPairingAndBudget` — the spec for `BuildRequest`:
//! compaction-boundary pairing, the recent-exchange cap, and budget-driven history
//! reduction. The streaming/service/capture specs land with their own units.

use artex::llm::{
    boundary_message, tool_result_text, user_text, BoundaryMeta, CompletionRequest, ContentBlock,
    Message, ToolSchema, BLOCK_THINKING, BLOCK_TOOL_USE, ROLE_ASSISTANT, ROLE_USER,
};
use artex::sidequestion::{build_request, Exchange, Model, Snapshot};
use serde_json::json;

/// Port of the Go test's `fixture()`: a small request whose message 2 is an assistant
/// turn pairing a signed thinking block with a tool_use, and message 3 a matching
/// user tool_result. The `input` / `input_schema` are structured JSON so the deep-copy
/// path is meaningful.
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

#[test]
fn build_request_compaction_tool_pairing_and_budget() {
    let mut s = Snapshot {
        request: fixture(),
        model: Model {
            window_tokens: 100_000,
            ..Default::default()
        },
        ..Default::default()
    };
    // s.Request.Messages = append(messages, BoundaryMessage, UserText("compressed
    // summary"), messages[3]) — the original tool_result message re-appended after a
    // compaction boundary, so it is orphaned and must be dropped by pairing.
    let tool_result_msg = s.request.messages[3].clone();
    s.request.messages.push(boundary_message(&BoundaryMeta {
        trigger: "auto".to_string(),
        ..Default::default()
    }));
    s.request.messages.push(user_text("compressed summary"));
    s.request.messages.push(tool_result_msg);

    let history: Vec<Exchange> = (0..25)
        .map(|_| Exchange {
            question: "q".repeat(100),
            answer: "answer".to_string(),
            status: "completed".to_string(),
            ..Default::default()
        })
        .collect();

    // Large window: everything fits, history capped at MaxRecentExchanges (20), the
    // compaction boundary leaves "compressed summary" as the first message.
    let r = build_request(&s, &history, "current question").expect("build large");
    assert_eq!(
        r.messages.len(),
        42,
        "history cap/compaction: {} messages",
        r.messages.len()
    );
    assert_eq!(r.messages[0].text(), "compressed summary");
    for m in &r.messages {
        for b in &m.content {
            assert_ne!(b.block_type, "tool_result", "orphan tool result survived");
        }
    }

    // Mid window: the input budget forces the oldest exchanges out.
    s.model.window_tokens = 1000;
    let r = build_request(&s, &history, "current question").expect("build small");
    assert!(
        r.messages.len() < 42,
        "budget did not reduce side history: {}",
        r.messages.len()
    );
    assert!(r.messages.len() >= 2, "budget dropped the bare base");

    // Tiny window: even the base + question cannot fit → ErrContextBudget.
    s.model.window_tokens = 10;
    assert!(
        build_request(&s, &[], "question").is_err(),
        "oversized base accepted"
    );
}
