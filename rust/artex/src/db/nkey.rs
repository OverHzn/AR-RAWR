//! Normalized natural keys — pure port of `db/nkey.go`.
//!
//! Child asset keys embed the parent's int64 id, encoding the hierarchy into
//! the key. PG dedups on UNIQUE(type, nkey), so these must match Go byte-for-byte.

use regex::Regex;

use crate::db::error::Result;
use crate::db::publicsuffix::root_domain;

/// `DomainKey`: lowercase, trim, drop a single trailing dot.
pub fn domain_key(fqdn: &str) -> String {
    fqdn.trim().to_lowercase().trim_end_matches('.').to_string()
}

/// `IPKey`: trimmed address.
pub fn ip_key(ip: &str) -> String {
    ip.trim().to_string()
}

/// `RootDomain`: registrable domain (eTLD+1) and whether the host is itself apex.
/// IPs and unclassifiable hosts are their own root (apex) — best effort.
pub fn root(host: &str) -> (String, bool) {
    root_domain(host)
}

fn itoa(n: i64) -> String {
    n.to_string()
}

pub fn port_key(ip_id: i64, proto: &str, port: i32) -> String {
    format!("{}|{}|{}", itoa(ip_id), proto.to_lowercase(), port)
}

pub fn service_key(port_id: i64, svc_name: &str) -> String {
    format!("{}|{}", itoa(port_id), svc_name.to_lowercase())
}

pub fn site_key(scheme: &str, host: &str, port: i32) -> String {
    format!("{}|{}|{}", scheme.to_lowercase(), host.to_lowercase(), port)
}

pub fn endpoint_key(site_id: i64, method: &str, url_template: &str) -> String {
    format!(
        "{}|{}|{}",
        itoa(site_id),
        method.to_uppercase(),
        url_template
    )
}

pub fn parameter_key(endpoint_id: i64, location: &str, name: &str) -> String {
    format!("{}|{}|{}", itoa(endpoint_id), location.to_lowercase(), name)
}

/// `NormalizeParamName`: lowercase + trim; no synonym merging.
pub fn normalize_param_name(name: &str) -> String {
    name.trim().to_lowercase()
}

pub fn tech_key(name: &str, version: &str) -> String {
    format!("{}|{}", name.to_lowercase(), version)
}

fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}

/// `TemplatePath`: collapse high-cardinality segments into placeholders.
pub fn template_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let numeric = re(r"^\d+$");
    let uuid = re(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$");
    let hex = re(r"^[0-9a-fA-F]{16,}$");
    let long = re(r"^[A-Za-z0-9_-]{24,}$");
    let segs: Vec<String> = path
        .split('/')
        .map(|s| {
            if s.is_empty() {
                return s.to_string();
            }
            if numeric.is_match(s) {
                "{id}".to_string()
            } else if uuid.is_match(s) {
                "{uuid}".to_string()
            } else if hex.is_match(s) {
                "{hex}".to_string()
            } else if long.is_match(s) {
                "{token}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect();
    segs.join("/")
}

/// `defaultPort`: explicit port when valid, else the scheme default.
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

/// `SplitURL`: (scheme, host, port, urlTemplate, query keys).
pub fn split_url(raw: &str, _method: &str) -> Result<(String, String, i32, String, Vec<String>)> {
    // Go's url.Parse accepts a relative reference: no scheme/host, empty port,
    // the whole string as path (e.g. "not-a-url" -> Path "not-a-url").
    let Ok(u) = url::Url::parse(raw) else {
        return Ok((
            String::new(),
            String::new(),
            0,
            template_path(raw),
            Vec::new(),
        ));
    };
    let scheme = u.scheme().to_lowercase();
    let host = u.host_str().unwrap_or("").to_lowercase();
    let port = default_port(
        &scheme,
        u.port().map(|p| p.to_string()).unwrap_or_default().as_str(),
    );
    let template = template_path(u.path());
    let mut params: Vec<String> = u.query_pairs().map(|(k, _)| k.into_owned()).collect();
    params.sort();
    Ok((scheme, host, port, template, params))
}
