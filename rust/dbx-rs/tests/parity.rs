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

fn arg_bytes(args: &[Value], i: usize) -> Vec<u8> {
    match &args[i] {
        Value::Null => Vec::new(),
        Value::String(s) => gocompat::encoding::base64_std_decode(s).expect("valid base64"),
        other => panic!("expected base64 string or null, got {other}"),
    }
}

fn b64(b: &[u8]) -> String {
    gocompat::encoding::base64_std_encode(b)
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
        "isModelReason" => json!(is_model_reason(&arg_str(args, 0))),
        "inheritedIntentTerminal" => json!(inherited_intent_terminal(&arg_str(args, 0))),
        "assetDisplayName" => json!(asset_display_name(
            &arg_str(args, 0),
            &arg_str(args, 1),
            &arg_str(args, 2),
            &arg_str(args, 3),
            &arg_str(args, 4),
            &arg_str(args, 5),
        )),
        "isKnownField" => json!(is_known_field(&arg_str(args, 0))),
        "sealedJSON" => {
            let (s, ok) = sealed_json(&arg_str(args, 0));
            json!([s, ok])
        }
        "looksLikeIPAddress" => json!(looks_like_ip_address(&arg_str(args, 0))),
        // batch 9ak
        "NormalizeTaskCompanyIDs" => match normalize_task_company_ids(&arg_i64_slice(args, 0)) {
            Ok(Some(v)) => json!([v, Value::Null]),
            Ok(None) => json!([Value::Null, Value::Null]),
            Err(e) => json!([Value::Null, e]),
        },
        "normalizeTaskAssetIDs" => match normalize_task_asset_ids(&arg_i64_slice(args, 0)) {
            Ok(v) => json!([v, Value::Null]),
            Err(e) => json!([Value::Null, e]),
        },
        "normalizeTaskCategoryName" => match normalize_task_category_name(&arg_str(args, 0)) {
            Ok((a, b)) => json!([[a, b], Value::Null]),
            Err(e) => json!([["", ""], e]),
        },
        "normalizeTaskAssetSource" => {
            match normalize_task_asset_source(&arg_str(args, 0), &arg_str(args, 1)) {
                Ok((a, b)) => json!([[a, b], Value::Null]),
                Err(e) => json!([["", ""], e]),
            }
        }
        // batch 9al
        "utf8Clean" => json!(b64(&utf8_clean(&arg_bytes(args, 0)))),
        "jsonbClean" => json!(b64(&jsonb_clean(&arg_bytes(args, 0)))),
        "readToken" => {
            let (tok, np) = read_token(&arg_str(args, 0), arg_i64(args, 1) as usize);
            json!([tok, np])
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
