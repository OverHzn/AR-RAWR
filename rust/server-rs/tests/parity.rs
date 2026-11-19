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
fn arg_byte(args: &[Value], i: usize) -> u8 {
    arg_i64(args, i) as u8
}

fn arg_opt_str(args: &[Value], i: usize) -> Option<String> {
    args[i].as_str().map(|s| s.to_string())
}
fn arg_opt_i64(args: &[Value], i: usize) -> Option<i64> {
    args[i].as_i64()
}
fn arg_f64(args: &[Value], i: usize) -> f64 {
    args[i].as_f64().expect("float arg")
}

/// Go `json.Marshal` renders an integral float without a decimal point (`1.0` ->
/// `1`). Emit an integer `Value` for integral floats so parity matches byte-for-byte.
fn go_float(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

fn arg_i64_slice(args: &[Value], i: usize) -> Vec<i64> {
    match &args[i] {
        Value::Array(a) => a.iter().map(|v| v.as_i64().expect("int element")).collect(),
        Value::Null => Vec::new(),
        other => panic!("expected array or null, got {other}"),
    }
}

/// Dispatch one case to the matching port. `Ok(value)` is a normal return,
/// `Err(message)` mirrors a Go `error` return (compared against the recorded
/// `error` string).
fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
    let ok = |v: Value| Ok(v);
    match name {
        // (string) -> string
        "identity" => ok(json!(identity(&arg_str(args, 0)))),
        "maskedSecret" => ok(json!(masked_secret(&arg_str(args, 0)))),
        "proxyURLString" => ok(json!(proxy_url_string(&arg_str(args, 0)))),
        "shellQuote" => ok(json!(shell_quote(&arg_str(args, 0)))),
        "normalizeEvidenceText" => ok(json!(normalize_evidence_text(&arg_str(args, 0)))),
        "shortQuote" => ok(json!(short_quote(&arg_str(args, 0)))),
        "trimTrailingSlash" => ok(json!(trim_trailing_slash(&arg_str(args, 0)))),
        "truncateReply" => ok(json!(truncate_reply(&arg_str(args, 0)))),
        "truncateBody" => ok(json!(truncate_body(&arg_str(args, 0)))),
        "normFilter" => ok(json!(norm_filter(&arg_str(args, 0)))),
        "yamlScalar" => ok(json!(yaml_scalar(&arg_str(args, 0)))),
        "sanitizeFilename" => ok(json!(sanitize_filename(&arg_str(args, 0)))),
        "normalizeConstraintKind" => ok(json!(normalize_constraint_kind(&arg_str(args, 0)))),
        "levelOf" => ok(json!(level_of(&arg_str(args, 0)))),
        "validatePassword" => ok(json!(validate_password(&arg_str(args, 0)))),
        "defaultPort" => ok(json!(default_port(&arg_str(args, 0)))),
        "schemeForPort" => ok(json!(scheme_for_port(arg_i64(args, 0)))),
        "zipMethodName" => ok(json!(zip_method_name(arg_i64(args, 0) as u16))),

        // (string) -> bool
        "isMaskedSecret" => ok(json!(is_masked_secret(&arg_str(args, 0)))),
        "validWorkerMessageRequestID" => {
            ok(json!(valid_worker_message_request_id(&arg_str(args, 0,))))
        }
        "inheritedIntentResult" => ok(json!(inherited_intent_result(&arg_str(args, 0)))),
        "validSkillName" => ok(json!(valid_skill_name(&arg_str(args, 0)))),
        "skillPathRune" => {
            let r = arg_str(args, 0).chars().next().expect("single-char arg");
            ok(json!(skill_path_rune(r)))
        }

        // (string) -> (string, bool)
        "canonicalTaskID" => {
            let (c, okk) = canonical_task_id(&arg_str(args, 0));
            ok(json!([c, okk]))
        }

        // (string) -> (int64, error)
        "parseTaskID" => match parse_task_id(&arg_str(args, 0)) {
            Ok(n) => Ok(json!(n)),
            Err(e) => Err(e),
        },

        // (string, default) -> int
        "atoiDefault" => ok(json!(atoi_default(&arg_str(args, 0), arg_i64(args, 1)))),
        "portOr" => ok(json!(port_or(&arg_str(args, 0), arg_i64(args, 1)))),

        // (int64) -> string
        "humanBytes" => ok(json!(human_bytes(arg_i64(args, 0)))),
        "i64s" => ok(json!(i64s(arg_i64(args, 0)))),

        // (bool) -> string
        "enabledWord" => ok(json!(enabled_word(arg_bool(args, 0)))),

        // (string, int64) -> string
        "llmFailKey" => ok(json!(llm_fail_key(&arg_str(args, 0), arg_i64(args, 1)))),

        // (string, int) -> string
        "preview" => ok(json!(preview(&arg_str(args, 0), arg_i64(args, 1)))),
        "firstLine" => ok(json!(first_line(&arg_str(args, 0), arg_i64(args, 1)))),
        "trunc" => ok(json!(trunc(&arg_str(args, 0), arg_i64(args, 1)))),

        // (composite) — batch 9aa
        "coalesceStr" => ok(json!(coalesce_str(
            arg_opt_str(args, 0).as_deref(),
            &arg_str(args, 1)
        ))),
        "stringValue" => ok(json!(string_value(arg_opt_str(args, 0).as_deref()))),
        "cleanStrs" => ok(json!(clean_strs(&arg_str_slice(args, 0)))),
        "contains" => ok(json!(contains(&arg_str_slice(args, 0), &arg_str(args, 1)))),
        "containsFold" => ok(json!(contains_fold(
            &arg_str_slice(args, 0),
            &arg_str(args, 1)
        ))),
        "cloneInt64Ptr" => ok(json!(clone_int64_ptr(arg_opt_i64(args, 0)))),
        "minF" => ok(go_float(min_f(arg_f64(args, 0), arg_f64(args, 1)))),
        "isHostByte" => ok(json!(is_host_byte(arg_byte(args, 0)))),
        "sameOptionalID" => ok(json!(same_optional_id(
            arg_opt_i64(args, 0),
            arg_opt_i64(args, 1)
        ))),
        "keepIfMasked" => ok(json!(keep_if_masked(arg_opt_str(args, 0).as_deref()))),
        "parseVersion" => {
            let v = parse_version(&arg_str(args, 0));
            ok(match v {
                Some(t) => json!(t),
                None => Value::Null,
            })
        }
        "versionLess" => ok(json!(version_less(&arg_str(args, 0), &arg_str(args, 1)))),
        // (composite) — batch 9ab
        "findingPaginationParam" => ok(json!(finding_pagination_param(
            &arg_str(args, 0),
            arg_i64(args, 1),
            arg_i64(args, 2)
        ))),
        "notifyLimitValue" => ok(json!(notify_limit_value(
            &arg_str(args, 0),
            arg_bool(args, 1),
            arg_i64(args, 2),
            arg_i64(args, 3)
        ))),
        "idStrings" => ok(json!(id_strings(&arg_i64_slice(args, 0)))),
        "normalizeFindingIDs" => ok(json!(normalize_finding_ids(
            &arg_i64_slice(args, 0),
            arg_i64(args, 1)
        ))),
        "stripProxyArgs" => ok(json!(strip_proxy_args(&arg_str_slice(args, 0)))),
        "taskContextHeader" => ok(json!(task_context_header(
            arg_i64(args, 0),
            &arg_str(args, 1),
            &arg_str(args, 2)
        ))),
        // batch 9ah
        "csvValues" => ok(json!(csv_values(&arg_str(args, 0)))),
        "skillNameFromFrontmatter" => ok(json!(skill_name_from_frontmatter(&arg_str(args, 0)))),
        "poolLess" => ok(json!(pool_less(
            arg_bool(args, 0),
            arg_i64(args, 1),
            arg_bool(args, 2),
            arg_i64(args, 3)
        ))),
        "normalizeArchiveIDs" => match normalize_archive_ids(&arg_i64_slice(args, 0)) {
            Ok(v) => ok(json!([v, Value::Null])),
            Err(e) => ok(json!([Value::Null, e])),
        },
        "isTerminalStatus" => ok(json!(is_terminal_status(&arg_str(args, 0)))),
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

        match dispatch(name, &args) {
            Ok(actual) => match c.get("ok") {
                Some(expected) if &actual == expected => {}
                Some(expected) => mismatches.push(format!(
                    "{name} args={args:?}\n   expected ok={expected}\n   actual   ok={actual}"
                )),
                None => mismatches.push(format!(
                    "{name} args={args:?}\n   expected error={:?}\n   actual   ok={actual}",
                    c.get("error")
                )),
            },
            Err(actual_err) => match c.get("error").and_then(Value::as_str) {
                Some(expected) if actual_err == expected => {}
                Some(expected) => mismatches.push(format!(
                    "{name} args={args:?}\n   expected error={expected:?}\n   actual   error={actual_err:?}"
                )),
                None => mismatches.push(format!(
                    "{name} args={args:?}\n   expected ok={:?}\n   actual   error={actual_err:?}",
                    c.get("ok")
                )),
            },
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
