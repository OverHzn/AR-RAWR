//! Pure task-asset / task-scope helpers — slice of `db/task_assets.go` +
//! `db/task_scope.go`. Unblocks the AssetStore write path.

use std::collections::HashSet;

use crate::db::asset_helpers::{normalize_url, parse_url};
use crate::db::error::{Error, Result};

pub const ERR_TASK_ASSET_INVALID: &str = "invalid task asset association";
pub const MAX_TASK_ASSET_MUTATION_COUNT: usize = 100;
pub const MAX_TASK_ASSET_SUMMARY_RUNES: usize = 500;
const DEFAULT_TASK_ASSET_SOURCE: &str = "system";

/// `normalizeTaskAssetIDs`: trim dupes, reject empty/non-positive/too-many.
pub fn normalize_task_asset_ids(ids: &[i64]) -> Result<Vec<i64>> {
    if ids.is_empty() {
        return Err(Error::sentinel(
            ERR_TASK_ASSET_INVALID,
            "invalid task asset association: asset_ids is required",
        ));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        if id <= 0 {
            return Err(Error::sentinel(
                ERR_TASK_ASSET_INVALID,
                "invalid task asset association: asset id must be positive",
            ));
        }
        if !seen.insert(id) {
            continue;
        }
        out.push(id);
        if out.len() > MAX_TASK_ASSET_MUTATION_COUNT {
            return Err(Error::sentinel(
                ERR_TASK_ASSET_INVALID,
                format!("invalid task asset association: at most {MAX_TASK_ASSET_MUTATION_COUNT} assets per request"),
            ));
        }
    }
    Ok(out)
}

/// `normalizeTaskAssetSource`: lowercase/trim source (default "system"), trim
/// summary, cap at 500 runes.
pub fn normalize_task_asset_source(source: &str, summary: &str) -> Result<(String, String)> {
    let source = source.trim().to_lowercase();
    let source = if source.is_empty() {
        DEFAULT_TASK_ASSET_SOURCE.to_string()
    } else {
        source
    };
    let summary = summary.trim().to_string();
    if summary.chars().count() > MAX_TASK_ASSET_SUMMARY_RUNES {
        return Err(Error::sentinel(
            ERR_TASK_ASSET_INVALID,
            format!("invalid task asset association: source summary exceeds {MAX_TASK_ASSET_SUMMARY_RUNES} characters"),
        ));
    }
    Ok((source, summary))
}

/// `stripHostPort`: drop a trailing :port from host:port / ip:port / [ipv6]:port.
/// Bare hosts and bare IPv6 (colons but no port) are returned unchanged.
pub fn strip_host_port(v: &str) -> String {
    let v = v.trim();
    // Bracketed IPv6 with port: "[::1]:8080" -> "::1".
    if v.starts_with('[') {
        if let Some(end) = v.find(']') {
            if v[end + 1..].starts_with(':') {
                return v[1..end].to_string();
            }
        }
        return v.to_string();
    }
    // A single colon with a numeric port and a non-colon host.
    if let Some((host, port)) = v.rsplit_once(':') {
        if !host.is_empty()
            && !host.contains(':')
            && !port.is_empty()
            && port.bytes().all(|c| c.is_ascii_digit())
        {
            return host.to_string();
        }
    }
    v.to_string()
}

/// `ipToHostCIDR`: a bare IP as its single-host CIDR (/32 or /128); "" if invalid.
pub fn ip_to_host_cidr(ip: &str) -> String {
    let ip = ip.trim();
    let Ok(addr) = ip.parse::<std::net::IpAddr>() else {
        return String::new();
    };
    if addr.is_ipv4() {
        format!("{ip}/32")
    } else {
        format!("{ip}/128")
    }
}

pub fn asset_key(id: i64) -> String {
    format!("a:{id}")
}
pub fn company_key(id: i64) -> String {
    format!("c:{id}")
}

/// `CoverageGraphNode`: the subset the pure helpers read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CoverageGraphNode {
    pub key: String,
    pub kind: String,
    pub domain: String,
    pub root_domain: String,
    pub ip: String,
    pub url: String,
    pub port: i32,
    pub app_name: String,
}

/// `coverageNodeLabel`: the most informative label of a graph node.
pub fn coverage_node_label(n: &CoverageGraphNode) -> String {
    if (n.kind == "endpoint" || n.kind == "service") && !n.url.is_empty() {
        return n.url.clone();
    }
    if n.kind == "app" && !n.app_name.is_empty() {
        return n.app_name.clone();
    }
    for v in [&n.url, &n.domain, &n.ip, &n.app_name, &n.root_domain] {
        if !v.is_empty() {
            return v.clone();
        }
    }
    n.key.clone()
}

/// `hostPortOf`: (host, port) a service/endpoint node hangs off — domain, else
/// URL host, else IP.
pub fn host_port_of(n: &CoverageGraphNode) -> (String, i32) {
    let mut host = n.domain.clone();
    let mut port = n.port;
    if host.is_empty() && !n.url.is_empty() {
        let (h, p, _) = parse_url(&normalize_url(&n.url));
        host = h;
        if port == 0 {
            port = p;
        }
    }
    if host.is_empty() {
        host = n.ip.clone();
    }
    (host, port)
}
