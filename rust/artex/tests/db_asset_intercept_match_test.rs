//! Pure matching/gate specs for asset interception — port of the Go
//! `db/asset_intercept_match_test.go` (`TestMatchAssetInterceptRules` +
//! `TestEvaluateAssetGate`). `TestAssetInterceptCandidates` needs the `Asset`
//! type and is deferred to the assets group.
//!
//! These are pure functions (no DB), so the suite runs unconditionally.

use artex::db::asset_intercept::AssetInterceptRule;
use artex::db::asset_intercept_match::{
    asset_intercept_kind_label, evaluate_asset_gate, match_asset_intercept_rules,
};

fn rule(kind: &str, pattern: &str, enabled: bool) -> AssetInterceptRule {
    AssetInterceptRule {
        kind: kind.to_string(),
        pattern: pattern.to_string(),
        enabled,
        ..Default::default()
    }
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Port of Go `TestMatchAssetInterceptRules`.
#[test]
fn match_asset_intercept_rules_cases() {
    struct Case {
        name: &'static str,
        rules: Vec<AssetInterceptRule>,
        domains: Vec<String>,
        ips: Vec<String>,
        urls: Vec<String>,
        want: bool,
        want_val: &'static str,
    }
    let empty: Vec<String> = Vec::new();

    let cases = vec![
        Case {
            name: "built-in fuzzy government domain hit",
            rules: vec![rule("fuzzy_domain", ".gov.cn", true)],
            domains: v(&["www.beijing.gov.cn"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: true,
            want_val: "www.beijing.gov.cn",
        },
        Case {
            name: "fuzzy education domain hit",
            rules: vec![rule("fuzzy_domain", ".edu", true)],
            domains: v(&["mit.edu"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: true,
            want_val: "mit.edu",
        },
        Case {
            name: "exact domain hit is case-insensitive",
            rules: vec![rule("exact_domain", "Example.com", true)],
            domains: v(&["example.com"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: true,
            want_val: "example.com",
        },
        Case {
            name: "exact domain does not hit subdomain",
            rules: vec![rule("exact_domain", "example.com", true)],
            domains: v(&["a.example.com"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: false,
            want_val: "",
        },
        Case {
            name: "exact IP hit",
            rules: vec![rule("exact_ip", "203.0.113.5", true)],
            domains: empty.clone(),
            ips: v(&["203.0.113.5"]),
            urls: empty.clone(),
            want: true,
            want_val: "203.0.113.5",
        },
        Case {
            name: "fuzzy IP prefix hit",
            rules: vec![rule("fuzzy_ip", "203.0.113.", true)],
            domains: empty.clone(),
            ips: v(&["203.0.113.99"]),
            urls: empty.clone(),
            want: true,
            want_val: "203.0.113.99",
        },
        Case {
            name: "CIDR hit",
            rules: vec![rule("cidr", "192.168.0.0/16", true)],
            domains: empty.clone(),
            ips: v(&["192.168.5.20"]),
            urls: empty.clone(),
            want: true,
            want_val: "192.168.5.20",
        },
        Case {
            name: "CIDR no hit",
            rules: vec![rule("cidr", "192.168.0.0/16", true)],
            domains: empty.clone(),
            ips: v(&["10.0.0.1"]),
            urls: empty.clone(),
            want: false,
            want_val: "",
        },
        Case {
            name: "exact URL hit",
            rules: vec![rule("exact_url", "https://a.gov.cn/login", true)],
            domains: empty.clone(),
            ips: empty.clone(),
            urls: v(&["https://a.gov.cn/login"]),
            want: true,
            want_val: "https://a.gov.cn/login",
        },
        Case {
            name: "fuzzy URL hits path",
            rules: vec![rule("fuzzy_url", "/admin", true)],
            domains: empty.clone(),
            ips: empty.clone(),
            urls: v(&["https://x.com/admin/panel"]),
            want: true,
            want_val: "https://x.com/admin/panel",
        },
        Case {
            name: "disabled rule does not hit",
            rules: vec![rule("fuzzy_domain", ".gov.cn", false)],
            domains: v(&["www.gov.cn"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: false,
            want_val: "",
        },
        Case {
            name: "no rules no hit",
            rules: vec![],
            domains: v(&["www.gov.cn"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: false,
            want_val: "",
        },
        Case {
            name: "empty pattern no hit",
            rules: vec![rule("fuzzy_domain", "  ", true)],
            domains: v(&["www.gov.cn"]),
            ips: empty.clone(),
            urls: empty.clone(),
            want: false,
            want_val: "",
        },
    ];

    for c in &cases {
        let (_r, val, ok) = match_asset_intercept_rules(&c.rules, &c.domains, &c.ips, &c.urls);
        assert_eq!(ok, c.want, "case {:?}: hit mismatch", c.name);
        if ok {
            assert_eq!(val, c.want_val, "case {:?}: matched value", c.name);
        }
    }
}

/// Port of Go `TestEvaluateAssetGate`.
#[test]
fn evaluate_asset_gate_cases() {
    let block = vec![rule("fuzzy_domain", ".gov.cn", true)];
    let allow = vec![rule("fuzzy_domain", "example.com", true)];
    let none: Vec<String> = Vec::new();

    // 1. Interception rule hit → reject (interception reason takes priority).
    let d = evaluate_asset_gate(&block, &allow, &v(&["www.gov.cn"]), &none, &none);
    assert!(
        !d.allowed,
        "hitting an interception rule should be rejected"
    );
    assert!(
        !d.reason.is_empty(),
        "interception rejection carries a reason"
    );

    // 2. No interception hit, allow rules exist but do not match → reject.
    let d = evaluate_asset_gate(&block, &allow, &v(&["foo.other.com"]), &none, &none);
    assert!(!d.allowed, "whitelist without a match should be rejected");
    assert!(!d.reason.is_empty(), "rejection should carry a reason");

    // 3. No interception hit, allow rule hit → allow.
    let d = evaluate_asset_gate(&block, &allow, &v(&["api.example.com"]), &none, &none);
    assert!(d.allowed, "hitting the whitelist should be allowed");

    // 4. No allow rules (whitelist not enabled) → no interception hit means allow.
    let d = evaluate_asset_gate(&block, &[], &v(&["foo.other.com"]), &none, &none);
    assert!(
        d.allowed,
        "with no whitelist, missing the interception rules should be allowed"
    );

    // 5. All allow rules disabled → treated as whitelist not enabled, allow.
    let disabled_allow = vec![rule("fuzzy_domain", "example.com", false)];
    let d = evaluate_asset_gate(&[], &disabled_allow, &v(&["foo.other.com"]), &none, &none);
    assert!(
        d.allowed,
        "with the whitelist fully disabled, should be allowed"
    );

    // 6. Interception beats allow: the same target hits both → reject.
    let d = evaluate_asset_gate(
        &[rule("fuzzy_domain", ".gov.cn", true)],
        &[rule("fuzzy_domain", ".gov.cn", true)],
        &v(&["www.gov.cn"]),
        &none,
        &none,
    );
    assert!(!d.allowed, "interception should take priority over allow");
}

/// `AssetInterceptKindLabel` is the gate-pinned `db-rs` helper: labels the known
/// kinds and passes unknown kinds through unchanged.
#[test]
fn kind_label_known_and_passthrough() {
    assert_eq!(asset_intercept_kind_label("exact_domain"), "Domain (exact)");
    assert_eq!(asset_intercept_kind_label("cidr"), "CIDR range");
    assert_eq!(asset_intercept_kind_label("fuzzy_url"), "URL (fuzzy)");
    assert_eq!(asset_intercept_kind_label("weird_kind"), "weird_kind");
}

/// `Reason` formats `Matched asset interception rule [Label: pattern] (note)`.
#[test]
fn rule_reason_format() {
    let r = AssetInterceptRule {
        kind: "fuzzy_domain".into(),
        pattern: ".gov.cn".into(),
        note: "govt blocklist".into(),
        ..Default::default()
    };
    assert_eq!(
        r.reason(),
        "Matched asset interception rule [Domain (fuzzy): .gov.cn] (govt blocklist)"
    );
    // Empty/whitespace note is omitted.
    let r2 = AssetInterceptRule {
        kind: "exact_ip".into(),
        pattern: "10.0.0.1".into(),
        note: "   ".into(),
        ..Default::default()
    };
    assert_eq!(
        r2.reason(),
        "Matched asset interception rule [IP (exact): 10.0.0.1]"
    );
}
