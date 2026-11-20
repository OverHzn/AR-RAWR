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
