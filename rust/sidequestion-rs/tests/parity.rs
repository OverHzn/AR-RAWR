//! Parity harness for `sidequestion-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `sidequestion.Parent.Key` by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use llm_rs::port::{CompletionRequest, Message};
use sidequestion_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args[i].as_i64().expect("int arg")
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "Parent.Key" => {
            let p = Parent {
                conversation_id: arg_i64(args, 0),
                task_id: arg_i64(args, 1),
                exploration_id: arg_i64(args, 2),
                intent_id: arg_i64(args, 3),
            };
            json!(p.key())
        }
        "outputBudget" => json!(output_budget(arg_i64(args, 0), arg_i64(args, 1))),
        "inputBudget" => json!(input_budget(arg_i64(args, 0), arg_i64(args, 1))),
        "EstimateInputTokens" => {
            let req: CompletionRequest =
                serde_json::from_value(args[0].clone()).expect("request decode");
            json!(estimate_input_tokens(&req))
        }
        "messageGroups" => {
            // Go passes a nil slice as JSON `null`; model that as an empty Vec.
            let msgs: Vec<Message> =
                serde_json::from_value::<Option<Vec<Message>>>(args[0].clone())
                    .expect("messages decode")
                    .unwrap_or_default();
            let groups = message_groups(&msgs);
            if groups.is_empty() {
                Value::Null // Go nil slice -> null
            } else {
                serde_json::to_value(groups).expect("serialize groups")
            }
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
