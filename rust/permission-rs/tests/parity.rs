//! Parity harness for `permission-rs`. Replays `cases.jsonl` (recorded from the real
//! Go `norma/permission.Evaluate` / `Allowed` / `Denied` / `AskUser` by `record.sh`)
//! against the Rust port.

use serde_json::Value;

use permission_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

/// Decode a recorded `[]string` arg. Go marshals a nil slice as `null` and an empty
/// slice as `[]`; both fold to an empty Vec here.
fn decode_string_list(v: &Value) -> Vec<String> {
    serde_json::from_value::<Option<Vec<String>>>(v.clone())
        .expect("string list decode")
        .unwrap_or_default()
}

/// Reconstruct a `Request` from the recorded args object. The host callback is modeled
/// by its resolved return value (`ask`): `null` -> no callback (None), otherwise the
/// recorded Decision.
fn decode_request(v: &Value) -> Request {
    let tool_decision: Decision =
        serde_json::from_value(v["toolDecision"].clone()).unwrap_or_else(|_| Decision::default());
    let ask: Option<Decision> = if v["ask"].is_null() {
        None
    } else {
        Some(serde_json::from_value(v["ask"].clone()).expect("ask decision decode"))
    };
    Request {
        tool_name: v["tool"].as_str().unwrap_or("").to_string(),
        input: None,
        is_read_only: v["readOnly"].as_bool().unwrap_or(false),
        tool_decision,
        mode: v["mode"].as_str().unwrap_or("").to_string(),
        allowed: decode_string_list(&v["allowed"]),
        disallowed: decode_string_list(&v["disallowed"]),
        ask,
    }
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "Allowed" => serde_json::to_value(allowed()).expect("serialize decision"),
        "Denied" => {
            let msg = args[0].as_str().expect("message arg");
            serde_json::to_value(denied(msg)).expect("serialize decision")
        }
        "AskUser" => {
            let msg = args[0].as_str().expect("message arg");
            serde_json::to_value(ask_user(msg)).expect("serialize decision")
        }
        "Evaluate" => {
            let req = decode_request(&args[0]);
            serde_json::to_value(evaluate(&req)).expect("serialize decision")
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
