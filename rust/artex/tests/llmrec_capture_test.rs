//! Differential tests for `llmrec/capture.go` — ports of the five Go capture
//! tests (nil inert, verbatim single attempt, retried attempts, empty).

use std::io::Read;

use artex::llmrec::{
    attempts, capture_from, new_capture, raw_request, raw_response, set_request, tee_or_passthrough,
};

#[test]
fn capture_from_missing() {
    let ctx = artex::llmrec::LlmContext::default();
    assert!(capture_from(&ctx).is_none());
    let (ctx2, cap) = new_capture();
    assert!(Arc::ptr_eq(&capture_from(&ctx2).unwrap(), &cap));
}

#[test]
fn nil_capture_is_inert() {
    set_request(None, b"body");
    assert!(raw_request(None).is_empty());
    assert!(raw_response(None).is_empty());
    assert!(attempts(None).is_empty());
    let mut tee = tee_or_passthrough(None, 200, &b"x"[..]);
    let mut out = Vec::new();
    tee.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"x");
}

#[test]
fn single_attempt_keeps_bytes_verbatim() {
    let (_ctx, cap) = new_capture();
    set_request(Some(&cap), br#"{"model":"x"}"#);
    set_request(Some(&cap), br#"{"model":"ignored"}"#);
    assert_eq!(raw_request(Some(&cap)), br#"{"model":"x"}"#);

    const SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: message_stop\ndata: {}\n\n";
    let mut body = cap.tee_response(200, SSE.as_bytes());
    let mut got = Vec::new();
    body.read_to_end(&mut got).unwrap();
    assert_eq!(got, SSE.as_bytes(), "tee altered the stream");
    assert_eq!(
        raw_response(Some(&cap)),
        SSE.as_bytes(),
        "lone attempt must be verbatim"
    );
    assert_eq!(attempts(Some(&cap)).len(), 1);
    assert_eq!(attempts(Some(&cap))[0].status, 200);
}

#[test]
fn retried_attempts_are_all_kept() {
    let (_ctx, cap) = new_capture();
    let mut first = cap.tee_response(429, &br#"{"error":"rate_limit"}"#[..]);
    let mut sink = Vec::new();
    first.read_to_end(&mut sink).unwrap();
    let mut second = cap.tee_response(200, &b"data: ok\n"[..]);
    let mut sink2 = Vec::new();
    second.read_to_end(&mut sink2).unwrap();

    let raw = String::from_utf8(raw_response(Some(&cap))).unwrap();
    assert!(
        raw.contains(r#"{"error":"rate_limit"}"#),
        "dropped retry: {raw}"
    );
    assert!(raw.contains("data: ok"), "dropped final attempt: {raw}");
    assert!(raw.contains("attempt 1/2 — HTTP 429"), "{raw}");
    assert!(raw.contains("attempt 2/2 — HTTP 200"), "{raw}");
    assert!(
        raw.find("rate_limit").unwrap() < raw.find("data: ok").unwrap(),
        "out of order"
    );
    assert_eq!(attempts(Some(&cap)).len(), 2);
}

#[test]
fn unused_capture_is_empty() {
    let (_ctx, cap) = new_capture();
    assert!(raw_request(Some(&cap)).is_empty());
    assert!(raw_response(Some(&cap)).is_empty());
    assert!(attempts(Some(&cap)).is_empty());
}

use std::sync::Arc;
