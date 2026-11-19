//! Parity harness for `transcript-rs`. Replays `cases.jsonl` (recorded from the real
//! Go `norma/transcript.Messages` / `LastUUID` by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use transcript_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

/// Decode the recorded `[]transcript.Record` arg. Go marshals a nil slice as `null`
/// and an empty slice as `[]`; both fold to an empty Vec here.
fn decode_records(v: &Value) -> Vec<Record> {
    serde_json::from_value::<Option<Vec<Record>>>(v.clone())
        .expect("records decode")
        .unwrap_or_default()
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "Messages" => {
            let recs = decode_records(&args[0]);
            let (msgs, usage) = messages(&recs);
            // Go returns a nil []llm.Message when nothing is collected; model that as
            // JSON null (an empty Vec would wrongly serialize to []).
            let msgs_json = if msgs.is_empty() {
                Value::Null
            } else {
                serde_json::to_value(&msgs).expect("serialize messages")
            };
            let usage_json = serde_json::to_value(usage).expect("serialize usage");
            json!({ "messages": msgs_json, "usage": usage_json })
        }
        "LastUUID" => {
            let recs = decode_records(&args[0]);
            json!(last_uuid(&recs))
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
