//! Rust port of `notify/http_test.go` (doJSON's HTTP-layer error classification) plus
//! the doJSON-level SSRF dial-guard and redirect-policy specs. These exercise the IO
//! shell ([`artex::notify::http::do_json`]) directly against real local sockets, so the
//! same code path the channel adapters use is covered end-to-end.
//!
//! The per-channel variants (dial guard through `dingTalkChannel.Send`, cross-host
//! redirect through a channel) live with the adapters; here we pin the shared engine.
//! Each test holds [`common::lock_env`] and sets `ARTEX_NOTIFY_ALLOW_LOCAL` itself,
//! because Rust runs a file's tests in parallel within one process.

mod common;

use artex::notify::http::{do_json, ALLOW_LOCAL_TARGETS_ENV};
use artex::notify::Context;

use common::{Response, TestServer};

/// `TestDoJSONClassifiesHTTPStatus`: 2xx/5xx/408/429 are retryable, other 4xx permanent;
/// the status code must appear in the message.
#[tokio::test]
async fn do_json_classifies_http_status() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");

    let cases: &[(u16, bool)] = &[
        (200, false),
        (429, false),
        (408, false),
        (500, false),
        (502, false),
        (503, false),
        (400, true),
        (401, true),
        (403, true),
        (404, true),
    ];
    for &(status, permanent) in cases {
        let srv = TestServer::start(move |_req| {
            Response::json(status, r#"{"detail":"upstream says no"}"#)
        })
        .await;
        let res = do_json(&Context::background(), "GET", &srv.url, &[], None).await;
        if (200..300).contains(&status) {
            assert!(
                res.is_ok(),
                "HTTP {status} (2xx) should not error: {:?}",
                res.err()
            );
            continue;
        }
        let err = res.expect_err("non-2xx should error");
        assert_eq!(
            err.is_permanent(),
            permanent,
            "HTTP {status} permanent classification wrong: {err}"
        );
        assert!(
            err.message().contains(&status.to_string()),
            "error should carry HTTP status code {status}: {err}"
        );
    }
}

/// `TestDoJSONIncludesResponseSnippet`: the peer's error description must survive.
#[tokio::test]
async fn do_json_includes_response_snippet() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv =
        TestServer::start(|_req| Response::json(400, r#"{"error":"invalid webhook token"}"#)).await;
    let err = do_json(&Context::background(), "GET", &srv.url, &[], None)
        .await
        .expect_err("should error");
    assert!(
        err.message().contains("invalid webhook token"),
        "error should bring back the peer's description: {err}"
    );
}

/// `TestDoJSONSnippetIsSingleLineAndBounded`: snippet collapses to one line and caps
/// length so the delivery-history layout / payload is not wrecked.
#[tokio::test]
async fn do_json_snippet_single_line_and_bounded() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let long = "x".repeat(5000);
    let body = format!("line1\nline2\r\n\tline3 {long}");
    let srv = TestServer::start(move |_req| Response::new(500, body.clone())).await;
    let err = do_json(&Context::background(), "GET", &srv.url, &[], None)
        .await
        .expect_err("should error");
    let msg = err.message();
    assert!(
        !msg.contains(['\r', '\n', '\t']),
        "error should be a single line: {msg:?}"
    );
    assert!(
        msg.len() <= 400,
        "error too long ({} bytes): {msg:?}",
        msg.len()
    );
}

/// `TestDoJSONRejectsOversizedResponse`: reads are capped, so a 1 MiB peer body cannot
/// bloat `last_error`.
#[tokio::test]
async fn do_json_rejects_oversized_response() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let huge = "A".repeat(1 << 20);
    let srv = TestServer::start(move |_req| Response::new(400, huge.clone())).await;
    let err = do_json(&Context::background(), "GET", &srv.url, &[], None)
        .await
        .expect_err("should error");
    assert!(
        err.message().len() <= 400,
        "oversized responses should be length-capped, got {} bytes",
        err.message().len()
    );
}

/// `TestDialGuardRejectsLoopbackByDefault`: with the opt-in closed, delivery to a
/// loopback address must be refused at the connection layer — the local service is
/// never hit — and the error explains how to opt in.
#[tokio::test]
async fn dial_guard_rejects_loopback_by_default() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "");
    let srv = TestServer::start(|_req| Response::json(200, r#"{"errcode":0}"#)).await;
    let err = do_json(&Context::background(), "GET", &srv.url, &[], None)
        .await
        .expect_err("delivery to loopback should be refused by default");
    assert_eq!(
        srv.hit_count(),
        0,
        "the request reached the local service — guard failed"
    );
    assert!(
        err.message().contains(ALLOW_LOCAL_TARGETS_ENV),
        "refusal should explain how to opt in: {err}"
    );
}

/// `TestDialGuardAllowsLoopbackWhenOptedIn`: once explicitly enabled, local delivery
/// works (a local postfix / intranet relay is legitimate).
#[tokio::test]
async fn dial_guard_allows_loopback_when_opted_in() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let srv = TestServer::start(|_req| Response::json(200, r#"{"errcode":0,"errmsg":"ok"}"#)).await;
    let body = do_json(&Context::background(), "GET", &srv.url, &[], None)
        .await
        .expect("delivery should work once explicitly allowed");
    assert_eq!(body, br#"{"errcode":0,"errmsg":"ok"}"#.to_vec());
    assert_eq!(srv.hit_count(), 1);
}

/// Cross-host redirect is refused (credentials live in the URL; the request is not
/// followed to the other host). Two servers on different ports = different Hosts.
#[tokio::test]
async fn cross_host_redirect_refused() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let b = TestServer::start(|_req| Response::json(200, r#"{"errcode":0}"#)).await;
    let b_url = b.url_for("/steal");
    let a = TestServer::start(move |_req| Response::redirect(302, b_url.clone())).await;

    let err = do_json(
        &Context::background(),
        "GET",
        &a.url_for("/start"),
        &[],
        None,
    )
    .await
    .expect_err("cross-host redirect should be refused");
    let msg = err.message();
    assert!(msg.contains("refusing cross-host redirect"), "got: {msg}");
    assert_eq!(
        b.hit_count(),
        0,
        "credentials must not be followed to the other host"
    );
    assert_eq!(
        a.hit_count(),
        1,
        "only the original request should reach server A"
    );
}

/// Same-host redirect is followed (e.g. adding a trailing slash), so legitimate
/// endpoints that normalise their path still deliver. The redirect target is built
/// from the request's own `Host` header, so it is same-host by construction.
#[tokio::test]
async fn same_host_redirect_allowed() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let a = TestServer::start(|req| {
        if req.path.starts_with("/final") {
            Response::new(200, "ok")
        } else {
            let host = req.header("host").unwrap_or("").to_string();
            Response::redirect(302, format!("http://{host}/final"))
        }
    })
    .await;

    let body = do_json(
        &Context::background(),
        "GET",
        &a.url_for("/start"),
        &[],
        None,
    )
    .await
    .expect("same-host redirect should be followed");
    assert_eq!(body, b"ok".to_vec());
    assert_eq!(a.hit_count(), 2, "original + the followed redirect");
}

/// Sanity: a cancelled context short-circuits before any socket work.
#[tokio::test]
async fn do_json_respects_cancelled_context() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let ctx = Context::canceled();
    let err = do_json(&ctx, "GET", "http://127.0.0.1:1/", &[], None)
        .await
        .expect_err("cancelled context should error");
    assert!(
        err.is_canceled(),
        "expected a cancellation error, got: {err}"
    );
}

/// Resolver-path dial guard: a **hostname** that resolves only to loopback
/// (`localhost` → 127.0.0.1 + ::1) is refused by [`GuardResolver`] when the opt-in is
/// closed, and the local service is never reached. The literal-IP test above covers the
/// hyper-util IP-literal fast path; this covers the resolver path a real `https://host`
/// delivery takes. It also proves the guard does not *fail open* — if it returned the
/// addresses unfiltered, the request would reach the server and `hit_count` would be 1.
#[tokio::test]
async fn resolver_blocks_hostname_resolving_to_loopback() {
    let _g = common::lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "");
    let srv = TestServer::start(|_req| Response::json(200, r#"{"errcode":0}"#)).await;
    // Swap the literal host for `localhost` (same server, reached by name).
    let named = srv.url.replace("127.0.0.1", "localhost");

    let err = do_json(&Context::background(), "GET", &named, &[], None)
        .await
        .expect_err("a name resolving only to loopback should be refused by default");
    assert_eq!(
        srv.hit_count(),
        0,
        "request reached the local service — resolver failed open"
    );
    assert!(
        err.message().contains(ALLOW_LOCAL_TARGETS_ENV),
        "refusal should explain how to opt in: {err}"
    );
}
