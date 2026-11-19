//! Rust port of the channel-adapter specs in `notify/channels_test.go` and the
//! Feishu packing specs in `notify/pack_test.go`, for the four HTTP channels
//! (DingTalk, WeCom, Feishu, Telegram).
//!
//! These drive the real adapters ([`artex::notify::dingtalk`] …) against a scripted
//! local HTTP server ([`common::TestServer`]), so the full path — payload build,
//! `do_json`, business-error classification — is exercised end-to-end. The server is
//! on 127.0.0.1, which the SSRF dial guard denies by default, so every networked test
//! holds [`common::lock_env`] and sets `ARTEX_NOTIFY_ALLOW_LOCAL`.

mod common;

use artex::notify::dingtalk::DingTalkChannel;
use artex::notify::feishu::FeishuChannel;
use artex::notify::http::ALLOW_LOCAL_TARGETS_ENV;
use artex::notify::telegram::TelegramChannel;
use artex::notify::wecom::WeComChannel;
use artex::notify::{Channel, Context, Error, Item, Message};

use common::{Response, TestServer};
use serde_json::{json, Value};

/// `singleMsg`: a single-send message with quotes and newlines (the input that most
/// easily produces invalid JSON from naive template interpolation).
fn single_msg() -> Message {
    Message {
        items: vec![Item {
            finding_id: 42,
            name: r#"login page "SQL injection" risk"#.to_string(),
            vulnclass: "SQL injection".to_string(),
            severity: "high".to_string(),
            summary: "param id\nunfiltered leads to injection".to_string(),
            assets: vec!["a.example.com".to_string(), "b.example.com".to_string()],
            detail_url: "https://artex.local/function/findings/detail?id=42".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// `batchMsg`: a batch digest message of `n` items.
fn batch_msg(n: usize) -> Message {
    let mut m = Message {
        batch: true,
        window_minutes: 30,
        home_url: "https://artex.local/function/findings".to_string(),
        ..Default::default()
    };
    for i in 0..n {
        m.items.push(Item {
            finding_id: (i + 1) as i64,
            name: format!("vuln{}", i + 1),
            vulnclass: "XSS".to_string(),
            severity: "medium".to_string(),
            summary: "reflected cross-site scripting".to_string(),
            assets: vec!["target.example.com".to_string()],
            ..Default::default()
        });
    }
    m
}

/// Start a server that answers every request with `resp_body` (HTTP 200, JSON).
async fn json_server(resp_body: &'static str) -> TestServer {
    TestServer::start(move |_req| Response::json(200, resp_body)).await
}

fn last_body(srv: &TestServer) -> Value {
    let req = srv.records().last().expect("a request was made").clone();
    serde_json::from_slice(&req.body).expect("request body is valid JSON")
}

// === DingTalk ===

#[tokio::test]
async fn dingtalk_sends_action_card_when_link_present() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"errcode":0,"errmsg":"ok"}"#).await;
    let cfg = json!({ "webhook": srv.url });

    DingTalkChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("delivery failed");

    let body = last_body(&srv);
    assert_eq!(body["msgtype"], "actionCard");
    assert_eq!(
        body["actionCard"]["singleURL"],
        "https://artex.local/function/findings/detail?id=42"
    );
}

#[tokio::test]
async fn dingtalk_falls_back_to_markdown_for_batch() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"errcode":0,"errmsg":"ok"}"#).await;
    let cfg = json!({ "webhook": srv.url });

    DingTalkChannel
        .send(&Context::background(), &cfg, &batch_msg(3))
        .await
        .expect("delivery failed");

    let body = last_body(&srv);
    assert_eq!(body["msgtype"], "markdown");
    let text = body["markdown"]["text"].as_str().unwrap();
    assert!(
        text.contains("in the last 30 minutes"),
        "batch body missing the time window: {text}"
    );
}

#[tokio::test]
async fn dingtalk_business_error_is_permanent() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"errcode":310000,"errmsg":"keywords not in content"}"#).await;
    let cfg = json!({ "webhook": srv.url });

    let err = DingTalkChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("errcode != 0 should error");
    assert!(
        err.is_permanent(),
        "keyword mismatch must be permanent: {err}"
    );
    assert!(
        err.message().contains("310000"),
        "error must carry the code: {err}"
    );
}

// === WeCom ===

#[tokio::test]
async fn wecom_truncates_cjk_within_byte_limit() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"errcode":0,"errmsg":"ok"}"#).await;
    let cfg = json!({ "webhook": srv.url });

    // 200 items definitely exceed 4096 bytes.
    WeComChannel
        .send(&Context::background(), &cfg, &batch_msg(200))
        .await
        .expect("delivery failed");

    let body = last_body(&srv);
    let content = body["markdown"]["content"].as_str().unwrap();
    assert!(
        content.len() <= 4096,
        "body {} bytes exceeds the WeCom limit 4096",
        content.len()
    );
    assert!(!content.is_empty(), "body is empty");
    // A `&str` is valid UTF-8 by construction, so a truncated body that stayed valid
    // UTF-8 (Go's `utf8.ValidString`) is guaranteed here too.
}

#[tokio::test]
async fn wecom_rate_limit_is_retryable_but_key_error_is_permanent() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");

    let limited = json_server(r#"{"errcode":45009,"errmsg":"api freq out of limit"}"#).await;
    let cfg = json!({ "webhook": limited.url });
    let err = WeComChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("45009 should error");
    assert!(!err.is_permanent(), "45009 must be retryable: {err}");

    let bad_key = json_server(r#"{"errcode":93000,"errmsg":"invalid webhook url"}"#).await;
    let cfg = json!({ "webhook": bad_key.url });
    let err = WeComChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("93000 should error");
    assert!(err.is_permanent(), "93000 must be permanent: {err}");
}

// === Feishu ===

#[tokio::test]
async fn feishu_card_structure_and_sign() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"code":0,"msg":"success"}"#).await;
    let cfg = json!({ "webhook": srv.url, "secret": "SECtest123" });

    FeishuChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("delivery failed");

    let body = last_body(&srv);
    assert_eq!(body["msg_type"], "interactive");
    assert_eq!(body["card"]["header"]["template"], "orange");
    // With a secret configured, signing params must be present (else Feishu 19021).
    assert!(!body["sign"].is_null(), "missing sign");
    assert!(!body["timestamp"].is_null(), "missing timestamp");

    // A button whose url points at the finding details must be present.
    let mut found = false;
    for el in body["card"]["elements"].as_array().unwrap() {
        if el["tag"] != "action" {
            continue;
        }
        for a in el["actions"].as_array().into_iter().flatten() {
            if a["url"] == "https://artex.local/function/findings/detail?id=42" {
                found = true;
            }
        }
    }
    assert!(found, "no button pointing to the detail page in the card");
}

#[tokio::test]
async fn feishu_without_secret_omits_sign() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"code":0,"msg":"success"}"#).await;
    let cfg = json!({ "webhook": srv.url });

    FeishuChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("delivery failed");

    let body = last_body(&srv);
    assert!(
        body["sign"].is_null(),
        "sign must be absent without a secret"
    );
    assert!(
        body["timestamp"].is_null(),
        "timestamp must be absent without a secret"
    );
}

#[tokio::test]
async fn feishu_packing_reports_kept() {
    // `TestFeishuPackingReportsKept`: a huge digest fits only a part.
    let m = batch_msg(2000);
    let (_card, kept) = artex::notify::feishu::feishu_card(&m);
    assert!(
        kept > 0 && kept < m.items.len(),
        "card should fit only a part, got {kept}"
    );
}

#[tokio::test]
async fn feishu_digest_bodies_stay_within_card_limit() {
    // `TestFeishuDigestBodiesStayWithinCardLimit`: the body on the wire must stay
    // under the limit, the envelope constant must hold, and across the messages every
    // item is delivered exactly once and in order.
    const TOTAL: usize = 500;
    const FEISHU_MAX_CARD_BYTES: usize = 16000;
    const FEISHU_ENVELOPE_BYTES: usize = 113;

    // Three shapes: short lines without assets, with one asset, and titles that grow
    // when JSON-serialized (`<>&"`).
    let mut short = batch_msg(TOTAL);
    for it in &mut short.items {
        it.assets.clear();
    }
    let with_asset = batch_msg(TOTAL);
    let mut escaped = batch_msg(TOTAL);
    for (i, it) in escaped.items.iter_mut().enumerate() {
        it.name = format!("<a href=\"x\">R&D</a> vuln{}", i + 1);
    }

    for (name, start) in [
        ("short lines without assets", short),
        ("short lines with one asset", with_asset),
        ("lines that grow when serialized", escaped),
    ] {
        let _g = common::lock_env().await;
        std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
        let srv = json_server(r#"{"code":0,"msg":"success"}"#).await;
        let cfg = json!({ "webhook": srv.url, "secret": "SECtest123" });

        let mut m = start;
        let mut seen = 0usize;
        let mut messages = 0usize;
        while !m.items.is_empty() {
            let before = srv.records().len();
            let kept = FeishuChannel
                .send(&Context::background(), &cfg, &m)
                .await
                .unwrap_or_else(|e| panic!("{name}: message {} failed: {e}", messages + 1));
            messages += 1;
            // The body actually put on the wire for this send.
            let raw = srv.records()[before..]
                .last()
                .expect("a request was made")
                .body
                .clone();
            assert!(
                raw.len() <= FEISHU_MAX_CARD_BYTES,
                "{name}: message {messages} body of {} bytes exceeds {FEISHU_MAX_CARD_BYTES} \
                 ({} of {TOTAL} items left)",
                raw.len(),
                m.items.len()
            );

            let body: Value = serde_json::from_slice(&raw).expect("body is valid JSON");
            // The card's raw bytes as embedded in the body.
            let card_raw_len = serde_json::to_vec(&body["card"]).unwrap().len();
            assert_eq!(
                raw.len() - card_raw_len,
                FEISHU_ENVELOPE_BYTES,
                "{name}: message {messages}: {} bytes around the card, envelope says {}",
                raw.len() - card_raw_len,
                FEISHU_ENVELOPE_BYTES
            );

            // Count the div lines after the intro (element 0); a button is not a div.
            let elements = body["card"]["elements"].as_array().unwrap();
            let mut lines = 0usize;
            for el in elements.iter().skip(1) {
                if el["tag"] != "div" {
                    continue;
                }
                lines += 1;
                // Each line ends its title with the closing bold marker, so the
                // title text "vuln{N}" is immediately followed by "**".
                let want = format!("vuln{}**", seen + lines);
                let content = el["text"]["content"].as_str().unwrap();
                assert!(
                    content.contains(&want),
                    "{name}: message {messages} line {lines} should be item {} ({want:?}) got {content:?}",
                    seen + lines
                );
            }
            assert!(
                kept > 0 && kept == lines,
                "{name}: message {messages}: {kept} items reported delivered, body holds {lines}"
            );

            seen += kept;
            m.items = m.items[kept..].to_vec();
        }
        assert_eq!(
            seen, TOTAL,
            "{name}: {seen} items delivered across {messages} messages"
        );
        assert!(
            messages >= 2,
            "{name}: {TOTAL} items must not fit one message"
        );
    }
}

// === Telegram ===

#[tokio::test]
async fn telegram_escapes_html_in_untrusted_content() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"ok":true}"#).await;
    let cfg = json!({ "bot_token": "tok", "chat_id": "1", "base_url": srv.url });

    let m = Message {
        items: vec![Item {
            severity: "high".to_string(),
            name: "<script>alert(1)</script>".to_string(),
            summary: "a & b < c".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    TelegramChannel
        .send(&Context::background(), &cfg, &m)
        .await
        .expect("delivery failed");

    let body = last_body(&srv);
    assert_eq!(body["parse_mode"], "HTML");
    let text = body["text"].as_str().unwrap();
    assert!(
        !text.contains("<script>"),
        "HTML not escaped, injection possible: {text}"
    );
    assert!(
        text.contains("&lt;script&gt;"),
        "expected escaped entities: {text}"
    );
    assert!(text.contains("a &amp; b"), "& not escaped: {text}");
}

#[tokio::test]
async fn telegram_error_classification() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");

    let rate_limited =
        json_server(r#"{"ok":false,"error_code":429,"description":"Too Many Requests"}"#).await;
    let cfg = json!({ "bot_token": "tok", "chat_id": "1", "base_url": rate_limited.url });
    let err = TelegramChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("429 should error");
    assert!(!err.is_permanent(), "429 should be retryable: {err}");

    let forbidden =
        json_server(r#"{"ok":false,"error_code":403,"description":"bot was blocked by the user"}"#)
            .await;
    let cfg = json!({ "bot_token": "tok", "chat_id": "1", "base_url": forbidden.url });
    let err = TelegramChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("403 should error");
    assert!(err.is_permanent(), "403 should be permanent: {err}");
}

// === Validation messages (the four registered HTTP channels) ===

#[test]
fn channel_validate_reports_missing_fields() {
    // `TestChannelValidateReportsMissingFields` (the four channels registered so far;
    // email joins in a later unit). Each error must name what is missing.
    let cases: Vec<(Box<dyn Channel>, Value, &str)> = vec![
        (Box::new(DingTalkChannel), json!({}), "Webhook"),
        (Box::new(FeishuChannel), json!({}), "Webhook"),
        (Box::new(WeComChannel), json!({}), "Webhook"),
        (Box::new(TelegramChannel), json!({}), "Bot Token"),
        (
            Box::new(TelegramChannel),
            json!({ "bot_token": "t" }),
            "Chat ID",
        ),
    ];
    for (ch, cfg, substr) in cases {
        let err = ch
            .validate(&cfg)
            .expect_err("config should fail validation");
        let msg = match err {
            Error::Msg(m) | Error::Permanent(m) | Error::Transient(m) => m,
        };
        assert!(
            msg.contains(substr),
            "{} config error should mention {substr:?}, got {msg:?}",
            ch.kind()
        );
    }
}
