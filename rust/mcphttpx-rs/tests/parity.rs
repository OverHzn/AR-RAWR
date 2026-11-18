//! Parity harness for `mcphttpx-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `mcphttp/` helpers by `record.sh`) against the Rust port.
//!
//! Each case is either `{fn, args, ok}` (success) or `{fn, args, error}` (the Go
//! call returned a non-nil error). `dispatch` returns `Result<Value, String>` so the
//! two shapes line up with `normalize_headers`'s `Result` and with the always-`Ok`
//! `is_unsupported_version`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use mcphttpx_rs::port::*;

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

fn arg_str_map(args: &[Value], i: usize) -> BTreeMap<String, String> {
    match &args[i] {
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().expect("string value").to_string()))
            .collect(),
        other => panic!("expected object arg, got {other}"),
    }
}

fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "normalizeHeaders" => normalize_headers(&arg_str_map(args, 0)).map(|m| json!(m)),
        "isUnsupportedVersion" => Ok(json!(is_unsupported_version(&arg_str(args, 0)))),
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
        let expected_err = c.get("error").and_then(Value::as_str);
        let actual = dispatch(name, &args);
        match (expected_err, actual) {
            (Some(exp), Err(act)) => {
                if exp != act {
                    mismatches.push(format!(
                        "{name} args={args:?}\n   expected error={exp:?}\n   actual   error={act:?}"
                    ));
                }
            }
            (None, Ok(act)) => {
                let expected = &c["ok"];
                if &act != expected {
                    mismatches.push(format!(
                        "{name} args={args:?}\n   expected={expected}\n   actual  ={act}"
                    ));
                }
            }
            (Some(exp), Ok(act)) => mismatches.push(format!(
                "{name} args={args:?}\n   expected error={exp:?} but got Ok({act})"
            )),
            (None, Err(act)) => mismatches.push(format!(
                "{name} args={args:?}\n   expected Ok but got error={act:?}"
            )),
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
