//! Differential tests for `intercept/target_resolve.go` pure helpers — port of
//! `TestSubset` + canonical IP normalization.

use std::net::IpAddr;

use artex::intercept::target_resolve::{canonical_ips, subset};

#[test]
fn subset_cases() {
    let cases: &[(&str, &[&str], &[&str], bool)] = &[
        ("equal", &["203.0.113.7"], &["203.0.113.7"], true),
        (
            "subset",
            &["203.0.113.7"],
            &["203.0.113.7", "203.0.113.8"],
            true,
        ),
        (
            "superset",
            &["203.0.113.7", "203.0.113.8"],
            &["203.0.113.7"],
            false,
        ),
        ("disjoint", &["203.0.113.9"], &["203.0.113.7"], false),
        (
            "mixed all pinned",
            &["203.0.113.7", "2001:db8::7"],
            &["2001:db8::7", "203.0.113.7"],
            true,
        ),
        (
            "mixed one family pinned",
            &["203.0.113.7", "2001:db8::7"],
            &["203.0.113.7"],
            false,
        ),
        ("empty answer", &[], &["203.0.113.7"], false),
        ("empty answer empty pin", &[], &[], false),
        ("empty pin", &["203.0.113.7"], &[], false),
    ];
    for (name, resolved, pinned, want) in cases {
        let r: Vec<String> = resolved.iter().map(|s| s.to_string()).collect();
        let p: Vec<String> = pinned.iter().map(|s| s.to_string()).collect();
        assert_eq!(subset(&r, &p), *want, "{name}");
    }
}

#[test]
fn canonical_ips_mapped_sorted_unique() {
    let a: IpAddr = "203.0.113.7".parse().unwrap();
    let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
    let v6: IpAddr = "2001:db8::7".parse().unwrap();
    let got = canonical_ips(&[v6, a, mapped, a]);
    assert_eq!(
        got,
        vec!["2001:db8::7", "203.0.113.7"],
        "unmapped + sorted + deduped"
    );
    assert!(canonical_ips(&[]).is_empty());
}
