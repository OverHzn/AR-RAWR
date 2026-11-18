//! Parity harness for `llm-rs`. Replays `cases.jsonl` (recorded from the real Go
//! `norma/llm` package by `record.sh`) against the Rust port.
//!
//! Go's `ToolUses` returns a nil slice when a message has no tool_use blocks, which
//! `json.Marshal` renders as `null` (not `[]`); `dispatch` reproduces that distinction.

use llm_rs::port::*;
use serde_json::{Value, json};

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).expect("arg decodes")
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "Text" => {
            let m: Message = parse(&args[0]);
            json!(m.text())
        }
        "ToolUses" => {
            let m: Message = parse(&args[0]);
            let tu = m.tool_uses();
            if tu.is_empty() {
                Value::Null // Go nil slice -> null
            } else {
                serde_json::to_value(tu).expect("serialize blocks")
            }
        }
        "Add" => {
            let mut u: Usage = parse(&args[0]);
            let o: Usage = parse(&args[1]);
            u.add(o);
            serde_json::to_value(u).expect("serialize usage")
        }
        "MergeEcho" => {
            let mut u: Usage = parse(&args[0]);
            let o: Usage = parse(&args[1]);
            u.merge_echo(o);
            serde_json::to_value(u).expect("serialize usage")
        }
        other => panic!("no dispatcher arm for {other}"),
    }
}

#[test]
fn parity_all_cases() {
    let all = cases();
    assert!(!all.is_empty(), "cases.jsonl must not be empty");
    let mut mismatches: Vec<String> = Vec::new();
    for c in &all {
        let name = c["fn"].as_str().expect("fn");
        let args: Vec<Value> = c["args"].as_array().expect("args array").clone();
        if c.get("error").is_some() {
            mismatches.push(format!("UNEXPECTED error case {name} args={args:?}"));
            continue;
        }
        let actual = dispatch(name, &args);
        let expected = &c["ok"];
        if &actual != expected {
            mismatches.push(format!(
                "{name} args={args:?}\n   expected={expected}\n   actual  ={actual}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} / {} cases mismatched:\n\n{}",
        mismatches.len(),
        all.len(),
        mismatches.join("\n")
    );
}

// boundary.go: normed directly against the read Go source (values embedded). These are
// not part of cases.jsonl — the exported Go functions are reached by bridge crates
// (agent-rs), whose recorded cases prove parity; here we lock the same semantics.
#[test]
fn boundary_direct() {
    fn m(role: &str, blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: role.to_string(),
            content: blocks,
        }
    }
    fn txt(s: &str) -> ContentBlock {
        ContentBlock {
            block_type: BLOCK_TEXT.into(),
            text: s.into(),
            ..ContentBlock::default()
        }
    }
    fn tu(id: &str) -> ContentBlock {
        ContentBlock {
            block_type: BLOCK_TOOL_USE.into(),
            id: id.into(),
            ..ContentBlock::default()
        }
    }
    fn tr(id: &str) -> ContentBlock {
        ContentBlock {
            block_type: BLOCK_TOOL_RESULT.into(),
            tool_use_id: id.into(),
            ..ContentBlock::default()
        }
    }
    fn bmark() -> ContentBlock {
        ContentBlock {
            block_type: BLOCK_COMPACT_BOUNDARY.into(),
            input: Some(json!({"pre_tokens": 5, "messages_summarized": 2, "post_tokens": 3})),
            ..ContentBlock::default()
        }
    }

    // IsBoundaryMarker
    assert!(is_boundary_marker(&m("assistant", vec![bmark()])));
    assert!(!is_boundary_marker(&m("user", vec![bmark()])));
    assert!(!is_boundary_marker(&m(
        "assistant",
        vec![bmark(), txt("x")]
    )));
    assert!(!is_boundary_marker(&m("assistant", vec![txt("x")])));
    assert!(!is_boundary_marker(&m("assistant", vec![])));

    // LastBoundaryIndex
    assert_eq!(
        last_boundary_index(&[
            m("user", vec![txt("a")]),
            m("assistant", vec![bmark()]),
            m("user", vec![txt("b")]),
        ]),
        1
    );
    assert_eq!(last_boundary_index(&[m("user", vec![txt("a")])]), -1);
    assert_eq!(
        last_boundary_index(&[
            m("assistant", vec![bmark()]),
            m("user", vec![txt("x")]),
            m("assistant", vec![bmark()]),
        ]),
        2
    );
    assert_eq!(last_boundary_index(&[]), -1);

    // MessagesAfterBoundary
    let after = messages_after_boundary(&[
        m("user", vec![txt("a")]),
        m("assistant", vec![bmark()]),
        m("user", vec![txt("b")]),
    ]);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].content[0].text, "b");
    assert_eq!(
        messages_after_boundary(&[m("user", vec![txt("a")]), m("user", vec![txt("b")])]).len(),
        2
    );

    // PairToolBlocks: paired kept; orphan result dropped; dangling tool_use KEPT (Go has no final filter).
    let paired = pair_tool_blocks(&[m("assistant", vec![tu("t1")]), m("user", vec![tr("t1")])]);
    assert_eq!(paired.len(), 2);
    assert_eq!(pair_tool_blocks(&[m("user", vec![tr("t1")])]).len(), 0); // orphan result -> empty
    assert_eq!(pair_tool_blocks(&[m("assistant", vec![tu("t1")])]).len(), 1); // dangling use kept
    let multi = pair_tool_blocks(&[
        m("assistant", vec![tu("t1"), tu("t2")]),
        m("user", vec![tr("t1")]),
    ]);
    assert_eq!(multi.len(), 2);
    assert_eq!(multi[0].content.len(), 2); // both tool_use kept
    assert_eq!(multi[1].content.len(), 1); // tr(t1) kept

    // MessagesForAPI = after-boundary then pair.
    let api = messages_for_api(&[
        m("user", vec![txt("a")]),
        m("assistant", vec![bmark()]),
        m("user", vec![txt("b")]),
    ]);
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].content[0].text, "b");

    // BoundaryMessage carries the meta as the boundary block's input.
    let meta = BoundaryMeta {
        trigger: "auto".into(),
        pre_tokens: 100,
        messages_summarized: 5,
        post_tokens: 200,
    };
    let bm = boundary_message(&meta);
    assert_eq!(bm.role, "assistant");
    assert_eq!(bm.content.len(), 1);
    assert_eq!(bm.content[0].block_type, BLOCK_COMPACT_BOUNDARY);
    assert_eq!(
        bm.content[0].input,
        Some(
            json!({"trigger": "auto", "pre_tokens": 100, "messages_summarized": 5, "post_tokens": 200})
        )
    );
    // empty trigger is omitted
    let bm2 = boundary_message(&BoundaryMeta {
        trigger: String::new(),
        pre_tokens: 1,
        messages_summarized: 1,
        post_tokens: 1,
    });
    assert_eq!(
        bm2.content[0].input,
        Some(json!({"pre_tokens": 1, "messages_summarized": 1, "post_tokens": 1}))
    );

    // ParseBoundaryMeta: text branch + completeness checks.
    let pi = |text: &str| ParseBoundaryMetaInput {
        has_text: true,
        text: text.to_string(),
        ..ParseBoundaryMetaInput::default()
    };
    assert_eq!(
        parse_boundary_meta(&pi(
            r#"{"pre_tokens":100,"messages_summarized":5,"post_tokens":200}"#
        ))
        .1,
        BoundaryStatus::Complete
    );
    assert_eq!(
        parse_boundary_meta(&pi(
            r#"{"pre_tokens":0,"messages_summarized":5,"post_tokens":200}"#
        ))
        .1,
        BoundaryStatus::NoTokens
    );
    assert_eq!(
        parse_boundary_meta(&pi(
            r#"{"pre_tokens":100,"messages_summarized":0,"post_tokens":200}"#
        ))
        .1,
        BoundaryStatus::NoMessage
    );
    assert_eq!(
        parse_boundary_meta(&pi(
            r#"{"pre_tokens":100,"messages_summarized":5,"post_tokens":-1}"#
        ))
        .1,
        BoundaryStatus::Incomplete
    );
    assert_eq!(
        parse_boundary_meta(&pi("garbage")).1,
        BoundaryStatus::NoTokens
    ); // parse fail -> zero meta
    let (meta2, st2) = parse_boundary_meta(&pi(
        r#"{"trigger":"auto","pre_tokens":10,"messages_summarized":2,"post_tokens":5}"#,
    ));
    assert_eq!(st2, BoundaryStatus::Complete);
    assert_eq!(meta2.trigger, "auto");
    // Content/Tool branches -> none.
    let none_ci = |f: fn(&mut ParseBoundaryMetaInput)| {
        let mut x = ParseBoundaryMetaInput::default();
        f(&mut x);
        parse_boundary_meta(&x).1
    };
    assert_eq!(none_ci(|x| x.has_content = true), BoundaryStatus::None);
    assert_eq!(none_ci(|x| x.has_thinking = true), BoundaryStatus::None);
    assert_eq!(none_ci(|x| x.has_tool_use_id = true), BoundaryStatus::None);
    assert_eq!(none_ci(|x| x.has_tool_calls = true), BoundaryStatus::None);
    // text wins over a tool-use-id.
    let mut both = pi(r#"{"pre_tokens":100,"messages_summarized":5,"post_tokens":200}"#);
    both.has_tool_use_id = true;
    assert_eq!(parse_boundary_meta(&both).1, BoundaryStatus::Complete);

    // BoundaryStatus::as_str
    assert_eq!(BoundaryStatus::Complete.as_str(), "complete");
    assert_eq!(BoundaryStatus::None.as_str(), "none");
    assert_eq!(BoundaryStatus::NoMessage.as_str(), "no_message");
    assert_eq!(BoundaryStatus::NoTokens.as_str(), "no_tokens");
    assert_eq!(BoundaryStatus::Incomplete.as_str(), "incomplete");
}
