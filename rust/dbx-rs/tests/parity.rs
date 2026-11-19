//! Parity harness for `dbx-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `db/` helpers by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use dbx_rs::port::*;

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

fn arg_byte(args: &[Value], i: usize) -> u8 {
    arg_i64(args, i) as u8
}

fn arg_str_slice(args: &[Value], i: usize) -> Vec<String> {
    match &args[i] {
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().expect("string element").to_string())
            .collect(),
        Value::Null => Vec::new(),
        other => panic!("expected array or null, got {other}"),
    }
}

fn arg_i64_slice(args: &[Value], i: usize) -> Vec<i64> {
    match &args[i] {
        Value::Array(a) => a.iter().map(|v| v.as_i64().expect("int element")).collect(),
        Value::Null => Vec::new(),
        other => panic!("expected array or null, got {other}"),
    }
}

fn arg_opt_i64(args: &[Value], i: usize) -> Option<i64> {
    args[i].as_i64()
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "nullIfEmpty" => json!(null_if_empty(&arg_str(args, 0))),
        "isIdentStart" => json!(is_ident_start(arg_byte(args, 0))),
        "isIdentChar" => json!(is_ident_char(arg_byte(args, 0))),
        "marshalStringArray" => json!(marshal_string_array(&arg_str_slice(args, 0))),
        "nullableInt" => json!(nullable_int(arg_i64(args, 0))),
        "truncateRunes" => json!(truncate_runes(&arg_str(args, 0), arg_i64(args, 1))),
        "normalizeKeyword" => json!(normalize_keyword(&arg_str(args, 0))),
        "NormalizeCoverageField" => json!(normalize_coverage_field(&arg_str(args, 0))),
        "normRiskArea" => json!(norm_risk_area(&arg_str(args, 0))),
        "normalizeHeartbeat" => json!(normalize_heartbeat(arg_i64(args, 0))),
        "normalizeRuleAction" => json!(normalize_rule_action(&arg_str(args, 0))),
        "sameOptionalInt64" => json!(same_optional_i64(
            arg_opt_i64(args, 0),
            arg_opt_i64(args, 1)
        )),
        "isUTF8Start" => json!(is_utf8_start(arg_byte(args, 0))),
        "companyNKey" => json!(company_n_key(&arg_str(args, 0))),
        "taskTemplateName" => json!(task_template_name(&arg_str(args, 0))),
        "taskTemplateNKey" => json!(task_template_n_key(&arg_str(args, 0))),
        "lessonExtractCompanyID" => json!(lesson_extract_company_id(&arg_i64_slice(args, 0))),
        "assetKey" => json!(asset_key(arg_i64(args, 0))),
        "companyKey" => json!(company_key(arg_i64(args, 0))),
        "isEncrypted" => json!(is_encrypted(&arg_str(args, 0))),
        "sameMainSegment" => json!(same_main_segment(
            arg_opt_i64(args, 0),
            arg_opt_i64(args, 1)
        )),
        "assetIDContainments" => json!(asset_id_containments(&arg_i64_slice(args, 0))),
        "mergeAgentKeys" => json!(merge_agent_keys(
            &arg_str_slice(args, 0),
            &arg_str_slice(args, 1)
        )),
        "placeholders" => {
            let (ph, ids) = placeholders(arg_i64(args, 0), &arg_i64_slice(args, 1));
            json!([ph, ids])
        }
        "lessonText" => json!(lesson_text(&arg_str(args, 0), arg_i64(args, 1))),
        "truncateNotifyError" => json!(truncate_notify_error(&arg_str(args, 0))),
        "malformedIPAssetWarningText" => {
            json!(malformed_ip_asset_warning_text(
                arg_i64(args, 0),
                &arg_str_slice(args, 1)
            ))
        }
        "isIPLiteral" => json!(is_ip_literal(&arg_str(args, 0))),
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
