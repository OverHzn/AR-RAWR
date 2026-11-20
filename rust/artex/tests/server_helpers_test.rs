//! Differential tests for `server/manager.go` JSON helpers — port of
//! `TestDecodeStrMapJSONNull`.

use artex::server::helpers::{decode_str_map, encode_json};

#[test]
fn decode_str_map_cases() {
    let mut m = decode_str_map(Some(b"null"));
    assert!(m.is_empty());
    m.insert("NODE_EXTRA_CA_CERTS".into(), "/tmp/ca.pem".into());
    assert_eq!(m["NODE_EXTRA_CA_CERTS"], "/tmp/ca.pem");

    assert!(decode_str_map(None).is_empty());
    assert!(decode_str_map(Some(b"")).is_empty());

    let got = decode_str_map(Some(br#"{"FOO":"bar"}"#));
    assert_eq!(got["FOO"], "bar");

    assert!(decode_str_map(Some(br#"["not","an","object"]"#)).is_empty());
}

#[test]
fn encode_json_round_trip() {
    let bytes = encode_json(&serde_json::json!({"a": 1})).unwrap();
    assert_eq!(bytes, br#"{"a":1}"#);
}

#[test]
fn goal_gate_check_cases() {
    use artex::server::helpers::goal_gate_check;

    let good = [
        format!("Audit {} at https://example.com/app every module for injection issues and record one finding per issue with evidence.", "x ".repeat(60)),
        format!("{}the api at target.example.org for unauthenticated data exposure, one record proof, then stop.", "probe ".repeat(25)),
        format!("{}10.0.0.5 and map every listening service before any test.", "recon ".repeat(25)),
        format!("{}localhost:3000 health endpoints respond identically before and after config reload.", "verify ".repeat(25)),
        format!("{}the module at /app/data/core for hardcoded secrets and report each hit.", "review ".repeat(25)),
        format!("[targetless] {}the attached material into a prioritized hypothesis list.", "summarize ".repeat(20)),
        "[targetless] analyze nothing".to_string(),
    ];
    for g in &good {
        assert!(goal_gate_check(g).is_ok(), "unexpected rejection: {g}");
    }

    let bad = [
        "do a thorough security review of everything and report anything interesting that comes up during the exploration phase of this audit, thanks. ".repeat(2),
        "audit the site".to_string(),
        "find bugs".to_string(),
    ];
    for g in &bad {
        assert!(goal_gate_check(g).is_err(), "expected rejection: {g}");
    }
}

#[test]
fn retryable_worker_model_error_cases() {
    use artex::server::helpers::{retryable_worker_model_error, WorkerModelError};
    assert!(!retryable_worker_model_error(
        true,
        &WorkerModelError::noa_budget()
    ));
    assert!(!retryable_worker_model_error(
        true,
        &WorkerModelError::task_llm()
    ));
    assert!(retryable_worker_model_error(
        true,
        &WorkerModelError::Generic
    ));
    assert!(!retryable_worker_model_error(
        false,
        &WorkerModelError::Generic
    ));
}

#[test]
fn decode_body_cap_and_response_helpers() {
    use artex::server::helpers::{
        decode_json_body, is_multipart, write_err, write_json, DecodeError, DECODE_MAX_BYTES,
    };

    let v = decode_json_body(br#"{"x":"ok"}"#, "application/json").unwrap();
    assert_eq!(v["x"], "ok");

    let huge = format!(r#"{{"x":"{}"}}"#, "a".repeat(DECODE_MAX_BYTES));
    assert_eq!(
        decode_json_body(huge.as_bytes(), "application/json").unwrap_err(),
        DecodeError::TooLarge {
            size: huge.len(),
            max: DECODE_MAX_BYTES
        }
    );
    assert!(matches!(
        decode_json_body(b"{bad", "application/json").unwrap_err(),
        DecodeError::InvalidJson(_)
    ));
    assert!(is_multipart("multipart/form-data; boundary=abc"));
    assert!(!is_multipart("application/json"));

    let ok = write_json(200, &serde_json::json!({"x": 1}));
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body, br#"{"x":1}"#);
    let err = write_err(400, "bad request");
    assert_eq!(err.status, 400);
    assert_eq!(err.body, br#"{"error":"bad request"}"#);
}
