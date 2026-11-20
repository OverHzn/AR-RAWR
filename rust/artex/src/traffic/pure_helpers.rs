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

// Body/index helpers from traffic/traffic.go.

const BINARY_TYPES: &[&str] = &[
    "image/",
    "audio/",
    "video/",
    "font/",
    "application/octet-stream",
    "application/zip",
    "application/gzip",
    "application/x-gzip",
    "application/x-tar",
    "application/x-7z-compressed",
    "application/x-rar",
    "application/pdf",
    "application/x-msdownload",
    "application/vnd.android.package-archive",
    "application/java-archive",
    "application/wasm",
    "application/x-shockwave-flash",
];

fn clip_bytes(b: &[u8], n: usize) -> &[u8] {
    &b[..b.len().min(n)]
}

/// `isBinaryBody`: binary by content type, or a NUL in the first 512 bytes.
pub fn is_binary_body(content_type: &str, body: &[u8]) -> bool {
    let ct = content_type.trim().to_lowercase();
    let ct = ct.split(';').next().unwrap_or("").trim();
    if BINARY_TYPES.iter().any(|p| ct.starts_with(p)) {
        return true;
    }
    clip_bytes(body, 512).contains(&0)
}

/// `binaryTag`: one-line description with leading magic bytes.
pub fn binary_tag(content_type: &str, body: &[u8]) -> String {
    let ct = content_type.trim();
    let ct = if ct.is_empty() {
        "application/octet-stream"
    } else {
        ct
    };
    let magic: String = clip_bytes(body, 4)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("[binary {ct}, {} bytes, magic={magic}]", body.len())
}

/// `truncateUTF8`: cut to at most n bytes without splitting a rune.
pub fn truncate_utf8(b: &[u8], n: usize) -> String {
    if b.len() <= n {
        return String::from_utf8_lossy(b).into_owned();
    }
    let mut cut = &b[..n];
    for _ in 0..3 {
        if cut.is_empty() {
            break;
        }
        if let Ok(s) = std::str::from_utf8(cut) {
            return s.to_string();
        }
        // Back off until the tail forms a complete rune.
        let mut end = cut.len();
        while end > 0 && std::str::from_utf8(&cut[..end]).is_err() {
            end -= 1;
        }
        if end > 0 {
            return String::from_utf8_lossy(&cut[..end]).into_owned();
        }
        cut = &cut[..cut.len() - 1];
    }
    String::from_utf8_lossy(cut).into_owned()
}

/// `sanitize`: filesystem-safe name (path separators/specials -> '_', "root").
pub fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '?' | '*' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if out.is_empty() || out == "_" {
        return "root".to_string();
    }
    if out.len() > 120 {
        out.truncate(120);
    }
    out
}

/// `headerLines`: flat "Name: value\n" rendering of a header map.
pub fn header_lines(header: &std::collections::BTreeMap<String, Vec<String>>) -> String {
    let mut out = String::new();
    for (k, vs) in header {
        for v in vs {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push('\n');
        }
    }
    out
}

/// `appliedRules`: (was rewritten, distinct applied rule ids in order).
pub fn applied_rules(lines: &[crate::traffic::rewrite_engine::TraceLine]) -> (bool, Vec<i64>) {
    let mut ids = Vec::new();
    let mut rewritten = false;
    for l in lines {
        if l.outcome != crate::traffic::rewrite_engine::OUTCOME_APPLIED {
            continue;
        }
        rewritten = true;
        if l.rule_id != 0 && !ids.contains(&l.rule_id) {
            ids.push(l.rule_id);
        }
    }
    (rewritten, ids)
}
