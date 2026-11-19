//! Parity harness for `rewritex-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `traffic/rewrite/` helpers by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use rewritex_rs::port::*;

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

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args[i].as_i64().expect("int arg")
}

fn arg_byte_slice(args: &[Value], i: usize) -> Vec<u8> {
    match &args[i] {
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_i64().expect("int element") as u8)
            .collect(),
        other => panic!("expected array arg, got {other}"),
    }
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "validToken" => json!(valid_token(&arg_str(args, 0))),
        "nulInHead" => json!(nul_in_head(&arg_byte_slice(args, 0))),
        "actionClass" => json!(action_class(&arg_str(args, 0))),
        "bodyAllowedForStatus" => json!(body_allowed_for_status(arg_i64(args, 0))),
        "headerTarget" => match header_target(&arg_str(args, 0)) {
            Ok(c) => json!([c, ""]),
            Err(e) => json!(["", e]),
        },
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
