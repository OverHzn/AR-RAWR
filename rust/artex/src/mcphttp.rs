//! Rust port of the ARTEX `mcphttp/` package: remote Streamable HTTP and legacy
//! SSE MCP clients.
//!
//! The core `mcp` package only speaks stdio; this adapts an HTTP/JSON-RPC MCP
//! server to the same [`CoreTool`] shape (`mcp__server__tool`) WITHOUT touching an
//! SDK. It implements the MCP "Streamable HTTP" transport (JSON-RPC over HTTP
//! POST, the server free to answer as `application/json` or a `text/event-stream`
//! carrying the response) and the pre-2025 legacy SSE transport (a long-lived
//! `GET /sse` announces a per-session `POST /message` endpoint; JSON-RPC responses
//! arrive asynchronously as SSE `message` events).
//!
//! Faithful to `mcphttp/client.go`. Go's `context.Context` is
//! [`crate::llm::Context`] (cancellation via `cancelled()` / `err()`);
//! `*http.Client` is `reqwest::Client`. The pure helpers `ToolName`,
//! `normalizeHeaders`, `isUnsupportedVersion` are reused from the `mcphttp-rs` /
//! `mcphttpx-rs` parity crates.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use crate::llm::Context;
use crate::tool::{build, CoreTool, RawInput, Spec, ToolContext};

pub use mcphttp_rs::port::tool_name;
pub use mcphttpx_rs::port::{is_unsupported_version, normalize_headers};

const PROTOCOL_VERSION: &str = "2025-11-25";
const LEGACY_SSE_PROTOCOL_VERSION: &str = "2024-11-05";

// === JSON-RPC wire types ===

#[derive(Debug, Clone, Serialize)]
struct RpcRequest {
    jsonrpc: &'static str,
    // Go `ID int json:"id,omitempty"`: a notification (id 0) omits the field.
    #[serde(skip_serializing_if = "is_zero")]
    id: i64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: i64,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mcp rpc error {}: {}", self.code, self.message)
    }
}

// === Client ===

/// Shared mutable state behind the client's mutex (Go `Client.mu` + fields).
struct Inner {
    next_id: i64,
    session_id: String,
    initialized: bool,
    protocol: String,
    // Legacy SSE only.
    message_url: String,
    pending: BTreeMap<i64, mpsc::Sender<RpcResponse>>,
    stream_err: watch::Sender<Option<String>>,
    legacy: bool,
}

/// A connection to one remote MCP server. [`Client::new`] uses Streamable HTTP;
/// [`Client::new_sse`] uses the legacy GET /sse + POST /message transport.
pub struct Client {
    server: String,
    url: String,
    headers: BTreeMap<String, String>,
    http: reqwest::Client,
    inner: Mutex<Inner>,
    // Legacy SSE: the background reader task (aborted on Close).
    stream_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    // Serialises legacyRoundTrip (Go `legacyMu`), so async POSTs are one at a time.
    legacy_mu: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl Client {
    /// `mcphttp.New`: connect to a remote MCP endpoint and perform the initialize
    /// handshake. `headers` are sent on every request (Authorization, …). When
    /// `insecure` is true, TLS certificate verification is skipped. `timeout_seconds`
    /// bounds each request (0 → 120s default).
    pub async fn new(
        ctx: Context,
        server: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
        insecure: bool,
        timeout_seconds: i64,
    ) -> Result<Arc<Client>, String> {
        let clean_headers = normalize_headers(headers)?;
        let timeout = if timeout_seconds > 0 {
            Duration::from_secs(timeout_seconds as u64)
        } else {
            Duration::from_secs(120)
        };
        let mut builder = reqwest::Client::builder().timeout(timeout);
        if insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let http = builder.build().map_err(|e| e.to_string())?;
        let c = Arc::new(Client {
            server: server.to_string(),
            url: url.to_string(),
            headers: clean_headers,
            http,
            inner: Mutex::new(Inner {
                next_id: 0,
                session_id: String::new(),
                initialized: false,
                protocol: PROTOCOL_VERSION.to_string(),
                message_url: String::new(),
                pending: BTreeMap::new(),
                stream_err: watch::channel(None).0,
                legacy: false,
            }),
            stream_task: Mutex::new(None),
            legacy_mu: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        });
        c.initialize(ctx).await?;
        Ok(c)
    }

    /// `mcphttp.NewSSE`: connect to the legacy MCP SSE transport. A long-lived
    /// `GET /sse` announces a per-session `POST /message` endpoint; JSON-RPC
    /// responses are delivered asynchronously as SSE `message` events.
    pub async fn new_sse(
        ctx: Context,
        server: &str,
        sse_url: &str,
        headers: &BTreeMap<String, String>,
        insecure: bool,
    ) -> Result<Arc<Client>, String> {
        let clean_headers = normalize_headers(headers)?;
        let mut builder = reqwest::Client::builder();
        if insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }
        // The SSE stream is intentionally long-lived: no overall timeout.
        let http = builder.build().map_err(|e| e.to_string())?;

        let mut req = http.get(sse_url).header("Accept", "text/event-stream");
        for (k, v) in &clean_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let snip = read_snippet(resp).await;
            return Err(format!("mcp sse http {status}: {snip}"));
        }

        let (stream_err_tx, _) = watch::channel(None::<String>);
        let (endpoint_tx, endpoint_rx) = watch::channel(String::new());
        let c = Arc::new(Client {
            server: server.to_string(),
            url: sse_url.to_string(),
            headers: clean_headers,
            http,
            inner: Mutex::new(Inner {
                next_id: 0,
                session_id: String::new(),
                initialized: false,
                protocol: LEGACY_SSE_PROTOCOL_VERSION.to_string(),
                message_url: String::new(),
                pending: BTreeMap::new(),
                stream_err: stream_err_tx,
                legacy: true,
            }),
            stream_task: Mutex::new(None),
            legacy_mu: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        });

        // Spawn the background reader: read the endpoint line, then dispatch
        // JSON-RPC responses from the remainder of the stream to pending callers.
        let reader = {
            let c = Arc::clone(&c);
            tokio::spawn(async move { read_legacy_sse(c, resp, endpoint_tx).await })
        };
        *c.stream_task.lock().unwrap() = Some(reader);

        // Wait for the reader to announce the message endpoint.
        let mut rx = endpoint_rx;
        let endpoint = loop {
            let cur = rx.borrow().clone();
            if !cur.is_empty() {
                break cur;
            }
            if rx.changed().await.is_err() {
                c.close().await;
                return Err("mcp sse endpoint: stream closed".to_string());
            }
        };
        if endpoint.is_empty() {
            c.close().await;
            return Err("mcp sse endpoint: empty".to_string());
        }
        c.inner.lock().unwrap().message_url = endpoint;

        if let Err(e) = c.initialize(ctx).await {
            c.close().await;
            return Err(e);
        }
        Ok(c)
    }

    // === initialize / handshake ===

    async fn initialize(&self, ctx: Context) -> Result<(), String> {
        let versions: &[&str] = if self.inner.lock().unwrap().legacy {
            &[LEGACY_SSE_PROTOCOL_VERSION]
        } else {
            &[PROTOCOL_VERSION, "2025-06-18"]
        };
        let n = versions.len();
        for (i, version) in versions.iter().enumerate() {
            self.inner.lock().unwrap().protocol = (*version).to_string();
            let result = self
                .call(
                    ctx.clone(),
                    "initialize",
                    Some(json!({
                        "protocolVersion": version,
                        "capabilities": {},
                        "clientInfo": {"name": "artex", "version": "0.2"}
                    })),
                )
                .await;
            match result {
                Err(e) => {
                    if i + 1 < n && is_unsupported_version(&e) {
                        continue;
                    }
                    return Err(e);
                }
                Ok(raw) => {
                    let negotiated: Value = raw;
                    {
                        let mut inner = self.inner.lock().unwrap();
                        if let Some(v) = negotiated.get("protocolVersion").and_then(Value::as_str) {
                            if !v.is_empty() {
                                inner.protocol = v.to_string();
                            }
                        }
                        inner.initialized = true;
                    }
                    return self
                        .notify(ctx, "notifications/initialized", Some(json!({})))
                        .await;
                }
            }
        }
        Err("mcp: no supported protocol version".to_string())
    }

    // === public surface ===

    /// `mcphttp.Client.Tools`: list the server's tools, adapted to [`CoreTool`]s.
    /// Takes `self: &Arc<Client>` so each tool's `Run` closure can own a handle to
    /// the client (Go captures `c *Client` directly).
    pub async fn tools(self: &Arc<Self>, ctx: Context) -> Result<Vec<Arc<dyn CoreTool>>, String> {
        let raw = self.call(ctx, "tools/list", Some(json!({}))).await?;
        #[derive(Deserialize)]
        struct ListRes {
            #[serde(default)]
            tools: Vec<RemoteTool>,
        }
        let res: ListRes = serde_json::from_value(raw).map_err(|e| e.to_string())?;
        Ok(res.tools.into_iter().map(|rt| self.wrap(rt)).collect())
    }

    fn wrap(self: &Arc<Self>, rt: RemoteTool) -> Arc<dyn CoreTool> {
        let schema = if rt.input_schema.is_null() {
            json!({ "type": "object" })
        } else {
            rt.input_schema.clone()
        };
        let full = tool_name(self.server.clone(), rt.name.clone())
            .unwrap_or_else(|_| format!("mcp__{}__{}", self.server, rt.name));
        let this = Arc::clone(self);
        let rt_name = rt.name.clone();
        let full_perm = full.clone();
        let spec = Spec {
            name: full,
            description: rt.description.clone(),
            prompt: String::new(),
            schema,
            read_only: None,
            concurrent: None,
            permissions: Some(Arc::new(move |_ctx, _input: RawInput, _pc| {
                crate::permission::ask_user(&format!("call MCP tool {full_perm}?"))
            })),
            run: Arc::new(move |ctx: Context, input: RawInput, tc: ToolContext| {
                let this = Arc::clone(&this);
                let name = rt_name.clone();
                Box::pin(async move { this.run_tool(ctx, &name, input, tc).await })
            }),
            raw_input: false,
        };
        build(spec)
    }

    /// The Run closure body: tools/call, concatenate content text, run through
    /// `Capture`. Errors become an error `Result` (Go `actool.Errorf`), not `Err`.
    async fn run_tool(
        &self,
        ctx: Context,
        name: &str,
        input: RawInput,
        tc: ToolContext,
    ) -> (crate::tool::Result, Option<String>) {
        let mut args = Value::Null;
        if !input.is_empty() {
            if let Ok(v) = serde_json::from_slice::<Value>(&input) {
                args = v;
            }
        }
        match self
            .call(
                ctx,
                "tools/call",
                Some(json!({ "name": name, "arguments": args })),
            )
            .await
        {
            Err(e) => (crate::tool::errorf(&format!("Error: {e}")), None),
            Ok(raw) => match serde_json::from_value::<CallRes>(raw) {
                Err(e) => (
                    crate::tool::errorf(&format!("Error: bad MCP response: {e}")),
                    None,
                ),
                Ok(res) => {
                    let mut text = String::new();
                    for blk in &res.content {
                        text.push_str(&blk.text);
                    }
                    let captured = crate::tool::capture(&tc, &text);
                    (
                        crate::tool::Result {
                            content: vec![crate::tool::single_text_block(&captured)],
                            is_error: res.is_error,
                            extra: None,
                        },
                        None,
                    )
                }
            },
        }
    }

    /// `mcphttp.Client.Call`: invoke one tool and return the concatenated text of
    /// its content blocks. Bypasses the agent permission layer — for backend batch
    /// jobs. Returns `Err` when the tool reports `isError`.
    pub async fn call_tool(&self, ctx: Context, tool: &str, args: Value) -> Result<String, String> {
        let raw = self
            .call(
                ctx,
                "tools/call",
                Some(json!({ "name": tool, "arguments": args })),
            )
            .await?;
        let res: CallRes =
            serde_json::from_value(raw).map_err(|e| format!("bad MCP response: {e}"))?;
        let mut text = String::new();
        for blk in &res.content {
            text.push_str(&blk.text);
        }
        if res.is_error {
            return Err(format!("mcp tool {tool:?} error: {text}"));
        }
        Ok(text)
    }

    /// The negotiated protocol version (Go reads `c.protocol` in tests).
    pub fn protocol(&self) -> String {
        self.inner.lock().unwrap().protocol.clone()
    }

    /// `mcphttp.Client.Close`: terminate the session best-effort. Legacy SSE aborts
    /// the stream reader; Streamable HTTP sends a DELETE with the session id.
    pub async fn close(&self) {
        if self.inner.lock().unwrap().legacy {
            if let Some(task) = self.stream_task.lock().unwrap().take() {
                task.abort();
            }
            return;
        }
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let sid = self.inner.lock().unwrap().session_id.clone();
        if sid.is_empty() {
            return;
        }
        let mut req = self.http.delete(&self.url).header("Mcp-Session-Id", &sid);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Ok(resp) = req.send().await {
            let _ = resp.bytes().await;
        }
    }

    // === JSON-RPC plumbing ===

    /// `Client.call`: assign an id, round-trip, unwrap result / error.
    async fn call(
        &self,
        ctx: Context,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String> {
        let id = {
            let mut inner = self.inner.lock().unwrap();
            inner.next_id += 1;
            inner.next_id
        };
        let resp = self
            .round_trip(
                ctx,
                RpcRequest {
                    jsonrpc: "2.0",
                    id,
                    method: method.to_string(),
                    params,
                },
                true,
            )
            .await?;
        let resp = resp.ok_or_else(|| format!("mcp: empty response for {method}"))?;
        if let Some(err) = resp.error {
            return Err(err.to_string());
        }
        Ok(resp.result)
    }

    /// `Client.notify`: a fire-and-forget notification (no response body).
    async fn notify(
        &self,
        ctx: Context,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), String> {
        self.round_trip(
            ctx,
            RpcRequest {
                jsonrpc: "2.0",
                id: 0,
                method: method.to_string(),
                params,
            },
            false,
        )
        .await?;
        Ok(())
    }

    /// `Client.roundTrip`: POST one JSON-RPC frame, parse the reply from JSON or
    /// (Streamable) an SSE body. Legacy SSE routes through `legacy_round_trip`.
    async fn round_trip(
        &self,
        ctx: Context,
        body: RpcRequest,
        expect_resp: bool,
    ) -> Result<Option<RpcResponse>, String> {
        if self.inner.lock().unwrap().legacy {
            return self.legacy_round_trip(ctx, body, expect_resp).await;
        }
        let data = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(data);
        let (sid, initialized, version) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.session_id.clone(),
                inner.initialized,
                inner.protocol.clone(),
            )
        };
        if initialized && !version.is_empty() {
            req = req.header("MCP-Protocol-Version", &version);
        }
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if !sid.is_empty() {
            req = req.header("Mcp-Session-Id", &sid);
        }

        let resp = req.send().await.map_err(|e| format_http_error(&e))?;
        let status = resp.status().as_u16();
        let resp_headers = resp.headers().clone();

        // Capture the session id the server assigns on initialize.
        if sid.is_empty() {
            if let Some(got) = resp_headers
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
            {
                if !got.is_empty() {
                    self.inner.lock().unwrap().session_id = got.to_string();
                }
            }
        }

        if !(200..300).contains(&status) {
            let snip = read_snippet(resp).await;
            return Err(format!("mcp http {status}: {snip}"));
        }
        if !expect_resp {
            let _ = resp.bytes().await;
            return Ok(None);
        }
        let content_type = resp_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if content_type.starts_with("text/event-stream") {
            return parse_sse(resp, body.id).await;
        }
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        let out: RpcResponse = serde_json::from_slice(&bytes)
            .map_err(|e| format!("mcp: decode json response: {e}"))?;
        Ok(Some(out))
    }

    /// `Client.legacyRoundTrip`: POST to the announced endpoint; the JSON-RPC
    /// response arrives on the SSE stream. Serialised for determinism.
    async fn legacy_round_trip(
        &self,
        ctx: Context,
        body: RpcRequest,
        expect_resp: bool,
    ) -> Result<Option<RpcResponse>, String> {
        let _guard = self.legacy_mu.lock().await;
        let data = serde_json::to_vec(&body).map_err(|e| e.to_string())?;

        let response_rx = if expect_resp {
            let (tx, rx) = mpsc::channel::<RpcResponse>(1);
            self.inner.lock().unwrap().pending.insert(body.id, tx);
            Some(rx)
        } else {
            None
        };

        let message_url = self.inner.lock().unwrap().message_url.clone();
        let mut req = self
            .http
            .post(&message_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(data);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let result = async {
            let resp = req.send().await.map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let snip = read_snippet(resp).await;
                return Err(format!("mcp sse http {status}: {snip}"));
            }
            let _ = resp.bytes().await;
            Ok::<(), String>(())
        }
        .await;

        // Always clean up the pending entry (Go `defer delete`).
        if expect_resp {
            self.inner.lock().unwrap().pending.remove(&body.id);
        }
        result?;

        if !expect_resp {
            return Ok(None);
        }
        let mut response_rx = response_rx.expect("response channel for expect_resp");
        let mut stream_err = self.inner.lock().unwrap().stream_err.subscribe();
        loop {
            tokio::select! {
                maybe = response_rx.recv() => {
                    return Ok(Some(maybe.ok_or("mcp sse stream: response channel closed")?));
                }
                _ = ctx.cancelled() => {
                    return Err(ctx.err().map(|e| e.to_string()).unwrap_or_else(|| "context canceled".to_string()));
                }
                _ = stream_err.changed() => {
                    if let Some(e) = stream_err.borrow().clone() {
                        return Err(format!("mcp sse stream: {e}"));
                    }
                }
            }
        }
    }
}

// === Remote tool wire type ===

#[derive(Debug, Clone, Deserialize)]
struct RemoteTool {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "inputSchema")]
    input_schema: Value,
}

/// A `tools/call` result (`content[]` + `isError`).
#[derive(Debug, Deserialize)]
struct CallRes {
    #[serde(default)]
    content: Vec<Content>,
    #[serde(default, rename = "isError")]
    is_error: bool,
}

#[derive(Debug, Deserialize)]
struct Content {
    #[serde(default)]
    #[allow(dead_code)]
    r#type: String,
    #[serde(default)]
    text: String,
}

// === SSE parsing ===

/// `parseSSE`: read an SSE stream and return the first `data:` frame that is a
/// JSON-RPC response matching `want_id` (server-to-client requests/notifications
/// are skipped). Mirrors Go's `bufio.Scanner` (8 MiB max line) line loop.
async fn parse_sse(
    mut resp: reqwest::Response,
    want_id: i64,
) -> Result<Option<RpcResponse>, String> {
    let mut buf = String::new();
    let mut data_buf = String::new();
    loop {
        // Drain every complete line currently buffered.
        while let Some(nl) = buf.find('\n') {
            let mut line: String = buf[..nl].to_string();
            buf.drain(..=nl);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                // event boundary
                if !data_buf.is_empty() {
                    if let Some(r) = decode_sse_frame(&data_buf, want_id) {
                        return Ok(Some(r));
                    }
                    data_buf.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_buf.push_str(rest.trim());
            }
        }
        match resp.chunk().await {
            Ok(Some(bytes)) => buf.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    // Final flush of a trailing, unterminated frame.
    if !data_buf.is_empty() {
        if let Some(r) = decode_sse_frame(&data_buf, want_id) {
            return Ok(Some(r));
        }
    }
    Err("mcp: no matching response in event stream".to_string())
}

/// Decode one SSE data payload into an [`RpcResponse`] if it matches `want_id`.
fn decode_sse_frame(payload: &str, want_id: i64) -> Option<RpcResponse> {
    let out: RpcResponse = serde_json::from_str(payload).ok()?;
    if out.id != want_id {
        return None;
    }
    Some(out)
}

/// `readLegacySSE` (+ `readSSEEndpoint`): consume the long-lived SSE stream. First
/// it reads lines until the `data:` endpoint announcement (resolved to an absolute
/// URL against the stream URL), publishes it on `endpoint_tx`, then dispatches any
/// subsequent `data:` JSON-RPC response to the pending caller with that id.
async fn read_legacy_sse(
    client: Arc<Client>,
    mut resp: reqwest::Response,
    endpoint_tx: watch::Sender<String>,
) {
    let base = client.url.clone();
    let mut buf = String::new();
    let mut endpoint_sent = false;

    loop {
        // Drain every complete line currently buffered.
        while let Some(nl) = buf.find('\n') {
            let mut line: String = buf[..nl].to_string();
            buf.drain(..=nl);
            if line.ends_with('\r') {
                line.pop();
            }
            let Some(rest) = line.strip_prefix("data:") else {
                continue; // event:, id:, comment, blank — not the payload
            };
            let payload = rest.trim();
            if payload.is_empty() {
                continue;
            }
            if !endpoint_sent {
                let resolved = resolve_url(payload, &base);
                let _ = endpoint_tx.send(resolved);
                endpoint_sent = true;
            } else if let Ok(rpc) = serde_json::from_str::<RpcResponse>(payload) {
                if rpc.id != 0 {
                    let tx = client.inner.lock().unwrap().pending.remove(&rpc.id);
                    if let Some(tx) = tx {
                        let _ = tx.send(rpc).await;
                    }
                }
            }
        }

        match resp.chunk().await {
            Ok(Some(bytes)) => buf.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(None) => break,
            Err(e) => {
                let _ = client
                    .inner
                    .lock()
                    .unwrap()
                    .stream_err
                    .send(Some(e.to_string()));
                return;
            }
        }
    }
    if !endpoint_sent {
        let _ = endpoint_tx.send(String::new());
    }
}

// === helpers ===

/// `url.Parse(candidate)` + `ResolveReference(base)` when relative. Uses `reqwest`'s
/// `Url::join`, which implements RFC-3986 reference resolution (Go's
/// `ResolveReference`).
fn resolve_url(candidate: &str, base: &str) -> String {
    match reqwest::Url::parse(candidate) {
        Ok(u) => u.to_string(),
        Err(_) => match reqwest::Url::parse(base) {
            Ok(b) => match b.join(candidate) {
                Ok(j) => j.to_string(),
                Err(_) => candidate.to_string(),
            },
            Err(_) => candidate.to_string(),
        },
    }
}

/// `readSnippet`: read up to 2048 bytes of the body, trimmed.
async fn read_snippet(resp: reqwest::Response) -> String {
    let bytes = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    let take = bytes.len().min(2048);
    String::from_utf8_lossy(&bytes[..take]).trim().to_string()
}

/// Format a reqwest error carrying the URL and cause (Go's `url.Error`); the tests
/// only assert that the call errors, not the exact text.
fn format_http_error(e: &reqwest::Error) -> String {
    let url = e.url().map(|u| u.to_string()).unwrap_or_default();
    if url.is_empty() {
        e.to_string()
    } else {
        format!("{url}: {e}")
    }
}
