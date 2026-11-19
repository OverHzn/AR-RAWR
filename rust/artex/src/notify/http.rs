//! Rust port of `notify/http.go` — the SSRF dial guard, URL validation, and
//! credential redaction shared by every delivery channel.
//!
//! This module is the **pure, testable substrate**: the IP-range classification,
//! the address validator, and the error-redaction helpers. The actual
//! `doJSON`/`httpClient` (network IO, status-code classification, redirect policy)
//! is built on top of this in the channel-adapters unit; keeping the security logic
//! here makes it unit-testable without a socket.
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
