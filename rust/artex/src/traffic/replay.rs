//! Replay validation — pure slice of `traffic/replay.go` (the HTTP dial itself
//! is deferred; all request sanitization happens before any socket work).

use crate::db::error::{Error, Result};
use crate::traffic::pure_helpers::host_only;
use crate::traffic::rewrite_rule::canonical_header_key;

pub const MAX_REPLAY_BODY: usize = 8 * 1024 * 1024;
pub const MAX_REPLAY_HEADERS: usize = 200;
pub const ERR_REPLAY_INVALID: &str = "invalid replay request";
pub const ERR_REPLAY_TOO_LARGE: &str = "replay request body too large";

const REPLAY_DROPPED: &[&str] = &[
    "Content-Length",
    "Transfer-Encoding",
    "Connection",
    "Proxy-Connection",
    "Proxy-Authorization",
    "Keep-Alive",
    "Te",
    "Trailer",
    "Expect",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplayHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReplayRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<ReplayHeader>,
    pub body: Vec<u8>,
}

/// A validated request: canonical headers (hop-by-hop removed), effective Host,
/// and the absolute URL.
#[derive(Debug, Clone)]
pub struct ValidatedReplay {
    pub method: String,
    pub url: String,
    pub host: String,
    pub headers: Vec<ReplayHeader>,
    pub body: Vec<u8>,
}

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::sentinel(ERR_REPLAY_INVALID, format!("{ERR_REPLAY_INVALID}: {msg}"))
}

/// `validateReplayRequest`: every rejection the Go `Replay` performs before
/// dialing, plus the header filtering it applies.
pub fn validate_replay_request(req: &ReplayRequest) -> Result<ValidatedReplay> {
    let method = req.method.trim();
    if method.is_empty() || method.eq_ignore_ascii_case("CONNECT") {
        return Err(invalid("method must be set and cannot be CONNECT"));
    }
    if !crate::traffic::query_helpers::valid_header_name(method) {
        return Err(invalid(format!("method {method:?} is not a valid token")));
    }
    let raw = req.url.trim();
    // The url crate normalizes "http:///x" to "http://x/"; Go rejects it as
    // having no host. Refuse an empty authority before parsing.
    if let Some(rest) = raw.split_once("://").map(|(_, r)| r) {
        if rest.starts_with('/') {
            return Err(invalid("url has no host"));
        }
    }
    let u = url::Url::parse(raw).map_err(|e| invalid(format!("url: {e}")))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(invalid("url must be absolute http or https"));
    }
    if u.host_str().unwrap_or("").is_empty() {
        return Err(invalid("url has no host"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(invalid(
            "url must not carry user info; send an Authorization header instead",
        ));
    }
    if req.body.len() > MAX_REPLAY_BODY {
        return Err(Error::sentinel(
            ERR_REPLAY_TOO_LARGE,
            format!(
                "{ERR_REPLAY_TOO_LARGE}: body is {} bytes, over the {MAX_REPLAY_BODY}-byte limit",
                req.body.len()
            ),
        ));
    }
    if req.headers.len() > MAX_REPLAY_HEADERS {
        return Err(invalid(format!(
            "{} headers, over the limit of {MAX_REPLAY_HEADERS}",
            req.headers.len()
        )));
    }

    // Headers named in Connection are hop-by-hop too.
    let mut connection_named = std::collections::HashSet::new();
    for h in &req.headers {
        if canonical_header_key(h.name.trim()) != "Connection" {
            continue;
        }
        for token in h.value.split(',') {
            let token = token.trim();
            if !token.is_empty() {
                connection_named.insert(canonical_header_key(token));
            }
        }
    }

    let mut headers = Vec::new();
    let mut host = String::new();
    for h in &req.headers {
        let name = h.name.trim();
        if !crate::traffic::query_helpers::valid_header_name(name) {
            return Err(invalid(format!("invalid header name {:?}", h.name)));
        }
        if h.value.contains(['\r', '\n', '\0']) {
            return Err(invalid(format!("header {name} contains a line break")));
        }
        let canonical = canonical_header_key(name);
        match canonical.as_str() {
            "Upgrade" => return Err(invalid("Upgrade requests cannot be replayed")),
            "Host" => {
                host = h.value.trim().to_string();
                if !host.eq_ignore_ascii_case(u.host_str().unwrap_or("")) {
                    return Err(invalid(format!(
                        "Host {host:?} differs from the URL host {:?}; the replay would contact or record a different host than the URL says, so edit the URL instead",
                        u.host_str().unwrap_or("")
                    )));
                }
            }
            _ if REPLAY_DROPPED.contains(&canonical.as_str())
                || connection_named.contains(&canonical) => {}
            _ => headers.push(ReplayHeader {
                name: canonical,
                value: h.value.clone(),
            }),
        }
    }
    if !headers.iter().any(|h| h.name == "User-Agent") {
        // An empty value prevents net/http from adding its default UA.
        headers.push(ReplayHeader {
            name: "User-Agent".to_string(),
            value: String::new(),
        });
    }
    Ok(ValidatedReplay {
        method: method.to_string(),
        url: u.to_string(),
        host,
        headers,
        body: req.body.clone(),
    })
}

/// `replayRecorded`: plain http is always recorded; https is recorded unless
/// the host is currently in passthrough (tunneled without interception).
pub fn replay_recorded(scheme: &str, host: &str, is_tunneled: impl Fn(&str) -> bool) -> bool {
    if scheme != "https" {
        return true;
    }
    !is_tunneled(host_only(host).as_str())
}
