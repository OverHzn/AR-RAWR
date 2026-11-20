//! Target-gate exemption validation — port of `intercept/target_exempt.go` (pure slice).
//!
//! An operator may exempt a private unicast IP literal, never loopback, link-local
//! (cloud metadata), unspecified or multicast. Names are the void rule's business
//! (resolved at check time); an IP literal is checked here because it is never resolved.

use crate::db::error::Error;
use crate::intercept::target::{normalize_target, TARGET_CIDR};

/// Instance-metadata addresses outside the link-local ranges.
const METADATA_ADDRS: [&str; 2] = ["fd00:ec2::254", "100.100.100.200"];

/// Ports `net/http` dials for proxy URLs without an explicit port.
fn proxy_port(scheme: &str) -> i32 {
    match scheme.to_lowercase().as_str() {
        "socks5" | "socks5h" => 1080,
        _ => 0,
    }
}

/// `ValidateExemptionHost`: refuse loopback/link-local/unspecified/multicast and
/// instance-metadata IP literals; private unicast + public + names are allowed.
pub fn validate_exemption_host(host: &str) -> std::result::Result<(), Error> {
    // netip.ParseAddr accepts an IPv6 zone (`fe80::1%eth0`); Rust's parser does not.
    let token = host.split('%').next().unwrap_or(host);
    let Ok(ip) = token.parse::<std::net::IpAddr>() else {
        return Ok(()); // a name — handled by the void rule at check time
    };
    let (unmapped, zone_free) = unmap_zone(ip);
    let kind = if unmapped.is_loopback() {
        Some("a loopback")
    } else if unmapped.is_unspecified() {
        Some("the unspecified")
    } else if is_link_local(&unmapped) {
        Some("a link-local (metadata range)")
    } else if is_multicast(&unmapped) {
        Some("a multicast")
    } else if METADATA_ADDRS.iter().any(|m| *m == zone_free.to_string()) {
        Some("an instance-metadata")
    } else {
        None
    };
    match kind {
        Some(k) => Err(Error::msg(format!(
            "{host} is {k} address and can never be exempt from target approval"
        ))),
        None => Ok(()),
    }
}

fn unmap_zone(ip: std::net::IpAddr) -> (std::net::IpAddr, std::net::IpAddr) {
    match ip {
        std::net::IpAddr::V4(_) => (ip, ip),
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => (std::net::IpAddr::V4(v4), std::net::IpAddr::V4(v4)),
            None => (std::net::IpAddr::V6(v6), std::net::IpAddr::V6(v6)),
        },
    }
}

fn is_link_local(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.octets()[0] == 169 && v.octets()[1] == 254,
        std::net::IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn is_multicast(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.octets()[0] >= 224 && v.octets()[0] <= 239,
        std::net::IpAddr::V6(v) => (v.segments()[0] & 0xff00) == 0xff00,
    }
}

/// One derived exemption: exact host + port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exemption {
    pub host: String,
    pub port: i32,
}

/// `DerivedExemptions`: normalize configured proxy/gateway endpoints into exact
/// host:port exemptions; scheme defaults (1080 for socks5) fill missing ports;
/// endpoints with no port and no known scheme are skipped.
pub fn derived_exemptions(endpoints: &[String]) -> Vec<Exemption> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw in endpoints {
        let Some(mut t) = normalize_target(raw) else {
            continue;
        };
        let trimmed = raw.trim();
        if let Some((scheme, _)) = trimmed.split_once("://") {
            if t.port == 0 {
                t.port = proxy_port(scheme);
            }
        }
        if t.port == 0 || t.kind == TARGET_CIDR {
            continue;
        }
        let key = format!("{}|{}", t.host, t.port);
        if seen.insert(key) {
            out.push(Exemption {
                host: t.host,
                port: t.port,
            });
        }
    }
    out
}

/// `voidsExemption`: an exemption on a name is void when it resolves to a
/// private/loopback/link-local address; unparseable IPs are void too.
pub fn voids_exemption(ip: &str) -> bool {
    let Ok(addr) = ip.parse::<std::net::IpAddr>() else {
        return true;
    };
    let (unmapped, _) = unmap_zone(addr);
    let private = match &unmapped {
        std::net::IpAddr::V4(v) => {
            let o = v.octets();
            o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
        }
        std::net::IpAddr::V6(v) => v.segments()[0] & 0xfe00 == 0xfc00, // fc00::/7 unique local
    };
    private || validate_exemption_host(ip).is_err()
}
