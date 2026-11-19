//! Asset interception matching — port of the pure half of
//! `db/asset_intercept_match.go` (`AssetInterceptKindLabel`, `AssetInterceptRule.Reason`,
//! `matchOne`, `MatchAssetInterceptRules`, `hasEnabledRule`, `EvaluateAssetGate`).
//!
//! `asset_intercept.go` only stores rules; this layer matches a target asset's
//! domain/IP/URL candidates against the enabled rules. The `*Asset`/`AssetStore`
//! helpers (`interceptCandidates`, `CheckAssetsIntercept`) are deferred until the
//! Asset type is ported with the assets group.
//!
//! `AssetInterceptKindLabel` is reused from `db-rs::port` (gate-pinned). The CIDR
//! matcher uses `std::net` (no external crate): `parse_cidr` + `cidr_contains`.

use crate::db::asset_intercept::AssetInterceptRule;
use std::net::IpAddr;

/// `AssetInterceptKindLabel`: human-readable label for a rule kind (re-export of
/// the gate-pinned `db-rs::port` helper).
pub fn asset_intercept_kind_label(kind: &str) -> String {
    db_rs::port::asset_intercept_kind_label(kind.to_string()).unwrap_or_else(|_| kind.to_string())
}

impl AssetInterceptRule {
    /// `Reason`: a readable hit reason, e.g.
    /// `Matched asset interception rule [Domain (fuzzy): .gov.cn] (note)`.
    pub fn reason(&self) -> String {
        let mut s = format!(
            "Matched asset interception rule [{}: {}]",
            asset_intercept_kind_label(&self.kind),
            self.pattern
        );
        let note = self.note.trim();
        if !note.is_empty() {
            s.push_str(&format!(" ({note})"));
        }
        s
    }
}

/// `matchOne`: does one enabled rule match any candidate? Returns the matched value.
fn match_one(
    r: &AssetInterceptRule,
    domains: &[String],
    ips: &[String],
    urls: &[String],
) -> (String, bool) {
    let p = r.pattern.trim();
    if p.is_empty() {
        return (String::new(), false);
    }
    match r.kind.as_str() {
        "exact_domain" => {
            for d in domains {
                if d.trim().eq_ignore_ascii_case(p) {
                    return (d.clone(), true);
                }
            }
        }
        "exact_ip" => {
            for ip in ips {
                if ip.trim() == p {
                    return (ip.clone(), true);
                }
            }
        }
        "exact_url" => {
            for u in urls {
                if u.trim() == p {
                    return (u.clone(), true);
                }
            }
        }
        "fuzzy_domain" => {
            let lp = p.to_lowercase();
            for d in domains {
                if !d.is_empty() && d.to_lowercase().contains(&lp) {
                    return (d.clone(), true);
                }
            }
        }
        "fuzzy_ip" => {
            for ip in ips {
                if !ip.is_empty() && ip.contains(p) {
                    return (ip.clone(), true);
                }
            }
        }
        "fuzzy_url" => {
            let lp = p.to_lowercase();
            for u in urls {
                if !u.is_empty() && u.to_lowercase().contains(&lp) {
                    return (u.clone(), true);
                }
            }
        }
        "cidr" => {
            let Some((net, prefix)) = parse_cidr(p) else {
                return (String::new(), false);
            };
            for ip in ips {
                if let Ok(pip) = ip.trim().parse::<IpAddr>() {
                    if cidr_contains(net, prefix, pip) {
                        return (ip.clone(), true);
                    }
                }
            }
        }
        _ => {}
    }
    (String::new(), false)
}

/// `MatchAssetInterceptRules`: first enabled rule matching any candidate + the
/// concrete matched value.
pub fn match_asset_intercept_rules(
    rules: &[AssetInterceptRule],
    domains: &[String],
    ips: &[String],
    urls: &[String],
) -> (AssetInterceptRule, String, bool) {
    for r in rules {
        if !r.enabled {
            continue;
        }
        let (v, ok) = match_one(r, domains, ips, urls);
        if ok {
            return (r.clone(), v, true);
        }
    }
    (AssetInterceptRule::default(), String::new(), false)
}

/// `hasEnabledRule`: does the set contain any enabled rule?
fn has_enabled_rule(rules: &[AssetInterceptRule]) -> bool {
    rules.iter().any(|r| r.enabled)
}

/// `AssetGateDecision`: the result of the "intercept first, then allow" gate.
#[derive(Debug, Clone, Default)]
pub struct AssetGateDecision {
    pub allowed: bool,
    /// Rejection reason (empty when allowed).
    pub reason: String,
}

/// `EvaluateAssetGate`:
/// 1. Any enabled block rule hit → reject (interception reason).
/// 2. Else, if allow rules have enabled entries and none match → reject (outside scope).
/// 3. Else allow. An empty/disabled allow set makes the whitelist inert (allow all).
pub fn evaluate_asset_gate(
    block_rules: &[AssetInterceptRule],
    allow_rules: &[AssetInterceptRule],
    domains: &[String],
    ips: &[String],
    urls: &[String],
) -> AssetGateDecision {
    if let (rule, _, true) = match_asset_intercept_rules(block_rules, domains, ips, urls) {
        return AssetGateDecision {
            allowed: false,
            reason: rule.reason(),
        };
    }
    if has_enabled_rule(allow_rules)
        && !match_asset_intercept_rules(allow_rules, domains, ips, urls).2
    {
        return AssetGateDecision {
            allowed: false,
            reason: "Not in the task's allowed (whitelist) scope; testing is not permitted"
                .to_string(),
        };
    }
    AssetGateDecision {
        allowed: true,
        reason: String::new(),
    }
}

/// `parseCIDR`: parse `addr/prefix` into `(network address, prefix length)`.
fn parse_cidr(s: &str) -> Option<(IpAddr, u8)> {
    let (addr, prefix) = s.split_once('/')?;
    let ip: IpAddr = addr.trim().parse().ok()?;
    let prefix: u8 = prefix.trim().parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return None;
    }
    Some((ip, prefix))
}

/// `ipnet.Contains`: does `ip` fall within `network/prefix`? Cross-family → false.
fn cidr_contains(net: IpAddr, prefix: u8, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(net), IpAddr::V4(ip)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(net) & mask) == (u32::from(ip) & mask)
        }
        (IpAddr::V6(net), IpAddr::V6(ip)) => {
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(net) & mask) == (u128::from(ip) & mask)
        }
        _ => false,
    }
}
