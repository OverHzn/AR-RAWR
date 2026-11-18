//! Parity harness for `server-rs`.
//!
//! Replays `cases.jsonl` — recorded from the *real* Go functions by a temporary
//! oracle (`server/zz_oracle_test.go`, deleted after recording) — and asserts the
//! Rust ports produce byte-identical output. This is the verification stand-in
//! for `rust-anything diff` for functions the bridge cannot reach (unexported).

use serde_json::{Value, json};

use server_rs::port::*;

fn cases() -> Vec<Value> {
    let raw = include_str!("../cases.jsonl");
    raw.lines()
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

/// Dispatch one case to the matching port and return the actual result as JSON.
fn dispatch(name: &str, args: &[Value]) -> Value {
    match name {
        // (string) -> string
        "identity" => json!(identity(&arg_str(args, 0))),
        "maskedSecret" => json!(masked_secret(&arg_str(args, 0))),
        "proxyURLString" => json!(proxy_url_string(&arg_str(args, 0))),
        "shellQuote" => json!(shell_quote(&arg_str(args, 0))),
        "normalizeEvidenceText" => json!(normalize_evidence_text(&arg_str(args, 0))),
        "shortQuote" => json!(short_quote(&arg_str(args, 0))),
        "trimTrailingSlash" => json!(trim_trailing_slash(&arg_str(args, 0))),
        "truncateReply" => json!(truncate_reply(&arg_str(args, 0))),
        "truncateBody" => json!(truncate_body(&arg_str(args, 0))),
        "normFilter" => json!(norm_filter(&arg_str(args, 0))),
        "yamlScalar" => json!(yaml_scalar(&arg_str(args, 0))),
        "sanitizeFilename" => json!(sanitize_filename(&arg_str(args, 0))),
        "normalizeConstraintKind" => json!(normalize_constraint_kind(&arg_str(args, 0))),
        "levelOf" => json!(level_of(&arg_str(args, 0))),
        "validatePassword" => json!(validate_password(&arg_str(args, 0))),
        "defaultPort" => json!(default_port(&arg_str(args, 0))),
        "schemeForPort" => json!(scheme_for_port(arg_i64(args, 0))),
        "zipMethodName" => json!(zip_method_name(arg_i64(args, 0) as u16)),

        // (string) -> bool
        "isMaskedSecret" => json!(is_masked_secret(&arg_str(args, 0))),
        "validWorkerMessageRequestID" => json!(valid_worker_message_request_id(&arg_str(args, 0))),
        "inheritedIntentResult" => json!(inherited_intent_result(&arg_str(args, 0))),
        "validSkillName" => json!(valid_skill_name(&arg_str(args, 0))),
        "skillPathRune" => {
            let r = arg_str(args, 0).chars().next().expect("single-char arg");
            json!(skill_path_rune(r))
        }

        // (string) -> (string, bool)
        "canonicalTaskID" => {
            let (c, ok) = canonical_task_id(&arg_str(args, 0));
            json!([c, ok])
        }

        // (string, default) -> int
        "atoiDefault" => json!(atoi_default(&arg_str(args, 0), arg_i64(args, 1))),
        "portOr" => json!(port_or(&arg_str(args, 0), arg_i64(args, 1))),

        // (int64) -> string
        "humanBytes" => json!(human_bytes(arg_i64(args, 0))),
        "i64s" => json!(i64s(arg_i64(args, 0))),

        // (bool) -> string
        "enabledWord" => json!(enabled_word(arg_bool(args, 0))),

        // (string, int64) -> string
        "llmFailKey" => json!(llm_fail_key(&arg_str(args, 0), arg_i64(args, 1))),

        // (string, int) -> string
        "preview" => json!(preview(&arg_str(args, 0), arg_i64(args, 1))),
        "firstLine" => json!(first_line(&arg_str(args, 0), arg_i64(args, 1))),
        "trunc" => json!(trunc(&arg_str(args, 0), arg_i64(args, 1))),

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
        let expected = c.get("ok").or_else(|| c.get("error"));

        if c.get("error").is_some() {
            // No ported helper in this crate is expected to error; record if one does.
            mismatches.push(format!(
                "UNEXPECTED error case {name} args={args:?}: {:?}",
                c["error"]
            ));
            continue;
        }

        let actual = dispatch(name, &args);
        let expected = expected.expect("ok or error");
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
