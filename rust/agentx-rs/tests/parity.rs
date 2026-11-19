//! Parity harness for `agentx-rs`. Replays `cases.jsonl` (recorded from the real
//! Go `agent/` helpers by `record.sh`) against the Rust port.
//!
//! Each case is either `{fn, args, ok}` (success) or `{fn, args, error}` (the Go
//! call returned a non-nil error). `dispatch` returns `Result<Value, String>` so
//! the two shapes line up with `normalize_fact_source`'s `Result` and with the
//! always-`Ok` helpers.

use serde_json::{Value, json};

use agentx_rs::port::*;

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

fn arg_str_slice(args: &[Value], i: usize) -> Vec<String> {
    match &args[i] {
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().expect("string element").to_string())
            .collect(),
        other => panic!("expected array arg, got {other}"),
    }
}

fn arg_bool(args: &[Value], i: usize) -> bool {
    args[i].as_bool().expect("bool arg")
}

fn arg_usize(args: &[Value], i: usize) -> usize {
    let n = arg_i64(args, i);
    assert!(n >= 0, "expected non-negative count arg, got {n}");
    n as usize
}

fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "budgetCap" => Ok(json!(budget_cap(arg_i64(args, 0)))),
        "left" => Ok(json!(left(arg_i64(args, 0), arg_i64(args, 1)))),
        "relatedOverviewBudgetForSources" => {
            Ok(json!(related_overview_budget_for_sources(arg_i64(args, 0))))
        }
        "foldableKind" => Ok(json!(foldable_kind(&arg_str(args, 0)))),
        "isFalsy" => Ok(json!(is_falsy(&arg_str(args, 0)))),
        "firstLine" => Ok(json!(first_line(&arg_str(args, 0), arg_usize(args, 1)))),
        "truncOutput" => Ok(json!(trunc_output(&arg_str(args, 0), arg_usize(args, 1)))),
        "workerChatMarker" => Ok(json!(worker_chat_marker(&arg_str(args, 0)))),
        "normalizeDirection" => Ok(json!(normalize_direction(&arg_str(args, 0)))),
        "hintAuthor" => Ok(json!(hint_author(&arg_str(args, 0)))),
        "normalizeFactSource" => normalize_fact_source(&arg_str(args, 0)).map(|s| json!(s)),
        "splitLines" => Ok(json!(split_lines(&arg_str(args, 0)))),
        "isEmptyJSON" => Ok(json!(is_empty_json(&arg_str(args, 0)))),
        "pid" => Ok(json!(pid(&arg_str(args, 0)))),
        "pidList" => Ok(json!(pid_list(&arg_str_slice(args, 0)))),
        "bbotQuote" => Ok(json!(bbot_quote(&arg_str(args, 0), &arg_str(args, 1)))),
        "clipBody" => Ok(json!(clip_body(&arg_str(args, 0)))),
        "nmapWebScheme" => Ok(json!(nmap_web_scheme(&arg_str(args, 0), &arg_str(args, 1)))),
        // (string) — batch 9ac
        "workerTrafficBlock" => Ok(json!(worker_traffic_block(arg_bool(args, 0)))),
        "untrustedContentTail" => Ok(json!(untrusted_content_tail(&arg_str(args, 0)))),
        "normalizeOllamaBaseURL" => Ok(json!(normalize_ollama_base_url(&arg_str(args, 0)))),
        "stripUserinfo" => Ok(json!(strip_userinfo(&arg_str(args, 0)))),
        "bbotToken" => Ok(json!(bbot_token(&arg_str(args, 0)))),
        "artifactSpec" => Ok(json!(artifact_spec(&arg_str(args, 0)))),
        "workerArtifactSpec" => Ok(json!(worker_artifact_spec(&arg_str(args, 0)))),
        "chatWorkDirSpec" => Ok(json!(chat_work_dir_spec(&arg_str(args, 0)))),
        // batch 9ag
        "inheritedIntentSummaryState" => {
            Ok(json!(inherited_intent_summary_state(&arg_str(args, 0))))
        }
        "terminalReasonLabel" => Ok(json!(terminal_reason_label(&arg_str(args, 0)))),
        "terminalReasonHint" => Ok(json!(terminal_reason_hint(&arg_str(args, 0)))),
        "plannerGoalAnchor" => Ok(json!(planner_goal_anchor(
            &arg_str(args, 0),
            &arg_str(args, 1)
        ))),
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
