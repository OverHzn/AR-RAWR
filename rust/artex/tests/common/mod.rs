//! Shared test harness for the `artex::notify` adapter tests.
//!
//! A tiny scriptable HTTP/1.1 server (tokio) plus helpers. The Go channel tests use
//! `httptest.NewServer`; this is the Rust equivalent — a real socket on 127.0.0.1:0 so
//! the SSRF dial guard and redirect policy are exercised exactly as in production.
//!
//! The handlers need 127.0.0.1 to be reachable, which the SSRF guard denies by default;
//! every test therefore holds [`lock_env`] and sets `ARTEX_NOTIFY_ALLOW_LOCAL` itself
//! (Rust runs a test file's cases in parallel threads of one process, so the process-wide
//! env var must be serialised — Go did the same globally in `TestMain`).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One parsed inbound request, recorded for assertions.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    /// path + query, e.g. `/robot/send?access_token=x`.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn body_string(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A scripted response.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }
    pub fn json(status: u16, body: &str) -> Self {
        Response {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
        }
    }
    pub fn redirect(status: u16, location: String) -> Self {
        Response {
            status,
            headers: vec![("Location".into(), location)],
            body: Vec::new(),
        }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }
}

type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

/// A scriptable HTTP server on an ephemeral 127.0.0.1 port.
pub struct TestServer {
    pub addr: SocketAddr,
    pub url: String,
    records: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Start a server whose every request is answered by `handler`.
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handler: Handler = Arc::new(handler);
        let records: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let rec2 = records.clone();
        let task = tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let handler = handler.clone();
                let records = rec2.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(sock, handler, records).await;
                });
            }
        });
        TestServer {
            addr,
            url: format!("http://{addr}"),
            records,
            task,
        }
    }

    /// A URL for `path` on this server.
    pub fn url_for(&self, path: &str) -> String {
        format!("{}{}", self.url, path)
    }

    /// Snapshot of every request received so far.
    pub fn records(&self) -> Vec<Request> {
        self.records.lock().unwrap().clone()
    }

    pub fn hit_count(&self) -> usize {
        self.records.lock().unwrap().len()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_conn(
    mut sock: TcpStream,
    handler: Handler,
    records: Arc<Mutex<Vec<Request>>>,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 1024];

    // Read until the end of the header block.
    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
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

    let req = Request {
        method,
        path,
        headers,
        body,
    };
    records.lock().unwrap().push(req.clone());
    let resp = handler(req);

    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    out.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    out.push_str("Connection: close\r\n");
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    sock.write_all(out.as_bytes()).await?;
    sock.write_all(&resp.body).await?;
    sock.flush().await?;
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        418 => "I'm a teapot",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Serialise tests that depend on the process-wide `ARTEX_NOTIFY_ALLOW_LOCAL` env var.
/// Acquire this at the top of any test that touches a local server or toggles the flag.
pub async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}
