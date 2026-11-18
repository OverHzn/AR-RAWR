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
