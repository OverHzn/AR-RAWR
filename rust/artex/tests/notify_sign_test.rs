//! Rust port of the pure `notify/sign_test.go`, `notify/ssrf_test.go`, and
//! `notify/redact_test.go` specs — the signing, SSRF-guard, and credential-redaction
//! substrate in [`artex::notify::{sign, http}`].
//!
//! The dial-guard-through-a-channel and per-channel error-leak cases in those Go
//! files need the concrete channel adapters (a later unit); here we pin the pure,
//! socket-free logic: the two signature algorithms against OpenSSL reference vectors,
//! the IP-range classification table, `validate_http_url`, and the redaction helpers.
//! `allow_local_targets()` reads the process env, so the cases that depend on it set
//! the env var explicitly (tests run in one process; Go used `TestMain`).

use std::net::IpAddr;

use artex::notify::http::{
    allow_local_targets, is_blocked_dial_ip, redact_request_target, redact_transport_error,
    redact_urls_in_text, validate_http_url, TransportError, ALLOW_LOCAL_TARGETS_ENV,
};
use artex::notify::sign::{dingtalk_signed_url, feishu_sign};

// `allow_local_targets` reads the process environment, so the two tests that touch
// it must not run concurrently with each other (Rust runs tests in parallel threads
// within one process; Go isolated this with `TestMain`). Serialize them.
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

// Reference values computed independently with OpenSSL (sign_test.go):
//   TS=1700000000000, SECRET=SECtest123
const SIGN_TEST_TS_MILLIS: i64 = 1_700_000_000_000;
const SIGN_TEST_SECRET: &str = "SECtest123";
const DINGTALK_EXPECTED: &str = "w3RMHXzixTMdzr8OHJUmVLS4IoPJVdu+Ut1LE48MePE=";
const FEISHU_EXPECTED: &str = "Hd4xFWQU6R6ad4nzy4ETIznzlqebqH7xcTFVmONTudo=";

/// `TestDingTalkSignMatchesReference`.
#[test]
fn dingtalk_sign_matches_reference() {
    let got = dingtalk_signed_url(
        "https://oapi.dingtalk.com/robot/send?access_token=tok",
        SIGN_TEST_SECRET,
        SIGN_TEST_TS_MILLIS,
    )
    .expect("signing failed");
    let u = gocompat::neturl::parse_url(&got).expect("produced address should parse");
    let q = query_get(&u.raw_query, "sign");
    assert_eq!(q, DINGTALK_EXPECTED, "signature mismatch");
    assert_eq!(query_get(&u.raw_query, "timestamp"), "1700000000000");
    // The original query parameter must not be overwritten by the signature.
    assert_eq!(query_get(&u.raw_query, "access_token"), "tok");
}

/// `TestFeishuSignMatchesReference`.
#[test]
fn feishu_sign_matches_reference() {
    assert_eq!(
        feishu_sign("1700000000000", SIGN_TEST_SECRET),
        FEISHU_EXPECTED
    );
}

/// `TestSignAlgorithmsDiffer` — the two algorithms must not collapse into one.
#[test]
fn sign_algorithms_differ() {
    let ding = dingtalk_signed_url(
        "https://example.com/hook",
        SIGN_TEST_SECRET,
        SIGN_TEST_TS_MILLIS,
    )
    .unwrap();
    let du = gocompat::neturl::parse_url(&ding).unwrap();
    assert_ne!(
        query_get(&du.raw_query, "sign"),
        feishu_sign("1700000000000", SIGN_TEST_SECRET),
        "DingTalk and Feishu signatures are identical, which means one of the two is wrong"
    );
}

/// `TestDingTalkNoSecretLeavesURLUntouched`.
#[test]
fn dingtalk_no_secret_leaves_url_untouched() {
    let hook = "https://oapi.dingtalk.com/robot/send?access_token=tok";
    let got = dingtalk_signed_url(hook, "", SIGN_TEST_TS_MILLIS).unwrap();
    assert_eq!(
        got, hook,
        "address should not change when no secret is configured"
    );
}

/// `TestValidateHTTPURL` — scheme/host sanity.
#[test]
fn validate_http_url_scheme_and_host() {
    for s in ["https://example.com/hook", "http://10.0.0.1:8080/x?y=1"] {
        // 10.0.0.1 is RFC1918, deliberately allowed; ensure no local-opt-in needed.
        assert!(validate_http_url(s).is_ok(), "{s:?} should be accepted");
    }
    for s in [
        "",
        "file:///etc/passwd",
        "ftp://example.com",
        "https://",
        "gopher://x",
    ] {
        assert!(validate_http_url(s).is_err(), "{s:?} should be rejected");
    }
}

/// `TestIsBlockedDialIP`.
///
/// Go's table also includes `""` (empty string), which `net.ParseIP` turns into a
/// nil `net.IP` and `isBlockedDialIP` blocks via its `if ip == nil` branch. Rust's
/// `IpAddr` cannot be nil, so that defensive case has no analogue here (see the
/// note on [`is_blocked_dial_ip`]); the dial-guard unit covers unparseable hosts.
#[test]
fn is_blocked_dial_ip_classification() {
    let blocked = [
        "127.0.0.1",
        "127.1.2.3",
        "::1",
        "169.254.169.254", // cloud metadata — the main reason this fn exists
        "169.254.1.1",
        "fe80::1",
        "0.0.0.0",
        "::",
        "224.0.0.1",
        "ff02::1",          // link-local multicast
        "ff01::1",          // interface-local multicast
        "::ffff:127.0.0.1", // IPv4-mapped form must convert back before the check
    ];
    for s in blocked {
        let ip: IpAddr = s.parse().unwrap();
        assert!(is_blocked_dial_ip(ip), "{s} should be rejected");
    }
    // RFC1918 private networks are deliberately ALLOWED.
    let allowed = [
        "10.0.0.5",
        "172.16.3.4",
        "192.168.1.10",
        "8.8.8.8",
        "2606:4700::1111",
    ];
    for s in allowed {
        let ip: IpAddr = s.parse().unwrap();
        assert!(
            !is_blocked_dial_ip(ip),
            "{s} should be allowed (private nets are common legitimate targets)"
        );
    }
}

/// `TestValidateHTTPURLRejectsLiteralPrivateTargets` (with local opt-in OFF).
#[test]
fn validate_http_url_rejects_literal_private_targets() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "");
    for raw in [
        "http://127.0.0.1:8080/hook",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]:8080/hook",
    ] {
        assert!(
            validate_http_url(raw).is_err(),
            "{raw} should be rejected at configuration time"
        );
    }
    for raw in [
        "https://oapi.dingtalk.com/robot/send",
        "http://10.0.0.9/hook",
    ] {
        assert!(validate_http_url(raw).is_ok(), "{raw} should pass: err");
    }
}

/// `TestValidateHTTPURLErrorNeverLeaksCredentials` — when `url.Parse` fails, the
/// error text must not carry the credential embedded in the address.
#[test]
fn validate_http_url_error_never_leaks_credentials() {
    const LEAK: &str = "LEAKPROBE0123456789abcdef";
    let cases = [
        format!("http://127.0.0.1/%zz?access_token={LEAK}"), // invalid percent escape
        format!("https://a.example.com:port/x?access_token={LEAK}"), // non-numeric port
        format!("http://[::1?access_token={LEAK}"),          // unbalanced brackets
    ];
    for raw in cases {
        // Confirm the input really fails to parse (else the case covers no branch).
        assert!(
            gocompat::neturl::parse_url(&raw).is_err(),
            "{raw:?} should fail to parse, otherwise this case does not cover the target branch"
        );
        let err = validate_http_url(&raw).expect_err("should fail validation");
        assert!(!err.contains(LEAK), "error leaked the credential: {err}");
    }
}

/// `TestRedactRequestTargetKeepsOnlySchemeAndHost`.
#[test]
fn redact_request_target_keeps_only_scheme_and_host() {
    let cases = [
        (
            "https://oapi.dingtalk.com/robot/send?access_token=S1",
            "https://oapi.dingtalk.com/\u{2026}",
        ),
        (
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=S2",
            "https://qyapi.weixin.qq.com/\u{2026}",
        ),
        (
            "https://open.feishu.cn/open-apis/bot/v2/hook/S3",
            "https://open.feishu.cn/\u{2026}",
        ),
        (
            "https://api.telegram.org/botS4/sendMessage",
            "https://api.telegram.org/\u{2026}",
        ),
        ("http://10.0.0.5:8080/hook", "http://10.0.0.5:8080/\u{2026}"),
    ];
    for (input, want) in cases {
        assert_eq!(redact_request_target(input), want, "redact({input:?})");
    }
    // Unparseable input is never echoed back.
    for bad in ["", "://", "not a url", "http://"] {
        let got = redact_request_target(bad);
        if !bad.is_empty() {
            assert!(
                !got.contains(bad),
                "unparseable input {bad:?} was echoed back as {got:?}"
            );
        }
    }
}

/// `TestRedactTransportErrorStripsURL`.
#[test]
fn redact_transport_error_strips_url() {
    const LEAK: &str = "LEAKPROBE0123456789abcdef";
    let err = TransportError {
        op: "Post".into(),
        url: format!("https://api.telegram.org/bot{LEAK}/sendMessage"),
        cause: "dial tcp 127.0.0.1:1: connect: connection refused".into(),
    };
    let got = redact_transport_error(&err);
    assert!(!got.contains(LEAK), "leaked the credential: {got}");
    assert!(
        got.contains("api.telegram.org"),
        "host should be kept: {got}"
    );
    assert!(
        got.contains("connection refused"),
        "underlying cause should be kept: {got}"
    );
    assert!(got.contains("Post"), "operation name should be kept: {got}");
}

/// `TestRedactURLsInTextHandlesFallback`.
#[test]
fn redact_urls_in_text_handles_fallback() {
    const LEAK: &str = "LEAKPROBE0123456789abcdef";
    let input = format!(
        "refusing cross-host redirect (a.example \u{2192} http://b.example/bot{LEAK}/send)"
    );
    let got = redact_urls_in_text(&input);
    assert!(!got.contains(LEAK), "leaked the credential: {got}");
    assert!(
        got.contains("http://b.example/\u{2026}"),
        "addresses should be redacted: {got}"
    );
    // Text without addresses is kept as-is.
    let plain = "dial tcp: connection refused";
    assert_eq!(redact_urls_in_text(plain), plain);
}

/// Confirm the opt-in flag flips `allow_local_targets` (sanity for the env reader).
#[test]
fn allow_local_targets_env_flag() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    assert!(allow_local_targets());
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "true");
    assert!(allow_local_targets());
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "0");
    assert!(!allow_local_targets());
    std::env::remove_var(ALLOW_LOCAL_TARGETS_ENV);
    assert!(!allow_local_targets());
}

/// Read `key` from a raw query string (test helper, mirrors `url.Values.Get`):
/// the value is percent-decoded, so `sign=w3..%2B..%3D` reads back as the raw
/// base64 `w3..+..=` — the same string Go's `u.Query().Get("sign")` returns.
fn query_get(raw_query: &str, key: &str) -> String {
    for pair in raw_query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let kd = gocompat::neturl::unescape(k, gocompat::neturl::ENC_QUERY_COMPONENT)
            .unwrap_or_else(|_| k.to_string());
        if kd == key {
            return gocompat::neturl::unescape(v, gocompat::neturl::ENC_QUERY_COMPONENT)
                .unwrap_or_else(|_| v.to_string());
        }
    }
    String::new()
}
