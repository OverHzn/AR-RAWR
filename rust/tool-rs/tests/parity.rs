//! Parity harness for `tool-rs`. Replays `cases.jsonl` (recorded from the real Go
//! `norma/tool` helpers by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use tool_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn arg_str(args: &[Value], i: usize) -> String {
    args[i].as_str().expect("string arg").to_string()
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "Text" => serde_json::to_value(text(&arg_str(args, 0))).expect("serialize result"),
        "Errorf" => serde_json::to_value(errorf(&arg_str(args, 0))).expect("serialize result"),
        "RenderDeferredToolsBlock" => {
            let names: Vec<String> = serde_json::from_value::<Option<Vec<String>>>(args[0].clone())
                .expect("names decode")
                .unwrap_or_default();
            json!(render_deferred_tools_block(&names))
        }
        "RenderTodoReminder" => {
            let todos: Vec<Todo> = serde_json::from_value::<Option<Vec<Todo>>>(args[0].clone())
                .expect("todos decode")
                .unwrap_or_default();
            json!(render_todo_reminder(&todos))
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
