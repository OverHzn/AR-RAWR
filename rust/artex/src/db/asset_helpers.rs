//! Pure asset helpers — slice of `db/assets.go` + `db/nkey.go`.
//!
//! These normalize URLs/IPs and build PostgreSQL array literals; the AssetStore
//! write methods (with their tx/lock protocol) land separately.

use crate::db::error::{Error, Result};

pub const ERR_ASSET_IP_INVALID: &str = "invalid asset ip";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PortService {
    pub port: i32,
    pub service: String,
}

/// `calcCSegment`: IPv4 /24 or IPv6 /48 network string (empty on bad input).
pub fn calc_c_segment(ip_str: &str) -> String {
    if ip_str.is_empty() {
        return String::new();
    }
    let ip: std::net::IpAddr = match ip_str.parse() {
        Ok(ip) => ip,
        Err(_) => return String::new(),
    };
    if ip.is_ipv4() {
        let parts: Vec<&str> = ip_str.split('.').collect();
        if parts.len() == 4 {
            return format!("{}.{}.{}.0/24", parts[0], parts[1], parts[2]);
        }
        String::new()
    } else {
        // IPv6 /48: mask the first 48 bits.
        let v6 = match ip {
            std::net::IpAddr::V6(v) => v,
            _ => return String::new(),
        };
        let mask = u128::MAX << 80;
        let net = u128::from(v6) & mask;
        let masked = std::net::Ipv6Addr::from(net);
        format!("{masked}/48")
    }
}

/// `defaultPort`: explicit port if valid, else scheme default (http 80/https 443).
pub fn default_port(scheme: &str, p: &str) -> i32 {
    if !p.is_empty() {
        if let Ok(n) = p.parse::<i32>() {
            return n;
        }
    }
    match scheme {
        "https" => 443,
        "http" => 80,
        _ => 0,
    }
}

/// `normalizeURL`: lowercase scheme+host, drop userinfo, strip trailing slash on
/// bare roots — credentials must never reach `assets.url`.
pub fn normalize_url(raw: &str) -> String {
    let Ok(mut u) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let scheme = u.scheme().to_lowercase();
    let host = u.host_str().unwrap_or("").to_lowercase();
    let _ = u.set_scheme(&scheme);
    let _ = u.set_host(Some(&host));
    u.set_username("").ok();
    u.set_password(None).ok();
    let mut s = u.to_string();
    if s.ends_with('/') && u.path() == "/" && u.query().is_none() {
        s.pop();
    }
    s
}

/// `parseURL`: (domain, port, service_name) from a URL; empty on parse failure.
pub fn parse_url(raw: &str) -> (String, i32, String) {
    let Ok(u) = url::Url::parse(raw) else {
        return (String::new(), 0, String::new());
    };
    let scheme = u.scheme().to_lowercase();
    let domain = u.host_str().unwrap_or("").to_lowercase();
    let port = default_port(
        &scheme,
        u.port().map(|p| p.to_string()).unwrap_or_default().as_str(),
    );
    let service = match scheme.as_str() {
        "https" => "HTTPS".to_string(),
        "http" => "HTTP".to_string(),
        other => other.to_uppercase(),
    };
    (domain, port, service)
}

/// `marshalJSONBArray`: PostgreSQL array literal for jsonb[] (each element a
/// double-quoted, backslash/quote-escaped JSON object).
pub fn marshal_jsonb_array(items: &[serde_json::Value]) -> Result<String> {
    if items.is_empty() {
        return Ok("{}".to_string());
    }
    let mut parts = Vec::with_capacity(items.len());
    for v in items {
        let b = serde_json::to_string(v).map_err(|e| Error::msg(e.to_string()))?;
        let s = b.replace('\\', "\\\\").replace('"', "\\\"");
        parts.push(format!("\"{s}\""));
    }
    Ok(format!("{{{}}}", parts.join(",")))
}

/// `marshalStringArray`: PostgreSQL text[] literal.
pub fn marshal_string_array(items: &[String]) -> String {
    if items.is_empty() {
        return "{}".to_string();
    }
    let escaped: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("{{{}}}", escaped.join(","))
}

/// `marshalPortServices`: PortService slice -> jsonb[] array literal.
pub fn marshal_port_services(ps: &[PortService]) -> Result<String> {
    let items: Vec<serde_json::Value> = ps
        .iter()
        .map(|p| serde_json::json!({ "port": p.port, "service": p.service }))
        .collect();
    marshal_jsonb_array(&items)
}

/// `nullableInt`: 0 -> None (SQL NULL), else Some.
pub fn nullable_int(v: i32) -> Option<i32> {
    if v == 0 {
        None
    } else {
        Some(v)
    }
}

/// `ValidateAssetIP`: keep hostnames out of `assets.ip` (it is cast to inet).
pub fn validate_asset_ip(value: &str) -> Result<()> {
    if value.is_empty() || value.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    Err(Error::sentinel(
        ERR_ASSET_IP_INVALID,
        format!(
            "{ERR_ASSET_IP_INVALID}: ip must be an IPv4/IPv6 address, got {value:?}. If this is a hostname, use type=subdomain and fill the domain field instead; if you really need to register the address, resolve its A/AAAA records first, then fill ip with the resolved address"
        ),
    ))
}
