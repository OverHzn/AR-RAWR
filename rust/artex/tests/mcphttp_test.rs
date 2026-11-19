//! Rust port of the `mcphttp` client specs: `client_test.go` and
//! `client_sse_test.go`.
//!
//! * `streamable_client_request_timeout` — `TestStreamableClientRequestTimeout`
//! * `streamable_client_protocol_versions` — `TestStreamableClientProtocolVersions`
//! * `streamable_client_version_rejection_forms` — `TestStreamableClientVersionRejectionForms`
//! * `legacy_sse_client_discover_and_call` — `TestLegacySSEClientDiscoverAndCall`
//!
//! Go's `httptest.NewServer` is replaced by small purpose-built tokio TCP servers:
//! the Streamable cases answer each POST with one JSON response (the `slow_call`
//! case sleeps inside the handler to exercise the per-request timeout), and the
//! legacy case runs the long-lived `GET /sse` stream alongside `POST /message`,
//! sharing per-session state the way the Go handler does (`/sse` owns the writer
//! and pumps message frames from a channel; `/message` only pushes into it).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use artex::llm::Context;

// === a minimal per-connection HTTP/1.1 request reader ===

struct Inbound {
    // The HTTP method line; the tests dispatch on the JSON-RPC method in the body,
    // so this is parsed for completeness but not read directly.
    #[allow(dead_code)]
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Inbound {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read one full HTTP request (headers + content-length body) from `sock`.
async fn read_request(sock: &mut TcpStream) -> std::io::Result<Option<Inbound>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    Ok(Some(Inbound {
        method,
        path,
        headers,
        body,
    }))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Status",
    }
}

/// Write a complete, Content-Length framed HTTP response (Connection: close).
async fn write_response(
    sock: &mut TcpStream,
    status: u16,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", status, reason(status));
    out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    sock.write_all(out.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.flush().await
}

fn json_ct() -> Vec<(&'static str, String)> {
    vec![("Content-Type", "application/json".to_string())]
}

fn rpc_id(v: &Value) -> i64 {
    v.get("id").and_then(Value::as_i64).unwrap_or(0)
}

fn headers_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// === Streamable HTTP test server ===
//
// Answers each POST (one request per connection) with a single JSON response chosen
// by the JSON-RPC method. The `tools/call` slow_call method sleeps `slow_seconds`.

type BoxFut = std::pin::Pin<
    Box<dyn std::future::Future<Output = (u16, Vec<(&'static str, String)>, Vec<u8>)> + Send>,
>;

/// Spawn a Streamable-style server. `responder` decides the response per request.
async fn spawn_streamable<F>(responder: F) -> (String, tokio::task::JoinHandle<()>)
where
    F: Fn(Inbound) -> BoxFut + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let responder = Arc::new(responder);
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let responder = responder.clone();
            tokio::spawn(async move {
                let Some(req) = (read_request(&mut sock).await).ok().flatten() else {
                    return;
                };
                let (status, headers, body) = responder(req).await;
                let _ = write_response(&mut sock, status, &headers, &body).await;
            });
        }
    });
    (format!("http://{addr}"), task)
}

/// The initialize / tools-list / notifications / slow-call responder for the timeout
/// test. The `tools/call` slow_call method sleeps `slow_seconds`.
fn timeout_responder(slow_seconds: u64) -> impl Fn(Inbound) -> BoxFut + Send + Sync + 'static {
    move |req: Inbound| {
        Box::pin(async move {
            let v = req.json();
            let method = v
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = rpc_id(&v);
            let body = match method.as_str() {
                "initialize" => json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"protocolVersion":"2025-06-18","capabilities":{},
                              "serverInfo":{"name":"t","version":"0"}}
                })
                .to_string()
                .into_bytes(),
                "notifications/initialized" => return (202, Vec::new(), Vec::new()),
                "tools/list" => json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"tools":[{"name":"slow_call","description":"slow",
                                        "inputSchema":{"type":"object"}}]}
                })
                .to_string()
                .into_bytes(),
                "tools/call" => {
                    tokio::time::sleep(Duration::from_secs(slow_seconds)).await;
                    json!({"jsonrpc":"2.0","id":id,
                           "result":{"content":[{"type":"text","text":"done"}]}})
                    .to_string()
                    .into_bytes()
                }
                _ => Vec::new(),
            };
            (200, json_ct(), body)
        })
    }
}

#[tokio::test]
async fn streamable_client_request_timeout() {
    // timeout=1: the 3s slow call is cut off well before the server answers.
    {
        let (url, task) = spawn_streamable(timeout_responder(3)).await;
        let ctx = Context::background();
        let c = artex::mcphttp::Client::new(ctx.clone(), "t", &url, &BTreeMap::new(), false, 1)
            .await
            .expect("connect");
        c.tools(ctx.clone()).await.expect("tools/list");
        let start = std::time::Instant::now();
        let res = c.call_tool(ctx.clone(), "slow_call", Value::Null).await;
        assert!(
            res.is_err(),
            "expected the 1s-timeout call to fail, got {res:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "call not cut off by the 1s timeout: {:?}",
            start.elapsed()
        );
        c.close().await;
        task.abort();
    }

    // timeout=10: the same slow call completes.
    {
        let (url, task) = spawn_streamable(timeout_responder(3)).await;
        let ctx = Context::background();
        let c = artex::mcphttp::Client::new(ctx.clone(), "t", &url, &BTreeMap::new(), false, 10)
            .await
            .expect("connect");
        let got = c.call_tool(ctx.clone(), "slow_call", Value::Null).await;
        assert_eq!(got.as_deref(), Ok("done"), "10s-timeout Call");
        c.close().await;
        task.abort();
    }

    // timeout=0: falls back to the 120s default (call completes).
    {
        let (url, task) = spawn_streamable(timeout_responder(3)).await;
        let ctx = Context::background();
        let c = artex::mcphttp::Client::new(ctx.clone(), "t", &url, &BTreeMap::new(), false, 0)
            .await
            .expect("connect");
        let got = c.call_tool(ctx.clone(), "slow_call", Value::Null).await;
        assert_eq!(got.as_deref(), Ok("done"), "default-timeout Call");
        c.close().await;
        task.abort();
    }
}

// === protocol-version negotiation ===

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    header: Option<String>,
    body_version: Option<String>,
}

#[tokio::test]
async fn streamable_client_protocol_versions() {
    for supported in ["2025-11-25", "2025-06-18", "2025-03-26"] {
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let supported = supported.to_string();
        let supported_for_closure = supported.clone();

        let (url, task) = spawn_streamable(move |req: Inbound| {
            let seen = seen2.clone();
            let supported = supported_for_closure.clone();
            Box::pin(async move {
                let v = req.json();
                let method = v
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let id = rpc_id(&v);
                let header = req.header("mcp-protocol-version").map(|s| s.to_string());
                let body_version = v
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                seen.lock().unwrap().push(Seen {
                    method: method.clone(),
                    header,
                    body_version,
                });

                // Reject any request whose header disagrees with the one version.
                if let Some(hv) = req.header("mcp-protocol-version") {
                    if hv != supported {
                        let body =
                            r#"{"code":"UNSUPPORTED_PROTOCOL_VERSION"}"#.to_string().into_bytes();
                        return (400, Vec::new(), body);
                    }
                }

                let body = match method.as_str() {
                    "initialize" => json!({
                        "jsonrpc":"2.0","id":id,
                        "result":{"protocolVersion": supported, "capabilities":{},
                                  "serverInfo":{"name":"t","version":"0"}}
                    })
                    .to_string()
                    .into_bytes(),
                    "notifications/initialized" => return (202, Vec::new(), Vec::new()),
                    "tools/list" => json!({"jsonrpc":"2.0","id":id,"result":{"tools":[]}})
                        .to_string()
                        .into_bytes(),
                    _ => Vec::new(),
                };
                (200, json_ct(), body)
            })
        })
        .await;

        let ctx = Context::background();
        let c = artex::mcphttp::Client::new(ctx.clone(), "t", &url, &BTreeMap::new(), false, 0)
            .await
            .expect("connect");
        c.tools(ctx.clone()).await.expect("tools/list");
        assert_eq!(c.protocol(), supported, "negotiated protocol");

        let seen = seen.lock().unwrap().clone();
        let mut inits = 0;
        for rec in &seen {
            if rec.method == "initialize" {
                inits += 1;
                assert!(
                    rec.header.is_none(),
                    "initialize carried MCP-Protocol-Version {:?}",
                    rec.header
                );
                assert_eq!(
                    rec.body_version.as_deref(),
                    Some("2025-11-25"),
                    "initialize offered {:?}",
                    rec.body_version
                );
            } else {
                assert_eq!(
                    rec.header.as_deref(),
                    Some(supported.as_str()),
                    "{} carried MCP-Protocol-Version {:?}",
                    rec.method,
                    rec.header
                );
            }
        }
        assert_eq!(inits, 1, "initialize attempts = {inits}, want 1");
        c.close().await;
        task.abort();
    }
}

// === version-rejection fallback forms ===

#[derive(Clone, Copy)]
enum Reject {
    Http400,
    JsonRpc,
    Unauthorized,
}

#[tokio::test]
async fn streamable_client_version_rejection_forms() {
    let cases: &[(&str, Reject, bool)] = &[
        ("http 400", Reject::Http400, true),
        ("json-rpc -32602", Reject::JsonRpc, true),
        ("unrelated 401", Reject::Unauthorized, false),
    ];

    for (name, reject, fallback) in cases {
        let inits: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let after: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let inits2 = inits.clone();
        let after2 = after.clone();
        let reject = *reject;

        let (url, task) = spawn_streamable(move |req: Inbound| {
            let inits = inits2.clone();
            let after = after2.clone();
            Box::pin(async move {
                let v = req.json();
                let method = v.get("method").and_then(Value::as_str).unwrap_or("").to_string();
                let id = rpc_id(&v);
                let header = req.header("mcp-protocol-version").map(|s| s.to_string());
                let body_version = v
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let rec = Seen {
                    method: method.clone(),
                    header,
                    body_version: body_version.clone(),
                };
                if method == "initialize" {
                    inits.lock().unwrap().push(rec);
                    // Accept only the fallback version; reject anything else per the case.
                    if body_version.as_deref() != Some("2025-06-18") {
                        return match reject {
                            Reject::Http400 => (
                                400,
                                Vec::new(),
                                b"Bad Request: Unsupported protocol version\n".to_vec(),
                            ),
                            Reject::JsonRpc => (
                                200,
                                json_ct(),
                                json!({
                                    "jsonrpc":"2.0","id":id,
                                    "error":{"code":-32602,"message":"Unsupported protocol version",
                                             "data":{"supported":["2025-06-18"],"requested":"2025-11-25"}}
                                })
                                .to_string()
                                .into_bytes(),
                            ),
                            Reject::Unauthorized => (401, Vec::new(), b"unauthorized\n".to_vec()),
                        };
                    }
                } else {
                    after.lock().unwrap().push(rec);
                }

                let body = match method.as_str() {
                    "initialize" => json!({
                        "jsonrpc":"2.0","id":id,
                        "result":{"protocolVersion":"2025-06-18","capabilities":{},
                                  "serverInfo":{"name":"t","version":"0"}}
                    })
                    .to_string()
                    .into_bytes(),
                    "notifications/initialized" => return (202, Vec::new(), Vec::new()),
                    _ => Vec::new(),
                };
                (200, json_ct(), body)
            })
        })
        .await;

        let ctx = Context::background();
        let c =
            artex::mcphttp::Client::new(ctx.clone(), "t", &url, &BTreeMap::new(), false, 0).await;

        // No initialize may carry a protocol header.
        for rec in inits.lock().unwrap().iter() {
            assert!(
                rec.header.is_none(),
                "[{name}] initialize carried MCP-Protocol-Version {:?}",
                rec.header
            );
        }

        if !fallback {
            assert!(c.is_err(), "[{name}] expected connect error");
            assert_eq!(
                inits.lock().unwrap().len(),
                1,
                "[{name}] unrelated error retried"
            );
            task.abort();
            continue;
        }

        let c = c.unwrap_or_else(|e| panic!("[{name}] connect: {e}"));
        let ins = inits.lock().unwrap().clone();
        assert_eq!(ins.len(), 2, "[{name}] fallback attempts = {ins:?}");
        assert_eq!(
            ins[0].body_version.as_deref(),
            Some("2025-11-25"),
            "[{name}] first offered"
        );
        assert_eq!(
            ins[1].body_version.as_deref(),
            Some("2025-06-18"),
            "[{name}] fallback offered"
        );
        assert_eq!(c.protocol(), "2025-06-18", "[{name}] negotiated protocol");
        let aft = after.lock().unwrap().clone();
        assert!(!aft.is_empty(), "[{name}] no post-initialize requests");
        assert_eq!(aft[0].method, "notifications/initialized");
        assert_eq!(
            aft[0].header.as_deref(),
            Some("2025-06-18"),
            "[{name}] notification protocol header"
        );
        c.close().await;
        task.abort();
    }
}

// === legacy SSE transport ===

/// Shared state for the legacy SSE server: whether the session was registered (by
/// the `/sse` handler) and the channel `/message` pushes response frames into (the
/// `/sse` handler owns the receiving end and pumps them to the open stream).
struct LegacyState {
    session_known: Mutex<bool>,
    messages_tx: tokio::sync::mpsc::UnboundedSender<String>,
}

#[tokio::test]
async fn legacy_sse_client_discover_and_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let (msg_tx, msg_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    // The /sse handler takes the receiver; stash it so only that connection owns it.
    let shared_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>> =
        Arc::new(Mutex::new(Some(msg_rx)));
    let state = Arc::new(LegacyState {
        session_known: Mutex::new(false),
        messages_tx: msg_tx,
    });

    let state2 = state.clone();
    let shared_rx2 = shared_rx.clone();
    let base2 = base.clone();
    let accept = tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let state = state2.clone();
            let shared_rx = shared_rx2.clone();
            let base = base2.clone();
            tokio::spawn(async move {
                let _ = handle_sse_conn(sock, state, shared_rx, base).await;
            });
        }
    });

    let headers = headers_of(&[("Authorization ", "Bearer test")]);
    let ctx = Context::background();
    let c = artex::mcphttp::Client::new_sse(
        ctx.clone(),
        "test",
        &format!("{base}/sse"),
        &headers,
        false,
    )
    .await
    .expect("NewSSE");

    let tools = c.tools(ctx.clone()).await.expect("tools/list");
    assert_eq!(tools.len(), 1, "unexpected tool count: {}", tools.len());
    assert!(
        tools[0].name().contains("echo"),
        "unexpected tool name: {}",
        tools[0].name()
    );

    let got = c
        .call_tool(ctx.clone(), "echo", json!({"value": "x"}))
        .await;
    assert_eq!(got.as_deref(), Ok("ok"), "Call echo");

    c.close().await;
    accept.abort();
}

/// Handle one connection on the legacy server. `/sse` opens the long-lived stream,
/// sends the endpoint event, then pumps JSON-RPC response frames from its message
/// receiver to the client; `/message` acknowledges the POST and pushes the JSON-RPC
/// response frame into the shared channel.
async fn handle_sse_conn(
    mut sock: TcpStream,
    state: Arc<LegacyState>,
    shared_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>>,
    base: String,
) -> std::io::Result<()> {
    let Some(req) = read_request(&mut sock).await? else {
        return Ok(());
    };

    if req.path == "/sse" {
        *state.session_known.lock().unwrap() = true;
        // Raw 200 with an event-stream body (no Content-Length, keep-alive).
        let head = "HTTP/1.1 200 OK\r\n\
                    Content-Type: text/event-stream\r\n\
                    Cache-Control: no-cache\r\n\
                    Connection: keep-alive\r\n\r\n";
        sock.write_all(head.as_bytes()).await?;
        let endpoint = format!("event: endpoint\ndata: {base}/message?sessionId=test-session\n\n");
        sock.write_all(endpoint.as_bytes()).await?;
        sock.flush().await?;

        // Take the message receiver (dropping the std Mutex guard before we await so
        // the future stays Send) and pump frames to the open stream. The loop ends on
        // a write error (client gone) or when the channel closes.
        let rx = shared_rx.lock().unwrap().take();
        if let Some(mut rx) = rx {
            while let Some(payload) = rx.recv().await {
                let frame = format!("event: message\ndata: {payload}\n\n");
                if sock.write_all(frame.as_bytes()).await.is_err() {
                    break;
                }
                if sock.flush().await.is_err() {
                    break;
                }
            }
        }
        return Ok(());
    }

    if req.path.starts_with("/message") {
        let known = *state.session_known.lock().unwrap();
        let has_session = req
            .path
            .split_once('?')
            .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("sessionId=")))
            == Some("test-session");
        if !has_session || !known {
            return write_response(&mut sock, 400, &[], b"unknown session\n").await;
        }
        let v = req.json();
        let id = rpc_id(&v);
        let method = v
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        write_response(&mut sock, 202, &[], b"").await?;
        if id == 0 {
            return Ok(());
        }
        let result = match method.as_str() {
            "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},
                                   "serverInfo":{"name":"test","version":"1"}}),
            "tools/list" => json!({"tools":[{"name":"echo","description":"echo input",
                                            "inputSchema":{"type":"object"}}]}),
            "tools/call" => json!({"content":[{"type":"text","text":"ok"}]}),
            _ => json!({}),
        };
        let payload = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
        let _ = state.messages_tx.send(payload);
        return Ok(());
    }

    write_response(&mut sock, 404, &[], b"").await
}
