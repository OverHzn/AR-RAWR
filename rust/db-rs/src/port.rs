//! The Rust port of `db/`.
//!
//! Only pure, value-typed functions are bridged here. Functions that take or
//! return structs (`[]ScopeInput`, `[]AssetInterceptRule`, `[]TrafficRef`,
//! `[]Lesson`, `TrafficEvidenceSnapshot`), depend on `net/netip`, the
//! `golang.org/x/net/publicsuffix` list, the asset-DSL/scope parsers, environment
//! state or the stateful process key, or return a nil slice (JSON `null`) stay in
//! Go (see `rust-anything.json`'s `js_only`). Shared Go-stdlib helpers live in the
//! `gocompat` crate.

#![forbid(unsafe_code)]

use gocompat::strings;
use serde_json::{Value, json};

/// Port of `AssetInterceptKindLabel` (`db/asset_intercept_match.go:17`).
pub fn asset_intercept_kind_label(kind: String) -> Result<String, String> {
    Ok(match kind.as_str() {
        "exact_domain" => "Domain (exact)",
        "exact_ip" => "IP (exact)",
        "exact_url" => "URL (exact)",
        "fuzzy_domain" => "Domain (fuzzy)",
        "fuzzy_ip" => "IP (fuzzy)",
        "fuzzy_url" => "URL (fuzzy)",
        "cidr" => "CIDR range",
        _ => return Ok(kind),
    }
    .to_string())
}

// --- Findings status / report / severity constants (db/findings.go) ----------

/// Port of `ValidChatMentionKind` (`db/chat_mentions.go:35`).
pub fn valid_chat_mention_kind(kind: String) -> Result<bool, String> {
    Ok(matches!(
        kind.as_str(),
        "finding"
            | "company"
            | "asset"
            | "endpoint"
            | "ip"
            | "app"
            | "root_domain"
            | "subdomain"
            | "service"
    ))
}

/// Port of `NormalizeICP` (`db/company_scope.go:33`).
pub fn normalize_icp(value: String) -> Result<String, String> {
    let trimmed = strings::go_trim_space(&value);
    let mapped: String = trimmed
        .chars()
        .filter(|c| !strings::go_is_space(*c))
        .collect();
    Ok(strings::go_to_lower(&mapped))
}

/// Port of `ValidCoverageOutcome` (`db/coverage_ledger.go:55`).
pub fn valid_coverage_outcome(o: String) -> Result<bool, String> {
    Ok(matches!(
        o.as_str(),
        "reported" | "no_issue_found" | "ruled_out" | "not_applicable" | "needs_follow_up"
    ))
}

/// Port of `NormalizeCoverageField` (`db/coverage_ledger.go:113`).
pub fn normalize_coverage_field(s: String) -> Result<String, String> {
    let mapped: String = s
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    Ok(strings::go_fields_join(&mapped))
}

/// Port of `CoverageCompletenessFor` (`db/coverage_ledger.go:317`).
pub fn coverage_completeness_for(
    status: String,
    paused: bool,
    budget: bool,
    past_deadline: bool,
) -> Result<Value, String> {
    let state = if budget {
        "budget"
    } else if status == "timeout" || (status == "done" && past_deadline) {
        "timeout"
    } else if status == "failed" {
        "aborted"
    } else if paused || status == "paused" {
        "paused"
    } else if status != "done" {
        "in_progress"
    } else {
        "complete"
    };
    if state == "complete" {
        Ok(json!({
            "complete": true,
            "state": state,
            "caveat": "Run ended normally. Classes listed as unexamined were not tested and are not clean.",
        }))
    } else {
        Ok(json!({
            "complete": false,
            "state": state,
            "caveat": format!(
                "Incomplete scan ({state}): the absence of a finding does not mean the target is clean; unexamined classes and open gaps were not covered."
            ),
        }))
    }
}

/// Port of `ValidTrafficRole` (`db/finding_traffic.go:70`).
pub fn valid_traffic_role(role: String) -> Result<bool, String> {
    Ok(matches!(
        role.as_str(),
        "baseline" | "proof" | "verification" | "supporting"
    ))
}

/// Port of `ValidVerificationStatus` (`db/finding_verification.go:17`).
pub fn valid_verification_status(s: String) -> Result<bool, String> {
    Ok(matches!(
        s.as_str(),
        "verified" | "unverified" | "blocked" | "not_assessed" | "false_positive"
    ))
}

/// Port of `ValidFindingStatus` (`db/findings.go:60`).
pub fn valid_finding_status(s: String) -> Result<bool, String> {
    Ok(matches!(
        s.as_str(),
        "pending"
            | "in_progress"
            | "confirmed"
            | "resolved"
            | "fixed"
            | "false_positive"
            | "ignored"
            | "duplicate"
            | "risk_accepted"
    ))
}

/// Port of `ValidFindingReportState` (`db/findings.go:81`).
pub fn valid_finding_report_state(s: String) -> Result<bool, String> {
    Ok(matches!(
        s.as_str(),
        "none" | "submitted" | "accepted" | "rejected" | "duplicate"
    ))
}

/// Port of `ValidSeverity` (`db/findings.go:99`).
pub fn valid_severity(s: String) -> Result<bool, String> {
    Ok(matches!(s.as_str(), "critical" | "high" | "medium" | "low"))
}

/// Port of `ParseExpID` (`db/llm_usage.go:321`).
pub fn parse_exp_id(s: String) -> Result<i64, String> {
    Ok(match s.parse::<i64>() {
        Ok(n) if n >= 0 => n,
        _ => 0,
    })
}

/// Port of `DomainKey` (`db/nkey.go:17`).
pub fn domain_key(fqdn: String) -> Result<String, String> {
    let lowered = strings::go_to_lower(strings::go_trim_space(&fqdn));
    Ok(lowered.strip_suffix('.').unwrap_or(&lowered).to_string())
}

/// Port of `IPKey` (`db/nkey.go:21`).
pub fn ip_key(ip: String) -> Result<String, String> {
    Ok(strings::go_trim_space(&ip).to_string())
}

/// Port of `PortKey` (`db/nkey.go:40`).
pub fn port_key(ip_id: i64, proto: String, port: i64) -> Result<String, String> {
    Ok(format!(
        "{}|{}|{}",
        ip_id,
        strings::go_to_lower(&proto),
        port
    ))
}

/// Port of `ServiceKey` (`db/nkey.go:44`).
pub fn service_key(port_id: i64, svc_name: String) -> Result<String, String> {
    Ok(format!("{}|{}", port_id, strings::go_to_lower(&svc_name)))
}

/// Port of `SiteKey` (`db/nkey.go:48`).
pub fn site_key(scheme: String, host: String, port: i64) -> Result<String, String> {
    Ok(format!(
        "{}|{}|{}",
        strings::go_to_lower(&scheme),
        strings::go_to_lower(&host),
        port
    ))
}

/// Port of `EndpointKey` (`db/nkey.go:52`).
pub fn endpoint_key(site_id: i64, method: String, url_template: String) -> Result<String, String> {
    Ok(format!(
        "{}|{}|{}",
        site_id,
        strings::go_to_upper(&method),
        url_template
    ))
}

/// Port of `ParameterKey` (`db/nkey.go:56`).
pub fn parameter_key(endpoint_id: i64, location: String, name: String) -> Result<String, String> {
    Ok(format!(
        "{}|{}|{}",
        endpoint_id,
        strings::go_to_lower(&location),
        name
    ))
}

/// Port of `NormalizeParamName` (`db/nkey.go:64`).
pub fn normalize_param_name(name: String) -> Result<String, String> {
    Ok(strings::go_to_lower(strings::go_trim_space(&name)))
}

/// Port of `TechKey` (`db/nkey.go:68`).
pub fn tech_key(name: String, version: String) -> Result<String, String> {
    Ok(format!("{}|{}", strings::go_to_lower(&name), version))
}

/// `regexp ^\d+$` (`reNumeric`): one or more ASCII digits.
fn re_numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `regexp ^[0-9a-fA-F]{16,}$` (`reHex`): 16+ hexadecimal digits.
fn re_hex(s: &str) -> bool {
    s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `regexp ^[A-Za-z0-9_-]{24,}$` (`reLong`): 24+ token characters.
fn re_long(s: &str) -> bool {
    s.len() >= 24
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `regexp` UUID shape (`reUUID`): 8-4-4-4-12 hex with dashes.
fn re_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if c != b'-' {
                    return false;
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Port of `TemplatePath` (`db/nkey.go:83`): collapse high-cardinality path
/// segments into placeholders (`/user/123` → `/user/{id}`).
pub fn template_path(path: String) -> Result<String, String> {
    if path.is_empty() {
        return Ok("/".to_string());
    }
    let mut segs: Vec<String> = path.split('/').map(|s| s.to_string()).collect();
    for seg in segs.iter_mut() {
        if seg.is_empty() {
            continue;
        }
        *seg = if re_numeric(seg) {
            "{id}".to_string()
        } else if re_uuid(seg) {
            "{uuid}".to_string()
        } else if re_hex(seg) {
            "{hex}".to_string()
        } else if re_long(seg) {
            "{token}".to_string()
        } else {
            seg.clone()
        };
    }
    Ok(segs.join("/"))
}

/// Port of `ValidNotifyMode` (`db/notification.go:49`).
pub fn valid_notify_mode(m: String) -> Result<bool, String> {
    Ok(m == "realtime" || m == "digest")
}

/// Port of `TargetKey` (`db/target_pins.go:40`).
pub fn target_key(host: String, port: i64) -> Result<String, String> {
    Ok(format!("{}:{}", host, port))
}

/// Port of `IsTaskArchiveFormatSupported` (`db/task_archives.go:24`).
pub fn is_task_archive_format_supported(version: i64) -> Result<bool, String> {
    Ok((1..=3).contains(&version))
}

/// Port of `IsTerminal` (`db/tasks.go:86`).
pub fn is_terminal(status: String) -> Result<bool, String> {
    Ok(status == "done" || status == "failed" || status == "timeout")
}
