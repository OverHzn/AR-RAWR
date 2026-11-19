//! Parity harness for `notifyx-rs`. Replays `cases.jsonl` (recorded from the
//! real Go `notify/` config helpers by `record.sh`) against the Rust port.

use serde_json::{Value, json};

use notifyx_rs::port::*;

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
        // Go nil slice records as JSON null; treat as empty.
        Value::Null => Vec::new(),
        Value::Array(arr) => arr
            .iter()
            .map(|v| v.as_str().expect("string in slice").to_string())
            .collect(),
        _ => panic!("string slice arg"),
    }
}

fn arg_i64_slice(args: &[Value], i: usize) -> Vec<i64> {
    match &args[i] {
        Value::Null => Vec::new(),
        Value::Array(arr) => arr
            .iter()
            .map(|v| v.as_i64().expect("int in slice"))
            .collect(),
        _ => panic!("int slice arg"),
    }
}

fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        "cfgString" => json!(cfg_string(&args[0], &arg_str(args, 1))),
        "cfgInt" => json!(cfg_int(&args[0], &arg_str(args, 1))),
        "cfgBool" => json!(cfg_bool(&args[0], &arg_str(args, 1))),
        "cfgStrings" => json!(cfg_strings(&args[0], &arg_str(args, 1))),
        "cfgMap" => json!(cfg_map(&args[0], &arg_str(args, 1))),
        "ValidKind" => json!(valid_kind(&arg_str(args, 0))),
        "Kinds" => json!(kinds()),
        "htmlEscape" => json!(html_escape(&arg_str(args, 0))),
        "htmlEscapeAttr" => json!(html_escape_attr(&arg_str(args, 0))),
        "telegramEscape" => json!(telegram_escape(&arg_str(args, 0))),
        "telegramEscapeAttr" => json!(telegram_escape_attr(&arg_str(args, 0))),
        "markdownEscape" => json!(markdown_escape(&arg_str(args, 0))),
        "htmlSeverityColor" => json!(html_severity_color(&arg_str(args, 0))),
        "containsAnyFold" => json!(contains_any_fold(
            &arg_str(args, 0),
            &arg_str_slice(args, 1)
        )),
        "intersectsInt" => json!(intersects_int(
            &arg_i64_slice(args, 0),
            &arg_i64_slice(args, 1)
        )),
        "assetLine" => json!(asset_line(&arg_str_slice(args, 0), arg_i64(args, 1))),
        "byteSize" => json!(byte_size(&arg_str(args, 0))),
        "runeSize" => json!(rune_size(&arg_str(args, 0))),
        "TruncateRunes" => json!(truncate_runes(&arg_str(args, 0), arg_i64(args, 1))),
        "OneLine" => json!(one_line(&arg_str(args, 0), arg_i64(args, 1))),
        "markdownText" => json!(markdown_text(&arg_str(args, 0), arg_i64(args, 1))),
        "snippet" => json!(snippet(&arg_str(args, 0))),
        "isBlankConfigValue" => json!(is_blank_config_value(&args[0])),
        "sameConfigValue" => json!(same_config_value(&args[0], &args[1])),
        "smtpReplyCode" => json!(smtp_reply_code(&arg_str(args, 0))),
        "feishuSeverityTemplate" => json!(feishu_severity_template(&arg_str(args, 0))),
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
