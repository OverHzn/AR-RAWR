//! Target normalization — port of `intercept/target.go` (the pure slice).
//!
//! `Target` is one network target a tool call declares. With its scope, Host and
//! Port are the key of a pin and of an approval request; path/query/scheme never
//! participate.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const TARGET_DOMAIN: &str = "domain";
pub const TARGET_IP: &str = "ip";
pub const TARGET_CIDR: &str = "cidr";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    pub kind: String,
    pub host: String,
    /// 0 = port not declared.
    pub port: i32,
}

fn scheme_port(scheme: &str) -> i32 {
    match scheme {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        "ftp" => 21,
        _ => 0,
    }
}

/// Percent-decode a URL host component (`url.PathUnescape` equivalent).
fn path_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = (b[i + 1] as char).to_digit(16);
            let l = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `NormalizeTarget`: parse a target written as a URL with a scheme, or as a
/// host, host:port, [IPv6]:port, IP literal or CIDR.
pub fn normalize_target(raw: &str) -> Option<Target> {
    let raw = raw.trim();
    let (def, is_url, rest) = if let Some(i) = raw.find("://") {
        if i > 0 {
            (scheme_port(&raw[..i].to_lowercase()), true, &raw[i + 3..])
        } else {
            (0, false, raw)
        }
    } else {
        (0, false, raw)
    };
    let mut rest = rest;
    if is_url {
        if let Some(end) = rest.find(['/', '?', '#']) {
            rest = &rest[..end];
        }
        if let Some(at) = rest.rfind('@') {
            rest = &rest[at + 1..];
        }
    }
    let (mut host, mut port) = (rest.to_string(), String::new());
    if rest.starts_with('[') {
        let end = rest.find(']')?;
        host = rest[1..end].to_string();
        let after = &rest[end + 1..];
        if !after.is_empty() {
            if !after.starts_with(':') {
                return None;
            }
            port = after[1..].to_string();
        }
    } else if rest.matches(':').count() == 1 {
        let (h, p) = rest.split_once(':').unwrap();
        host = h.to_string();
        port = p.to_string();
    }
    if is_url && !rest.starts_with('[') {
        host = path_unescape(&host);
    }
    let mut t = Target {
        port: def,
        ..Target::default()
    };
    if !port.is_empty() {
        let n: i32 = port.parse().ok()?;
        if !(1..=65535).contains(&n) {
            return None;
        }
        t.port = n;
    }
    let (kind, normalized) = normalize_host(&host, true)?;
    t.kind = kind;
    t.host = normalized;
    Some(t)
}

/// Mask an IP network to its canonical `network/prefix` string.
fn mask_prefix(ip: IpAddr, ones: u32) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let mask = if ones == 0 {
                0u32
            } else {
                u32::MAX << (32 - ones)
            };
            let net = Ipv4Addr::from(u32::from(v4) & mask);
            format!("{net}/{ones}")
        }
        IpAddr::V6(v6) => {
            let mask = if ones == 0 {
                0u128
            } else {
                u128::MAX << (128 - ones)
            };
            let net = Ipv6Addr::from(u128::from(v6) & mask);
            format!("{net}/{ones}")
        }
    }
}

/// `normalizeHost`: canonical (kind, host) form of one host token.
pub fn normalize_host(raw: &str, numeric: bool) -> Option<(String, String)> {
    let h = raw.trim().to_lowercase();
    let h = h.strip_suffix('.').unwrap_or(&h);
    let h = h.split('%').next().unwrap_or(h); // drop IPv6 zone
    if h.is_empty() {
        return None;
    }
    if h.contains('/') {
        let (addr, prefix) = h.split_once('/')?;
        let ip: IpAddr = addr.parse().ok()?;
        let bits = if ip.is_ipv4() { 32 } else { 128 };
        let ones: u32 = prefix.parse().ok()?;
        if ones > bits {
            return None;
        }
        return Some((TARGET_CIDR.to_string(), mask_prefix(ip, ones)));
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        let canon = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => match v6.to_ipv4() {
                Some(v4) => v4.to_string(),
                None => v6.to_string(),
            },
        };
        return Some((TARGET_IP.to_string(), canon));
    }
    if numeric {
        if let Some(ip) = parse_ipv4_forms(h) {
            return Some((TARGET_IP.to_string(), ip.to_string()));
        }
    }
    // A name the profile refuses is kept as written (it then fails to resolve
    // and gets blocked, rather than passing as a call without a target).
    let name = idna::domain_to_ascii(h).unwrap_or_else(|_| h.to_string());
    let name = if name.is_empty() { h.to_string() } else { name };
    Some((TARGET_DOMAIN.to_string(), name))
}

/// `parseIPv4Forms`: inet_aton-style IPv4 — 1..4 parts, decimal/hex/octal, last
/// part filling the remaining bytes.
pub fn parse_ipv4_forms(s: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 4 || parts.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for (n, p) in parts.iter().enumerate() {
        let (digits, base) = if let Some(hex) = p.strip_prefix("0x") {
            (hex, 16u32)
        } else if p.len() > 1 && p.starts_with('0') {
            (&p[1..], 8u32)
        } else {
            (*p, 10u32)
        };
        let x = u64::from_str_radix(digits, base).ok()?;
        if n < parts.len() - 1 {
            if x > 255 {
                return None;
            }
            v = v << 8 | x;
        } else {
            let rest = 8 * (4 - n);
            if x >= (1u64 << rest) {
                return None;
            }
            v = v << rest | x;
        }
    }
    Some(Ipv4Addr::new(
        (v >> 24) as u8,
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
    ))
}

/// `validHostname`: whether h is a plain DNS name (letters/digits/hyphen/underscore).
pub fn valid_hostname(h: &str) -> bool {
    if h.len() > 253 {
        return false;
    }
    for label in h.split('.') {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return false;
        }
    }
    true
}
