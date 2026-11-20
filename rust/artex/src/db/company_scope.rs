//! Company scope parsers — port of `db/company_scope.go`.
//!
//! Pure classification/normalization of asset-scope lines into domain / IP / CIDR
//! / ICP / keyword rules. Guards reject bare TLDs and over-broad networks so a
//! rule can never swallow the internet. eTLD checks use the vendored PSL.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::db::publicsuffix::public_suffix;

/// `ParsedScope`: one parsed asset-scope entry. The kind selects Domain, Net or Value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedScope {
    /// `"domain" | "ip" | "cidr" | "icp" | "keyword"`.
    pub kind: String,
    /// Normalized root/registrable domain (kind=domain).
    pub domain: String,
    /// Normalized CIDR, single IP as /32 or /128 (kind=ip|cidr).
    pub net: String,
    /// Normalized text (kind=icp|keyword).
    pub value: String,
    /// Original input line.
    pub raw: String,
}

/// `ScopeInput`: structured API form; empty `kind` uses auto classification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeInput {
    pub kind: String,
    pub value: String,
}

fn err(msg: impl Into<String>) -> crate::db::error::Error {
    crate::db::error::Error::msg(msg)
}

/// `NormalizeICP`: strip every Unicode whitespace rune, fold case. No fuzzy or
/// punctuation normalization.
pub fn normalize_icp(value: &str) -> String {
    let stripped: String = value
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    gocompat::strings::go_to_lower(&stripped)
}

fn normalize_keyword(value: &str) -> String {
    gocompat::strings::go_to_lower(&gocompat::strings::go_fields_join(value))
}

/// `looksLikeIPAddress`: heuristic for malformed/partial IP literals (mirrors Go).
fn looks_like_ip_address(value: &str) -> bool {
    let value = value.trim();
    if value.contains("://") || value.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if value.matches(':').count() >= 2 {
        // Require an IPv6-looking prefix (also catches malformed 2001:db8::zz).
        let mut valid_segments = 0;
        for part in value.split(':') {
            if part.is_empty() {
                if valid_segments > 0 || value.starts_with("::") {
                    return true;
                }
                continue;
            }
            if part.len() > 4 {
                return false;
            }
            if !part
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || ('A'..='F').contains(&c))
            {
                return false;
            }
            valid_segments += 1;
            if valid_segments >= 2 {
                return true;
            }
        }
        return false;
    }
    if !value.contains('.') {
        return false;
    }
    value.chars().all(|c| c == '.' || c.is_ascii_digit())
}

/// Canonicalize an IP and report whether it is an IPv4 address (Go `net.IP.String()`
/// + `To4() != nil`).
fn canonical_ip(ip: IpAddr) -> (String, bool) {
    match ip {
        IpAddr::V4(v4) => (v4.to_string(), true),
        IpAddr::V6(v6) => match v6.to_ipv4() {
            Some(v4) => (v4.to_string(), true),
            None => (v6.to_string(), false),
        },
    }
}

/// Parse a CIDR into its canonical `network/prefix` string + address family.
/// Mirrors Go `net.ParseCIDR` + `IPNet.String()` (network address masked).
fn parse_cidr(raw: &str) -> Option<(String, bool, u32)> {
    let (addr, prefix) = raw.rsplit_once('/')?;
    let ip: IpAddr = addr.parse().ok()?;
    let (canon_ip, is_v4) = canonical_ip(ip);
    let (bits, ones) = if is_v4 {
        (32u32, prefix.parse::<u32>().ok()?)
    } else {
        (128u32, prefix.parse::<u32>().ok()?)
    };
    if ones > bits {
        return None;
    }
    let network = if is_v4 {
        let v4: Ipv4Addr = canon_ip.parse().ok()?;
        let mask = if ones == 0 {
            0u32
        } else {
            u32::MAX << (32 - ones)
        };
        Ipv4Addr::from(u32::from(v4) & mask).to_string()
    } else {
        let v6: Ipv6Addr = match ip {
            IpAddr::V6(v) => v,
            IpAddr::V4(_) => return None,
        };
        let mask = if ones == 0 {
            0u128
        } else {
            u128::MAX << (128 - ones)
        };
        Ipv6Addr::from(u128::from(v6) & mask).to_string()
    };
    Some((format!("{network}/{ones}"), is_v4, ones))
}

/// `scopeHostname`: parse a raw scope as a URL/host and normalize it to ASCII,
/// lowercased, without trailing dot; IPs are returned canonicalized.
fn scope_hostname(raw: &str) -> std::result::Result<String, crate::db::error::Error> {
    let mut candidate = raw.trim().to_string();
    if candidate.is_empty() {
        return Err(err("hostname is empty"));
    }
    if candidate.starts_with("//") {
        candidate = format!("http:{candidate}");
    } else if !candidate.contains("://") {
        candidate = format!("http://{candidate}");
    }
    let parsed = url::Url::parse(&candidate).map_err(|_| err("missing hostname"))?;
    let host_str = parsed.host_str().unwrap_or("");
    if host_str.is_empty() {
        return Err(err("missing hostname"));
    }
    let host = host_str.trim().trim_end_matches('.').to_string();
    if host.is_empty() {
        return Err(err("hostname is empty"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(canonical_ip(ip).0);
    }
    let ascii = idna::domain_to_ascii(&host).map_err(|_| err("invalid domain"))?;
    let host = ascii.to_lowercase();
    if host.len() > 253 {
        return Err(err("domain exceeds 253 characters"));
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return Err(err("domain needs at least two labels"));
    }
    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(err("invalid domain label"));
        }
    }
    Ok(host)
}

/// `ParseScopeLine`: classify and validate one typed scope line (root domain /
/// IP / CIDR). Bare public suffixes and over-broad networks are rejected.
pub fn parse_scope_line(line: &str) -> std::result::Result<ParsedScope, crate::db::error::Error> {
    let raw = line.trim().to_string();
    let mut r = ParsedScope {
        raw: raw.clone(),
        ..ParsedScope::default()
    };
    if raw.is_empty() {
        return Err(err("empty line"));
    }
    // CIDR first (URL parsing treats the slash as a path separator).
    if let Some((net, is_v4, ones)) = parse_cidr(&raw) {
        let bits = if is_v4 { 32 } else { 128 };
        let min = if is_v4 { 16 } else { 32 };
        if ones < min {
            return Err(err(format!(
                "network too broad (IPv{} requires >= /{}): {}",
                if is_v4 { 4 } else { 6 },
                min,
                raw
            )));
        }
        let _ = bits;
        r.kind = "cidr".to_string();
        r.net = net;
        return Ok(r);
    }
    // Single IP.
    if let Ok(ip) = raw.parse::<IpAddr>() {
        let (canon, is_v4) = canonical_ip(ip);
        r.kind = "ip".to_string();
        r.net = format!("{canon}/{}", if is_v4 { 32 } else { 128 });
        return Ok(r);
    }
    let host = scope_hostname(&raw)
        .map_err(|_| err(format!("not recognizable as a valid domain/IP/CIDR: {raw}")))?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        let (canon, is_v4) = canonical_ip(ip);
        r.kind = "ip".to_string();
        r.net = format!("{canon}/{}", if is_v4 { 32 } else { 128 });
        return Ok(r);
    }
    if looks_like_ip_address(&host) {
        return Err(err(format!("invalid IP: {raw}")));
    }
    if raw.contains('-') && raw.matches('.').count() >= 6 {
        return Err(err(format!(
            "express IP ranges as CIDR (e.g. 1.2.3.0/24): {raw}"
        )));
    }
    // Domain (registrable). Reject bare TLDs / public suffixes.
    let d = db_rs::port::domain_key(host).unwrap_or_default();
    let (suffix, icann) = public_suffix(&d);
    if icann && suffix == d {
        return Err(err(format!("a bare TLD cannot be used as a scope: {raw}")));
    }
    r.kind = "domain".to_string();
    r.domain = d;
    Ok(r)
}

/// `ParseAutoScopeLine`: classify one untyped textarea line. Network-looking and
/// domain-looking values stay strict; all other non-empty text is a keyword.
pub fn parse_auto_scope_line(
    line: &str,
) -> std::result::Result<ParsedScope, crate::db::error::Error> {
    let raw = line.trim();
    if raw.is_empty() {
        return Err(err("empty line"));
    }
    if parse_cidr(raw).is_some() {
        return parse_scope_line(raw);
    }
    if raw.parse::<IpAddr>().is_ok() {
        return parse_scope_line(raw);
    }
    if let Some((slash, _)) = raw.rsplit_once('/') {
        let address = slash.trim();
        if address.parse::<IpAddr>().is_ok() || looks_like_ip_address(address) {
            return Err(err(format!("invalid CIDR: {raw}")));
        }
    }
    if looks_like_ip_address(raw) {
        return Err(err(format!("invalid IP: {raw}")));
    }
    let looks_like_domain =
        raw.contains("://") || (raw.contains('.') && !raw.chars().any(|c| c.is_whitespace()));
    if looks_like_domain {
        return parse_scope_line(raw);
    }
    // ICP filing numbers contain no dot; text with a dot is a keyword.
    let lower = gocompat::strings::go_to_lower(raw);
    if !raw.contains(['.', '\u{ff0e}', '\u{3002}'])
        && (lower.contains("icp") || lower.contains("filing"))
    {
        return parse_scope_input(ScopeInput {
            kind: "icp".to_string(),
            value: raw.to_string(),
        });
    }
    parse_scope_input(ScopeInput {
        kind: "keyword".to_string(),
        value: raw.to_string(),
    })
}

/// `ParseScopeInput`: validate an explicitly typed rule; empty kind uses auto.
pub fn parse_scope_input(
    input: ScopeInput,
) -> std::result::Result<ParsedScope, crate::db::error::Error> {
    let kind = gocompat::strings::go_to_lower(input.kind.trim());
    let raw = input.value.trim();
    if kind.is_empty() {
        return parse_auto_scope_line(raw);
    }
    match kind.as_str() {
        "domain" | "ip" | "cidr" => {
            let rule = parse_scope_line(raw)?;
            if rule.kind != kind {
                return Err(err(format!("{raw:?} is not a valid {kind} scope")));
            }
            Ok(rule)
        }
        "icp" => {
            let value = normalize_icp(raw);
            if value.is_empty() {
                return Err(err("ICP must not be empty"));
            }
            Ok(ParsedScope {
                kind,
                value,
                raw: raw.to_string(),
                ..ParsedScope::default()
            })
        }
        "keyword" => {
            let value = normalize_keyword(raw);
            if value.is_empty() {
                return Err(err("company keyword must not be empty"));
            }
            Ok(ParsedScope {
                kind,
                value,
                raw: raw.to_string(),
                ..ParsedScope::default()
            })
        }
        _ => Err(err(format!("unsupported scope kind: {kind}"))),
    }
}
