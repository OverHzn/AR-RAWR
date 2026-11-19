//! Rust port of `notify/http.go` — the SSRF dial guard, URL validation, credential
//! redaction, and the `doJSON`/`httpClient` delivery engine shared by every channel.
//!
//! The module has two layers:
//! - a **pure, socket-free substrate** — the IP-range classification
//!   ([`is_blocked_dial_ip`]), the address validator ([`validate_http_url`]), and the
//!   error-redaction helpers ([`redact_request_target`]/[`redact_transport_error`]/
//!   [`redact_urls_in_text`]) — unit-tested directly, and
//! - the **network IO shell** ([`do_json`] + the process-wide [`http_client`] lazily
//!   built in [`http_client`]): status-code classification, the cross-host redirect
//!   policy, and the SSRF dial guard (literal-IP pre-check + the [`GuardResolver`] that
//!   vets hostname lookups, closing DNS-rebinding).
//!
//! Channel adapters never build their own client; they call [`do_json`].
//!
//! ## What is blocked and why
//! Deny-by-default for loopback, link-local (incl. the cloud metadata endpoint
//! `169.254.169.254`), unspecified, and multicast addresses — these are not where
//! IM bots or public mail servers live, and what they reach is sensitive. RFC1918
//! private networks are **deliberately allowed**: self-hosted Mattermost / SMTP
//! relays on an intranet are common legitimate uses, and blocking them would make
//! the feature unusable. The escape hatch is `ARTEX_NOTIFY_ALLOW_LOCAL=1`.

use std::net::IpAddr;

use gocompat::neturl::{parse_url, Url};

/// `AllowLocalTargetsEnv` (`notify/http.go`): the env var that re-enables delivery
/// to loopback / link-local addresses (a local SMTP relay is legitimate).
pub const ALLOW_LOCAL_TARGETS_ENV: &str = "ARTEX_NOTIFY_ALLOW_LOCAL";

/// `allowLocalTargets()`: whether delivering to loopback/link-local is allowed.
/// Truthy when the (trimmed) env var is `"1"` or `"true"` (case-insensitive).
pub fn allow_local_targets() -> bool {
    match std::env::var(ALLOW_LOCAL_TARGETS_ENV) {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

/// `isBlockedDialIP` (`notify/http.go`): whether `ip` is in a range "not allowed for
/// delivery by default" — loopback, link-local (unicast/multicast), interface-local
/// multicast, unspecified, or multicast. RFC1918 private addresses are **not**
/// blocked.
///
/// Go's `net.IP.To4()` converts an IPv4-mapped IPv6 address back to IPv4 before the
/// check; std `IpAddr` keeps them distinct, so [`to_ipv4`] does the same mapping
/// (only for the canonical `::ffff:a.b.c.d` form — NOT the deprecated `::a.b.c.d`,
/// which would misclassify `::1`).
///
/// Go's extra `if ip == nil { return true }` branch has no Rust equivalent: a
/// `net.IP` is nil when the address could not be parsed at all (e.g. `ParseIP("")`),
/// whereas `IpAddr` is non-nullable by construction. The equivalent outcome is
/// preserved at the call sites — [`validate_http_url`] only checks *literal* IPs
/// (`if let Some(ip) = parse_ip(..)`, mirroring Go's `if ip := ParseIP(host); ip != nil`),
/// and the dial guard (a later unit) always holds a resolved socket address.
pub fn is_blocked_dial_ip(ip: IpAddr) -> bool {
    let ip = to_ipv4(ip).unwrap_or(ip);
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() // 127.0.0.0/8
                || v4.is_unspecified() // 0.0.0.0
                || v4.is_multicast() // 224.0.0.0/4
                || (a == 169 && b == 254) // link-local 169.254.0.0/16
        }
        IpAddr::V6(v6) => {
            // Go checks IsLoopback, IsLinkLocalUnicast, IsLinkLocalMulticast,
            // IsInterfaceLocalMulticast, IsUnspecified, IsMulticast. For IPv6 the
            // ff00::/8 multicast test subsumes both ff01::/16 (interface-local) and
            // ff02::/16 (link-local), so three checks suffice — std has no stable
            // predicate for link-local-unicast fe80::/10, so that one is masked.
            let seg = v6.segments();
            v6.is_loopback() // ::1
                || v6.is_unspecified() // ::
                || (seg[0] & 0xffc0) == 0xfe80 // link-local unicast fe80::/10
                || (seg[0] & 0xff00) == 0xff00 // multicast ff00::/8 (covers ff01/ff02)
        }
    }
}

/// Go `net.IP.To4()` subset: convert an IPv4-mapped IPv6 (`::ffff:a.b.c.d`) to its
/// IPv4 form. Only the canonical mapped prefix is recognised — the deprecated
/// IPv4-compatible `::a.b.c.d` is intentionally left as IPv6 (matching modern Go,
/// where `::1` is loopback, not `0.0.0.1`).
fn to_ipv4(ip: IpAddr) -> Option<IpAddr> {
    match ip {
        IpAddr::V4(_) => Some(ip),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4),
    }
}

/// `validateHTTPURL` (`notify/dingtalk.go`): check that an address is usable, the
/// scheme is http/https, and — for a literal IP — that it is not an internal range
/// (unless opted in). Domains are left to the dial phase (`is_blocked_dial_ip` is the
/// final enforcement and also covers DNS rebinding).
///
/// Error messages are **redacted**: `url.Parse` errors carry the full original
/// address, and these platforms' addresses embed credentials.
pub fn validate_http_url(raw: &str) -> Result<(), String> {
    let u: Url = parse_url(raw)
        .map_err(|_| format!("address cannot be parsed ({})", redact_request_target(raw)))?;
    if u.scheme != "http" && u.scheme != "https" {
        return Err(format!("only http/https are supported, got {:?}", u.scheme));
    }
    if u.host.is_empty() {
        return Err("missing host name".to_string());
    }
    if let Some(ip) = parse_ip(&u.hostname()) {
        if is_blocked_dial_ip(ip) && !allow_local_targets() {
            return Err(format!(
                "refusing to deliver to loopback/link-local address {ip} \
                 (if you really need to deliver to a local service, set {ALLOW_LOCAL_TARGETS_ENV}=1)"
            ));
        }
    }
    Ok(())
}

/// Parse an IP literal the way Go's `net.ParseIP` does for the cases the guard
/// sees: standard IPv4 dotted-quad or IPv6 (including a bracketed `[..]` host, which
/// `hostname()` has already stripped). Returns `None` for a non-IP host (a domain).
fn parse_ip(host: &str) -> Option<IpAddr> {
    host.parse::<IpAddr>().ok()
}

/// `redactRequestTarget` (`notify/http.go`): compress a delivery address to
/// `scheme://host/…` for error messages. Everything except scheme and host is
/// dropped, because there is no generic, safe way to tell which part of a URL is the
/// credential (DingTalk/WeCom in the query, Feishu in the last path segment, Telegram
/// mid-path). On parse failure a fixed placeholder is returned — never the raw string.
pub fn redact_request_target(raw: &str) -> String {
    match parse_url(raw) {
        Ok(u) if !u.host.is_empty() => format!("{}://{}/\u{2026}", u.scheme, u.host),
        _ => "(unparseable address)".to_string(),
    }
}

/// A transport-layer error, decomposed the way Go's `*url.Error` is: the operation,
/// the target URL (still carrying the credential), and the underlying cause. The
/// caller builds this from the real error; [`redact_transport_error`] strips the URL.
#[derive(Debug, Clone)]
pub struct TransportError {
    /// The HTTP operation (`Get`/`Post`/…).
    pub op: String,
    /// The full URL the request targeted (may contain credentials).
    pub url: String,
    /// The underlying cause (e.g. `dial tcp …: connect: connection refused`).
    pub cause: String,
}

/// `redactTransportError` (`notify/http.go`): keep the operation, host, and
/// underlying cause, but strip the URL (and thus the credential). For a structured
/// [`TransportError`] (the `*url.Error` analogue) the host is parsed out of the URL;
/// the cause is kept verbatim.
pub fn redact_transport_error(err: &TransportError) -> String {
    let host = parse_url(&err.url).ok().map(|u| u.host).unwrap_or_default();
    format!("{} {}: {}", err.op, host, err.cause)
}

/// `redactURLsInText` (`notify/http.go`): replace every `http(s)://…` address in a
/// piece of text with its redacted form. A fallback for errors whose structured
/// fields are unavailable (redirect-policy errors, third-party errors). Addresses are
/// split on whitespace and quotes — they never contain either.
pub fn redact_urls_in_text(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &s[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest
                .find([' ', '\t', '\n', '"', '\''])
                .unwrap_or(rest.len());
            out.push_str(&redact_request_target(&rest[..end]));
            i += end;
            continue;
        }
        // Push one full UTF-8 char (advance by its byte length).
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ===========================================================================
// doJSON + httpClient: the network IO shell (notify/http.go)
// ===========================================================================
//
// This is the single HTTP entry point every channel adapter uses. It layers three
// hardenings on top of a stock client, all ported from Go's `notify/http.go`:
//
//   ① SSRF dial guard. Literal IPs are checked here (hyper-util connects to an IP
//      literal without consulting the resolver, so a resolver hook alone would miss
//      them); hostnames are vetted in [`GuardResolver`] at resolve time, which also
//      closes DNS-rebinding (reqwest connects to exactly the addresses it returns).
//   ② Cross-host redirect refusal. The credentials of these platforms live in the URL,
//      so following a redirect to another host hands them over; same-host redirects
//      (e.g. a trailing slash) are allowed.
//   ③ Credential redaction. Every transport-layer error string is run through
//      [`redact_urls_in_text`] so the access_token / bot token never reaches
//      `last_error`, the delivery-history API, logs, or the test-send 502.
//
// The connect timeout is 15s — a peer slower than that is effectively down. This
// client deliberately does **not** use the project's egress/GlobalProxy: IM pushes go
// direct, not through the (possibly unstable) pentest target tunnel.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;

use super::{Context, Error};

/// `respBodyLimit` (`notify/http.go`): cap on how much of a response body is read.
/// An abnormal peer may return oversized content, while only the status and a short
/// error description are ever needed.
const RESP_BODY_LIMIT: usize = 8 << 10;

/// `httpClient` (`notify/http.go`): the process-wide client shared by all deliveries.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(redirect_policy())
            .dns_resolver(Arc::new(GuardResolver))
            .build()
            .expect("building the notification HTTP client")
    })
}

/// Go's `httpClient.CheckRedirect`: refuse cross-host redirects, allow same-host,
/// cap the chain at five hops. `previous()` is Go's `via` (it includes the original
/// request); `url()` is the next request. `URL.Host` in Go includes the port, so the
/// comparison is host **and** port.
fn redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects");
        }
        // Build the message inside an immutable-borrow scope so the `String` is owned
        // before `attempt.error(..)` consumes `attempt`.
        let cross_host = match attempt.previous().first() {
            Some(first) => {
                let next = attempt.url();
                if first.host_str() != next.host_str()
                    || first.port_or_known_default() != next.port_or_known_default()
                {
                    Some(format!(
                        "refusing cross-host redirect ({} \u{2192} {})",
                        host_port(first),
                        host_port(next)
                    ))
                } else {
                    None
                }
            }
            None => None,
        };
        if let Some(msg) = cross_host {
            return attempt.error(msg);
        }
        attempt.follow()
    })
}

/// `host:port` for a redirect error message (Go's `URL.Host`, port included).
fn host_port(u: &reqwest::Url) -> String {
    match (u.host_str(), u.port_or_known_default()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
}

/// The dial-guard resolver: every hostname lookup is filtered so a name that resolves
/// to loopback / link-local / metadata is refused (unless `ARTEX_NOTIFY_ALLOW_LOCAL`
/// opts in). Because reqwest connects to exactly the addresses returned here, a name
/// that flips to an internal IP between validation and connection (DNS rebinding)
/// cannot slip through.
struct GuardResolver;

impl Resolve for GuardResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            if allow_local_targets() {
                return Ok(Box::new(resolved.into_iter()) as Addrs);
            }
            let safe: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|a| !is_blocked_dial_ip(a.ip()))
                .collect();
            if safe.is_empty() {
                // Every resolved address is internal: refuse with the opt-in hint.
                return Err(format!(
                    "host {host} resolves to a loopback/link-local address; \
                     if you really need to deliver to a local service, set \
                     {ALLOW_LOCAL_TARGETS_ENV}=1"
                )
                .into());
            }
            Ok(Box::new(safe.into_iter()) as Addrs)
        })
    }
}

/// `doJSON` (`notify/http.go`): send one request and return the (length-capped)
/// response body.
///
/// `payload` of `None` sends an empty body (GET, or platforms that need none); a
/// `Some` value is JSON-encoded and sent with a JSON content type. `headers` are
/// appended verbatim (for the generic webhook's custom headers).
///
/// Error classification is the core job: network-layer failures and 5xx / 408 / 429
/// are retryable (returned as [`Error::Msg`]); the remaining 4xx are permanent
/// configuration/permission problems ([`Error::Permanent`]) — retrying a 403 just logs
/// the same error three times.
pub async fn do_json(
    ctx: &Context,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    payload: Option<&serde_json::Value>,
) -> Result<Vec<u8>, Error> {
    if ctx.is_cancelled() {
        return Err(Error::msg("context canceled"));
    }

    let parsed = gocompat::neturl::parse_url(url).map_err(|_| {
        // url.Parse's error text carries the full address; never pass it through.
        Error::permanent(format!(
            "invalid request URL: {}",
            redact_request_target(url)
        ))
    })?;

    // ① Literal-IP SSRF guard. hyper-util connects to an IP literal without calling
    //    the resolver, so literal internal targets must be caught here.
    if let Some(ip) = parse_ip(&parsed.hostname()) {
        if is_blocked_dial_ip(ip) && !allow_local_targets() {
            return Err(Error::transient(format!(
                "request failed: {} {}: {}",
                title_op(method),
                parsed.host,
                blocked_ip_message(ip)
            )));
        }
    }

    let body = match payload {
        Some(v) => Some(
            serde_json::to_vec(v)
                .map_err(|e| Error::permanent(format!("failed to build request body: {e}")))?,
        ),
        None => None,
    };

    let method_parsed = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| Error::permanent(format!("invalid request method: {method}")))?;
    let mut req = http_client().request(method_parsed, url);
    if let Some(b) = &body {
        req = req
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )
            .body(b.clone());
    }
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let mut resp = req.send().await.map_err(|e| {
        // `host:port` mirrors Go's `url.Error{URL}.Host`; read it before
        // `without_url()` drops the URL from the error below.
        let host = e.url().map(host_port).unwrap_or_default();
        let cause = redact_reqwest_error(e);
        Error::transient(format!(
            "request failed: {} {}: {}",
            title_op(method),
            host,
            cause
        ))
    })?;
    let status = resp.status().as_u16();

    // Read the body length-capped: an entry in the delivery history stores a copy of
    // `last_error`, so an oversized peer response must not be fully buffered.
    let mut raw: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = RESP_BODY_LIMIT.saturating_sub(raw.len());
                if chunk.len() > remaining {
                    raw.extend_from_slice(&chunk[..remaining]);
                    break;
                }
                raw.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            // Unlike Go's `resp.Body.Read` (a bare `io` error), reqwest wraps body
            // errors in a `reqwest::Error` that may embed the request URL, so redact.
            Err(e) => {
                return Err(Error::msg(format!(
                    "failed to read response: {}",
                    redact_reqwest_error(e)
                )))
            }
        }
    }

    if (200..300).contains(&status) {
        return Ok(raw);
    }
    let snip = snippet(&String::from_utf8_lossy(&raw));
    // 429 (rate limit) and 408 (timeout) are worth retrying; the rest of 4xx are not.
    if status == 429 || status == 408 {
        return Err(Error::transient(format!(
            "peer rate limit or timeout (HTTP {status}): {snip}"
        )));
    }
    if status >= 500 {
        return Err(Error::transient(format!(
            "peer server error (HTTP {status}): {snip}"
        )));
    }
    Err(Error::permanent(format!(
        "peer rejected the request (HTTP {status}): {snip}"
    )))
}

/// The opt-in hint shared by both dial-guard paths (literal-IP pre-check + resolver).
fn blocked_ip_message(ip: std::net::IpAddr) -> String {
    format!(
        "refusing to deliver to loopback/link-local address {ip} \
         (if you really need to deliver to a local service, set {ALLOW_LOCAL_TARGETS_ENV}=1)"
    )
}

/// `snippet` (`notify/http.go`): a one-line, 200-rune preview of a response body.
fn snippet(raw: &str) -> String {
    notifyx_rs::port::snippet(raw)
}

/// Strip a `reqwest::Error` to a redacted cause string, the analogue of Go's
/// `redactTransportError(err)` for the non-`*url.Error` case.
///
/// reqwest puts the full request URL — which for these platforms **contains the
/// credential** (DingTalk `access_token`, Feishu hook id, Telegram bot token) — in the
/// error's `Display` as `… for url (https://host/secret)`. [`reqwest::Error::without_url`]
/// drops that field (exactly like taking Go's `url.Error.Err` instead of its `Error()`),
/// and [`redact_urls_in_text`] then scrubs any URL a deeper source still carries, so no
/// credential reaches `last_error` / logs / the test-send 502.
fn redact_reqwest_error(e: reqwest::Error) -> String {
    redact_urls_in_text(&error_chain_string(&e.without_url()))
}

/// Flatten an error and its `source()` chain into one string (reqwest hides the real
/// cause — e.g. the redirect-policy message — in the source chain).
fn error_chain_string(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(c) = src {
        s.push_str(": ");
        s.push_str(&c.to_string());
        src = c.source();
    }
    s
}

/// Go's `url.Error.Op` is the title-cased method (`GET` → `Get`, `POST` → `Post`).
fn title_op(method: &str) -> String {
    let mut chars = method.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}
