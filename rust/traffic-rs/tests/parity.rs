//! Parity harness for `traffic-rs`. Replays `cases.jsonl` (recorded from the real
//! Go `traffic/` functions by `record.sh`) against the Rust ports.

use serde_json::{Value, json};

use traffic_rs::port::*;

fn cases() -> Vec<Value> {
    include_str!("../cases.jsonl")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON case"))
        .collect()
}

fn arg_str(args: &[Value], i: usize) -> String {
    args[i].as_str().expect("str arg").to_string()
}
fn arg_i64(args: &[Value], i: usize) -> i64 {
    args[i].as_i64().expect("int arg")
}
fn arg_bool(args: &[Value], i: usize) -> bool {
    args[i].as_bool().expect("bool arg")
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "sanitize" => json!(sanitize(&arg_str(args, 0))),
        "ftsQuote" => json!(fts_quote(&arg_str(args, 0))),
        "clip" => json!(clip(&arg_str(args, 0), arg_i64(args, 1))),
        "boolInt" => json!(bool_int(arg_bool(args, 0))),
        "validHeaderName" => json!(valid_header_name(&arg_str(args, 0))),
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
