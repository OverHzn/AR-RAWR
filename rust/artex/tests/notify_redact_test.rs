//! Port of the credential-leak regression tests in `notify/redact_test.go`.
//!
//! Error text is shown to operators (and returned by the delivery-history API),
//! so it must never carry a credential. Every channel that embeds a secret in a
//! URL (query, path segment, or signing secret) is driven at a doomed address and
//! at a malformed one; in both cases the send must fail and the secret sentinel
//! must not appear anywhere in the error.

mod common;

use artex::notify::{channel, Context, Item, Message};
use serde_json::{json, Value};

use common::lock_env;

/// A sentinel that can never be a real credential, searched for in error text.
const LEAK_PROBE_TOKEN: &str = "LEAKPROBE0123456789abcdef";

fn probe_msg() -> Message {
    Message {
        items: vec![Item {
            finding_id: 1,
            severity: "high".to_string(),
            name: "leak probe".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn send_err(kind: &str, cfg: Value) -> String {
    channel(kind)
        .unwrap_or_else(|| panic!("channel {kind} not registered"))
        .send(&Context::background(), &cfg, &probe_msg())
        .await
        .expect_err("the send should error")
        .message()
}

fn assert_no_secret(text: &str, secret: &str) {
    assert!(
        !text.contains(secret),
        "error text leaked the credential {secret:?}:\n    {text}"
    );
}

#[tokio::test]
async fn channel_errors_never_leak_credentials() {
    // A doomed peer: 127.0.0.1 is loopback, so the SSRF dial guard blocks before
    // any connection is made; the refusal text is already redacted.
    let _g = lock_env().await;
    std::env::remove_var(artex::notify::http::ALLOW_LOCAL_TARGETS_ENV);

    let t = LEAK_PROBE_TOKEN;
    let cases: Vec<(&str, Value)> = vec![
        (
            "dingtalk",
            json!({"webhook": format!("http://127.0.0.1:1/robot/send?access_token={t}")}),
        ), // access_token in query
        (
            "wecom",
            json!({"webhook": format!("http://127.0.0.1:1/cgi-bin/webhook/send?key={t}")}),
        ), // key in query
        (
            "feishu",
            json!({"webhook": format!("http://127.0.0.1:1/open-apis/bot/v2/hook/{t}")}),
        ), // hook id in last path segment
        (
            "telegram",
            json!({"bot_token": t, "chat_id": "1", "base_url": "http://127.0.0.1:1"}),
        ), // bot token mid-path
        (
            "dingtalk",
            json!({"webhook": "http://127.0.0.1:1/robot/send", "secret": t}),
        ), // signing secret
    ];

    for (kind, cfg) in cases {
        let err = send_err(kind, cfg).await;
        assert_no_secret(&err, LEAK_PROBE_TOKEN);
    }
}

#[tokio::test]
async fn channel_errors_never_leak_credentials_in_permanent_path() {
    // Malformed addresses exercise the URL-validation / parse branch, which also
    // sends error text outward and must not carry the credential either.
    let _g = lock_env().await;
    std::env::remove_var(artex::notify::http::ALLOW_LOCAL_TARGETS_ENV);

    let t = LEAK_PROBE_TOKEN;
    let cases: Vec<(&str, Value)> = vec![
        ("dingtalk", json!({"webhook": format!("file:///{t}")})),
        ("wecom", json!({"webhook": format!("gopher://{t}")})),
        ("feishu", json!({"webhook": format!("ftp://{t}/hook")})),
        (
            "telegram",
            json!({"bot_token": "tok", "chat_id": "1", "base_url": format!("file://{t}")}),
        ),
        ("webhook", json!({"url": format!("javascript:{t}")})),
    ];

    for (kind, cfg) in cases {
        let err = send_err(kind, cfg).await;
        assert_no_secret(&err, LEAK_PROBE_TOKEN);
    }
}
