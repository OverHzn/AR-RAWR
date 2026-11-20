//! Pure query/validation helpers — slice of `traffic/traffic.go` + `replay.go`.

use crate::db::error::{Error, Result};

pub const MIN_TRIGRAM: usize = 3;

/// `ftsQuote`: quote a term as an FTS5 string literal (punctuation and query
/// operators are matched literally).
pub fn fts_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `ftsFilter`: the MATCH predicate when the term has at least 3 runes.
pub fn fts_filter(fts_enabled: bool, term: &str) -> Option<(String, String)> {
    if !fts_enabled || term.trim().chars().count() < MIN_TRIGRAM {
        return None;
    }
    Some((
        "rowid IN (SELECT rowid FROM ex_fts WHERE ex_fts MATCH ?)".to_string(),
        fts_quote(term.trim()),
    ))
}

/// `statusFilter`: exact code ("404") or class bucket ("4xx").
pub fn status_filter(s: &str) -> Option<(&'static str, Vec<i32>)> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    if b.len() == 3 && (b'1'..=b'5').contains(&b[0]) && b[1] == b'x' && b[2] == b'x' {
        let base = (b[0] - b'0') as i32 * 100;
        return Some(("status>=? AND status<?", vec![base, base + 100]));
    }
    s.parse::<i32>().ok().map(|n| ("status=?", vec![n]))
}

/// Whitelisted sort columns (caller input can never reach SQL verbatim).
pub fn sort_column(s: &str) -> &'static str {
    match s {
        "status" => "status",
        "resp_len" => "resp_len",
        _ => "ts",
    }
}

/// `normalizeSearchHost`: (host, port) from a URL, host:port, or bare/[ipv6] host.
pub fn normalize_search_host(raw: &str) -> Result<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::msg("host is a required parameter"));
    }
    let (mut host, mut port) = if raw.contains("://") {
        let u =
            url::Url::parse(raw).map_err(|_| Error::msg(format!("cannot parse host: {raw:?}")))?;
        if u.host_str().unwrap_or("").is_empty() {
            return Err(Error::msg(format!("cannot parse host: {raw:?}")));
        }
        (
            u.host_str().unwrap_or("").to_string(),
            u.port().map(|p| p.to_string()).unwrap_or_default(),
        )
    } else if raw.starts_with('[') {
        // Bracketed IPv6, with or without a trailing :port.
        if let Some(end) = raw.find(']') {
            let h = raw[1..end].to_string();
            let p = raw[end + 1..].strip_prefix(':').unwrap_or("").to_string();
            (h, p)
        } else {
            (raw.to_string(), String::new())
        }
    } else if let Some((h, p)) = raw.rsplit_once(':') {
        if h.contains(':') {
            (raw.to_string(), String::new())
        } else {
            (h.to_string(), p.to_string())
        }
    } else {
        (raw.to_string(), String::new())
    };
    host = host.trim().trim_matches(['[', ']']).to_string();
    if host.is_empty() {
        return Err(Error::msg(format!("cannot parse host: {raw:?}")));
    }
    if !port.is_empty() {
        let n = port
            .parse::<i32>()
            .map_err(|_| Error::msg(format!("invalid port: {port:?}")))?;
        if !(1..=65535).contains(&n) {
            return Err(Error::msg(format!("invalid port: {port:?}")));
        }
        port = n.to_string();
    }
    Ok((host.to_lowercase(), port))
}

/// `proxyCausedErr`: gateway/proxy transport noise that is not a target failure.
pub fn proxy_caused_err(message: &str) -> bool {
    let s = message.to_lowercase();
    [
        "head request",
        "http2",
        "http/2",
        "protocol error",
        "protocol_error",
        "malformed",
    ]
    .iter()
    .any(|p| s.contains(p))
}

/// RFC 9110 token check (header name).
pub fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

/// `newExchangeID` (pure part): unix seconds + rolling 4-digit sequence.
pub fn exchange_id(now_secs: i64, seq: u64) -> String {
    format!("{now_secs}-{:04}", seq % 10000)
}
