//! Rust port of the generic-webhook specs: the webhook cases in
//! `notify/channels_test.go` and the capability-boundary specs in
//! `notify/webhook_template_test.go`.
//!
//! The first two tests drive the real [`WebhookChannel`] against a scripted local
//! server (so payload build + `do_json` run end-to-end); the rest pin the template
//! engine's capability boundary (unknown fields/functions error rather than echo,
//! the context is pure data, and a render result that is not valid JSON is a
//! permanent failure). Tests that actually open a socket hold [`common::lock_env`]
//! and set `ARTEX_NOTIFY_ALLOW_LOCAL`, because the server is on 127.0.0.1.

mod common;

use artex::notify::webhook::{
    parse_webhook_template, render_webhook_body, webhook_template_data, WebhookChannel,
};
use artex::notify::{Channel, Context, Error, Item, Message};

use common::{Response, TestServer};
use serde_json::{json, Value};

/// `singleMsg`: a single-send message with quotes and newlines — the input that
/// most easily produces invalid JSON from naive template interpolation.
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

async fn json_server(resp_body: &'static str) -> TestServer {
    TestServer::start(move |_req| Response::json(200, resp_body)).await
}

// === channels_test.go webhook cases ===

#[tokio::test]
async fn webhook_default_template_produces_valid_json() {
    // The whole point of the default template: a title with quotes/newlines would
    // break a naive `"title": "{{.Title}}"`; `{{json .Title}}` keeps it valid.
    let _g = common::lock_env().await;
    std::env::set_var(artex::notify::http::ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"ok":true}"#).await;
    let cfg = json!({ "url": srv.url });

    WebhookChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("delivery failed");

    let req = srv.records().last().expect("a request was made").clone();
    let body: Value = serde_json::from_slice(&req.body).expect("request body is valid JSON");

    assert_eq!(
        body["title"], r#"[🟠 High] login page "SQL injection" risk"#,
        "title not restored correctly: {}",
        body["title"]
    );
    let items = body["items"].as_array().expect("items is an array");
    assert_eq!(items.len(), 1, "items should have 1 entry");
    assert_eq!(
        items[0]["summary"], "param id\nunfiltered leads to injection",
        "summary not restored correctly"
    );
    // The id must be a JSON number, not a string.
    assert!(
        items[0]["finding_id"].is_number(),
        "finding_id should be a number, got {}",
        items[0]["finding_id"]
    );
}

#[tokio::test]
async fn webhook_custom_template_and_headers() {
    let _g = common::lock_env().await;
    std::env::set_var(artex::notify::http::ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"ok":true}"#).await;
    let cfg = json!({
        "url": srv.url,
        "headers": { "X-Token": "s3cret" },
        "body_template": r#"{"msg": {{json (printf "%d items" .Count)}}, "first": {{json (index .Items 0).Name}}}"#,
    });

    WebhookChannel
        .send(&Context::background(), &cfg, &batch_msg(3))
        .await
        .expect("delivery failed");

    let req = srv.records().last().expect("a request was made").clone();
    assert_eq!(req.header("X-Token"), Some("s3cret"), "custom header lost");
    let body: Value = serde_json::from_slice(&req.body).expect("request body is valid JSON");
    assert_eq!(
        body["msg"], "3 items",
        "custom template rendered incorrectly"
    );
    assert_eq!(body["first"], "vuln1", "range/index extraction wrong");
}

#[tokio::test]
async fn webhook_reports_all_items_delivered() {
    // `TestWebhookAndEmailReportAllItems`: the webhook does not truncate, so the
    // whole batch counts as delivered (returned count == number of items).
    let _g = common::lock_env().await;
    std::env::set_var(artex::notify::http::ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = json_server(r#"{"ok":true}"#).await;
    let cfg = json!({ "url": srv.url });

    let kept = WebhookChannel
        .send(&Context::background(), &cfg, &batch_msg(7))
        .await
        .expect("delivery failed");
    assert_eq!(kept, 7, "a webhook delivers the whole batch");
}

#[tokio::test]
async fn webhook_rejects_non_json_render_result() {
    let cfg = json!({ "url": "https://example.com/hook", "body_template": "not json at all" });
    let err = WebhookChannel
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("a non-JSON render result should fail");
    assert!(
        err.is_permanent(),
        "a non-JSON render is a permanent failure, got {err}"
    );
}

#[test]
fn webhook_validate_catches_bad_config_early() {
    let bad = [
        json!({}),
        json!({ "url": "file:///etc/passwd" }),
        json!({ "url": "https://example.com", "method": "DELETE" }),
        json!({ "url": "https://example.com", "body_template": "{{.Items." }),
    ];
    for (i, cfg) in bad.iter().enumerate() {
        assert!(
            WebhookChannel.validate(cfg).is_err(),
            "config set {i} should be rejected: {cfg}"
        );
    }
}

// === webhook_template_test.go capability-boundary specs ===

#[test]
fn webhook_template_context_is_pure_data() {
    // Rust analog of TestTemplateContextHasNoMethods: the template context is pure
    // data, so the capability surface is exactly its keys — no extra "method"-like
    // fields can be reached. Pin both the top-level and per-item key sets.
    let data = webhook_template_data(&single_msg());
    let obj = data.as_object().expect("context is an object");
    let mut top: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
    top.sort_unstable();
    assert_eq!(
        top,
        vec!["Batch", "Count", "HomeURL", "Items", "SentAt", "Title"],
        "template context keys changed — before adding one, confirm it exposes no capability"
    );

    let item = data["Items"]
        .get(0)
        .expect("one item")
        .as_object()
        .expect("item is an object");
    let mut keys: Vec<&str> = item.keys().map(|s| s.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "Assets",
            "DetailURL",
            "FindingID",
            "FromStatus",
            "Name",
            "Severity",
            "SeverityLabel",
            "StatusLabel",
            "Summary",
            "ToStatus",
            "VulnClass",
        ],
        "template item keys changed — the context must stay pure data (zero methods)"
    );
}

#[test]
fn webhook_template_funcs_are_minimal() {
    // Rust analog of TestTemplateFuncsAreMinimal: only pure-serialization helpers
    // exist. A template that reaches for any other capability (file/network/exec)
    // must fail to parse, naming the unknown function.
    for bad in [
        r#"{{ readFile "/etc/passwd" }}"#,
        r#"{{ httpGet "http://x" }}"#,
        r#"{{ exec "id" }}"#,
    ] {
        let err = parse_webhook_template(bad)
            .expect_err("a non-existent function must be rejected at parse time");
        assert!(
            err.contains("not defined"),
            "parse error should report an undefined function, got {err:?}"
        );
    }
}

#[test]
fn webhook_template_cannot_reach_unknown_data() {
    // Accessing a field that does not exist must error — not echo a zero value — and
    // the error must not carry real content (title/summary) out.
    let err = render_webhook_body(r#"{"x": {{.Environment}}, "y": {{.Env}}}"#, &single_msg())
        .expect_err("accessing a non-existent field should error");
    for leak in ["SQL injection", "param id"] {
        assert!(
            !err.contains(leak),
            "template error leaked message content {leak:?}: {err}"
        );
    }
}

#[test]
fn webhook_template_render_fails_permanently() {
    let cfg = json!({ "url": "https://example.com/hook", "body_template": "{{.Items." });
    // Caught at save time…
    assert!(
        WebhookChannel.validate(&cfg).is_err(),
        "template syntax errors should be caught at save time"
    );
    // …and even if validation is bypassed, a direct send is a permanent failure.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(WebhookChannel.send(&Context::background(), &cfg, &single_msg()))
        .expect_err("a bad template should fail the send");
    assert!(
        err.is_permanent(),
        "a bad template should be a permanent failure, got {err}"
    );
}

#[test]
fn webhook_template_can_only_produce_json() {
    // A valid template passes validation.
    let ok =
        json!({ "url": "https://example.com/hook", "body_template": r#"{"t":{{json .Title}}}"# });
    assert!(
        WebhookChannel.validate(&ok).is_ok(),
        "a valid template should pass validation"
    );

    // A template whose render result is not JSON must be rejected as permanent.
    let bad = json!({ "url": "http://127.0.0.1:1/hook", "body_template": "not json {{.Count}}" });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt
        .block_on(WebhookChannel.send(&Context::background(), &bad, &single_msg()))
        .expect_err("rendering non-JSON should be a permanent failure");
    assert!(err.is_permanent(), "got {err}");
    let msg = match err {
        Error::Permanent(m) | Error::Msg(m) | Error::Transient(m) => m,
    };
    assert!(
        msg.contains("valid JSON"),
        "the error message should indicate a JSON problem, got {msg:?}"
    );
}
