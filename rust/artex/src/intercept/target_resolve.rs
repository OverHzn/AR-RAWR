//! Target resolution helpers — pure slice of `intercept/target_resolve.go`.
//!
//! `subset` decides whether every resolved address is covered by a pin. An empty
//! answer is never a subset: a lookup that returned nothing matches no pin.

use std::net::IpAddr;

/// `canonicalIPs`: sorted, unique address strings; IPv4-mapped addresses unmapped
/// and zones dropped (Rust `IpAddr` carries no zone).
pub fn canonical_ips(addrs: &[IpAddr]) -> Vec<String> {
    let mut out: Vec<String> = addrs
        .iter()
        .map(|a| match a {
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4).to_string(),
                None => a.to_string(),
            },
            _ => a.to_string(),
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `subset`: every resolved address is one of the pinned ones; empty answer -> false.
pub fn subset(resolved: &[String], pinned: &[String]) -> bool {
    if resolved.is_empty() {
        return false;
    }
    resolved.iter().all(|ip| pinned.contains(ip))
}
