//! Parity harness for `enrich-rs`. Replays `cases.jsonl` (recorded from the real
//! Go `enrich/` functions by `record.sh`) against the Rust ports.

use serde_json::{Value, json};

use enrich_rs::port::*;

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
fn arg_str(args: &[Value], i: usize) -> String {
    args[i].as_str().expect("str arg").to_string()
}
fn arg_str_slice(args: &[Value], i: usize) -> Vec<String> {
    args[i]
        .as_array()
        .expect("array arg")
        .iter()
        .map(|v| v.as_str().expect("str in array").to_string())
        .collect()
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "id0" => json!(id0(arg_i64(args, 0))),
        "itoa" => json!(itoa(arg_i64(args, 0))),
        "hostOf" => json!(host_of(&arg_str(args, 0))),
        "uniq" => json!(uniq(&arg_str_slice(args, 0))),
        "isBlank" => json!(is_blank(&args[0])),
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
