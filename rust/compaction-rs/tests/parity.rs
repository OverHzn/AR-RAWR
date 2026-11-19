//! Parity harness for `compaction-rs`. Replays `cases.jsonl` (recorded from the real
//! Go `norma/compaction` functions by `record.sh`) against the Rust port.

use llm_rs::port::Message;
use serde_json::{Value, json};

use compaction_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "EstimateTokens" => {
            // Go passes a nil slice as JSON `null`; model that as an empty Vec.
            let msgs: Vec<Message> =
                serde_json::from_value::<Option<Vec<Message>>>(args[0].clone())
                    .expect("messages decode")
                    .unwrap_or_default();
            json!(estimate_tokens(&msgs))
        }
        "IsOverflow" => {
            // null -> Go nil error; string -> errors.New(s).
            let err = args[0].as_str();
            json!(is_overflow(err))
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
