//! Pure traffic helpers — slice of `traffic/traffic.go`.

use crate::db::error::{Error, Result};

/// `clampBlobRange`: clamp a (offset, length) range to a blob of `size` bytes.
/// `length <= 0` means "the rest". Returns (offset, length, empty).
pub fn clamp_blob_range(offset: i64, length: i64, size: i64) -> (i64, i64, bool) {
    let offset = offset.max(0);
    if size <= 0 || offset >= size {
        return (offset, 0, true);
    }
    let remain = size - offset;
    let length = if length <= 0 || length > remain {
        remain
    } else {
        length
    };
    (offset, length, false)
}

/// `hostOnly`: strip an optional :port so passthrough keys match CONNECT and
/// request-URL forms. Bare IPv6 (colons, no port) is returned unchanged.
pub fn host_only(hostport: &str) -> String {
    if hostport.starts_with('[') {
        if let Some(end) = hostport.find(']') {
            if hostport[end + 1..].starts_with(':') {
                return hostport[1..end].to_string();
            }
        }
        return hostport.to_string();
    }
    if let Some((host, port)) = hostport.rsplit_once(':') {
        if !host.is_empty()
            && !host.contains(':')
            && !port.is_empty()
            && port.bytes().all(|c| c.is_ascii_digit())
        {
            return host.to_string();
        }
    }
    hostport.to_string()
}

/// `ProxyAddr`: the URL workers set as HTTP(S)_PROXY. A bare ":port" maps to
/// loopback so the agents reach the local proxy.
pub fn proxy_addr(addr: &str) -> String {
    if let Some(port) = addr.strip_prefix(':') {
        format!("http://127.0.0.1:{port}")
    } else {
        format!("http://{addr}")
    }
}

/// `ValidateProxyURL`: only http/https/socks5 with a host; returns a parsed URL.
pub fn validate_proxy_url(raw: &str) -> Result<url::Url> {
    let raw = raw.trim();
    let u = url::Url::parse(raw)
        .map_err(|e| Error::msg(format!("parse proxy address {raw:?}: {e}")))?;
    match u.scheme() {
        "http" | "https" | "socks5" => {}
        "" => {
            return Err(Error::msg(format!(
                "proxy {raw:?} is missing a scheme (use http://, https://, or socks5://)"
            )))
        }
        other => {
            return Err(Error::msg(format!(
                "unsupported proxy scheme {other:?} (use http, https, or socks5)"
            )))
        }
    }
    if u.host_str().unwrap_or("").is_empty() {
        return Err(Error::msg(format!("proxy {raw:?} is missing a host")));
    }
    Ok(u)
}
