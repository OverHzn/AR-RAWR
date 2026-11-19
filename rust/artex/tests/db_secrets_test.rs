//! Parity test for the secret-box crypto core — port of `db/secrets_test.go`'s
//! `TestSecretBoxCrypto` (the DB-free part). Proves AES-256-GCM seal/open with
//! AAD binding, tamper detection, distinct nonces, empty-stays-empty, and the
//! canonical-JSON envelope for JSON columns.

use artex::db::secrets::{
    is_encrypted, is_sealed, open_secret, parse_secret_key, seal_secret, sealed_json, SecretBox,
    COL_MCP_ENV, SECRET_PREFIX,
};

/// `testSecretKey` — a fixed 32-byte key (`"0123456789abcdef0123456789abcdef"`).
fn test_secret_key() -> Vec<u8> {
    b"0123456789abcdef0123456789abcdef".to_vec()
}

fn b64_decode(s: &str) -> Vec<u8> {
    gocompat::encoding::base64_std_decode(s).expect("valid base64")
}

fn b64_encode(b: &[u8]) -> String {
    gocompat::encoding::base64_std_encode(b)
}

#[test]
fn secret_box_crypto() {
    let b = SecretBox::new(&test_secret_key()).expect("key must be 32 bytes");

    // encrypt → distinct ciphertexts (fresh nonce each call), no plaintext leak.
    let c1 = b.encrypt("t.c", "hunter2").expect("encrypt");
    let c2 = b.encrypt("t.c", "hunter2").expect("encrypt");
    assert!(is_encrypted(&c1), "ciphertext must carry enc:v1: prefix");
    assert!(
        !is_encrypted("hunter2"),
        "plaintext must not look encrypted"
    );
    assert_ne!(c1, c2, "distinct nonces → distinct ciphertexts");
    assert!(
        !c1.contains("hunter2"),
        "ciphertext must not contain plaintext"
    );

    // round trip.
    assert_eq!(b.decrypt("t.c", &c1).expect("decrypt"), "hunter2");

    // wrong AAD must fail (the AAD is authenticated).
    assert!(b.decrypt("t.other", &c1).is_err(), "wrong AAD must fail");

    // tampered ciphertext must fail (flip the last byte of the raw blob).
    let mut raw = b64_decode(c1.strip_prefix(SECRET_PREFIX).unwrap_or(&c1));
    let last = raw.len() - 1;
    raw[last] ^= 1;
    let tampered = format!("{SECRET_PREFIX}{}", b64_encode(&raw));
    assert!(
        b.decrypt("t.c", &tampered).is_err(),
        "tampered ciphertext must fail"
    );

    // empty stays empty.
    assert_eq!(b.encrypt("t.c", "").expect("encrypt empty"), "");

    // JSON column: sealed envelope hides the plaintext and round-trips to
    // canonical JSON (keys sorted).
    let sealed = b
        .seal(&COL_MCP_ENV, r#"{"b":1, "a":"tok"}"#)
        .expect("seal json");
    assert!(is_sealed(&COL_MCP_ENV, &sealed), "json must be sealed");
    assert!(
        !sealed.contains("tok"),
        "sealed json must not leak the value"
    );
    assert_eq!(
        b.open(&COL_MCP_ENV, &sealed).expect("open json"),
        r#"{"a":"tok","b":1}"#,
        "canonical JSON round trip (keys sorted)"
    );

    // reserved envelope key is refused on seal.
    assert!(
        b.seal(&COL_MCP_ENV, r#"{"__artex_enc":"x"}"#).is_err(),
        "reserved key must be refused"
    );

    // empty object stays empty.
    assert_eq!(b.seal(&COL_MCP_ENV, "{}").expect("seal empty obj"), "{}");
}

/// `sealedJSON` only recognises a lone `{"__artex_enc":"enc:v1:..."}` envelope.
#[test]
fn sealed_json_recognises_only_lone_envelope() {
    assert!(sealed_json(r#"{"__artex_enc":"enc:v1:AAAA"}"#).is_some());
    assert!(
        sealed_json(r#"{"__artex_enc":"plain"}"#).is_none(),
        "not enc:v1:"
    );
    assert!(
        sealed_json(r#"{"__artex_enc":"enc:v1:A","x":1}"#).is_none(),
        "extra key"
    );
    assert!(sealed_json(r#"{"other":1}"#).is_none(), "wrong key");
    assert!(sealed_json("not json").is_none());
}

/// `parseSecretKey` accepts a 32-byte key encoded as base64 or hex.
#[test]
fn parse_secret_key_accepts_base64_and_hex() {
    let raw = test_secret_key();
    let b64 = b64_encode(&raw);
    assert_eq!(parse_secret_key(&b64).expect("base64"), raw);

    let hex = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(parse_secret_key(&hex).expect("hex"), raw);

    assert!(parse_secret_key("too-short").is_err());
    assert!(parse_secret_key("").is_err());
}

/// `sealSecret`/`openSecret` require an active box; before `InitSecrets` they
/// report the "key not initialized" error for a sealed value and pass plaintext
/// through. (We do not install the global box here — that is `InitSecrets`, which
/// needs the DB; this proves the pre-init contract.)
#[test]
fn open_secret_passes_plaintext_through_pre_init() {
    // A plaintext (unsealed) value is a legacy row and passes through unchanged,
    // even before a key is installed.
    assert_eq!(
        open_secret(&COL_MCP_ENV, r#"{"a":1}"#).expect("legacy passthrough"),
        r#"{"a":1}"#
    );
    assert_eq!(
        open_secret(&artex::db::secrets::COL_LLM_KEY, "sk-plain").expect("legacy passthrough"),
        "sk-plain"
    );
    // seal_secret with no active box is an error.
    assert!(seal_secret(&COL_MCP_ENV, r#"{"a":1}"#).is_err());
}
