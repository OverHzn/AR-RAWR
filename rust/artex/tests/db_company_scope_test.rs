//! Differential tests for `db/company_scope.go` parsers — direct ports of
//! `TestParseScopeLine`, `TestParseAutoScopeLine`, `TestParseStructuredCompanyScope`.
//!
//! Pure — no Postgres needed.

use artex::db::company_scope::{
    normalize_icp, parse_auto_scope_line, parse_scope_input, parse_scope_line, ScopeInput,
};

fn normalized(r: &artex::db::company_scope::ParsedScope) -> String {
    match r.kind.as_str() {
        "domain" => r.domain.clone(),
        "ip" | "cidr" => r.net.clone(),
        _ => r.value.clone(),
    }
}

#[test]
fn parse_scope_line_cases() {
    // (input, kind, want_err) — from Go TestParseScopeLine.
    let cases: &[(&str, &str, bool)] = &[
        ("example.com", "domain", false),
        ("https://sub.example.com/path", "domain", false),
        ("1.2.3.4", "ip", false),
        ("10.0.0.0/8", "", true), // over-broad IPv4 (< /16)
        ("198.51.100.0/24", "cidr", false),
        ("co.uk", "", true), // bare public suffix
        ("not a host", "", true),
        ("1.2.3.1-1.2.3.9", "", true), // ranges must be CIDR
    ];
    for &(input, want_kind, want_err) in cases {
        let got = parse_scope_line(input);
        if want_err {
            assert!(
                got.is_err(),
                "ParseScopeLine({input:?}) should error: {got:?}"
            );
        } else {
            let r = got.unwrap_or_else(|e| panic!("ParseScopeLine({input:?}): {e}"));
            assert_eq!(r.kind, want_kind, "ParseScopeLine({input:?}) kind");
        }
    }
}

#[test]
fn parse_auto_scope_line_cases() {
    // (input, kind, normalized, want_err) — from Go TestParseAutoScopeLine.
    let cases: &[(&str, &str, &str, bool)] = &[
        ("example.com", "domain", "example.com", false),
        (
            "https://sub.example.com/path",
            "domain",
            "sub.example.com",
            false,
        ),
        (
            "https://example.com/path?source=x",
            "domain",
            "example.com",
            false,
        ),
        (
            "https://user:pass@example.com/path",
            "domain",
            "example.com",
            false,
        ),
        ("http://203.0.113.10/path", "ip", "203.0.113.10/32", false),
        ("203.0.113.10", "ip", "203.0.113.10/32", false),
        ("2001:db8::10", "ip", "2001:db8::10/128", false),
        ("198.51.100.0/24", "cidr", "198.51.100.0/24", false),
        (
            "Bei Jing ICP filing 123",
            "icp",
            "beijingicpfiling123",
            false,
        ),
        (
            "Shanghai network filing 9988",
            "icp",
            "shanghainetworkfiling9988",
            false,
        ),
        ("icp.example.com", "domain", "icp.example.com", false),
        (
            "https://example.com/path?icp=1",
            "domain",
            "example.com",
            false,
        ),
        (
            "ICP filing www.example.com",
            "keyword",
            "icp filing www.example.com",
            false,
        ),
        ("ACME Corp ICP v1.0", "keyword", "acme corp icp v1.0", false),
        (
            "ICP filing example\u{ff0e}com",
            "keyword",
            "icp filing example\u{ff0e}com",
            false,
        ),
        ("  ACME   Security  ", "keyword", "acme security", false),
        (
            "ACME: Cloud: Security",
            "keyword",
            "acme: cloud: security",
            false,
        ),
        ("  ", "", "", true),
        ("10.0.0.0/not-a-prefix", "", "", true),
        ("999.0.0.1", "", "", true),
        ("2001:db8::zz", "", "", true),
        ("2001:db8::zz/64", "", "", true),
        ("10.0.0.0/8", "", "", true),
        ("co.uk", "", "", true),
        ("foo..example.com", "", "", true),
    ];
    for &(input, want_kind, want_norm, want_err) in cases {
        let got = parse_auto_scope_line(input);
        if want_err {
            assert!(
                got.is_err(),
                "ParseAutoScopeLine({input:?}) should error: {got:?}"
            );
        } else {
            let r = got.unwrap_or_else(|e| panic!("ParseAutoScopeLine({input:?}): {e}"));
            assert_eq!(r.kind, want_kind, "ParseAutoScopeLine({input:?}) kind");
            assert_eq!(
                normalized(&r),
                want_norm,
                "ParseAutoScopeLine({input:?}) normalized"
            );
        }
    }
}

#[test]
fn parse_structured_scope_input_cases() {
    // From Go TestParseStructuredCompanyScope.
    let icp = parse_scope_input(ScopeInput {
        kind: "ICP".to_string(),
        value: " Beijing ICP filing 123-1\t".to_string(),
    })
    .unwrap();
    assert_eq!(icp.kind, "icp");
    assert_eq!(icp.value, "beijingicpfiling123-1");
    let kw = parse_scope_input(ScopeInput {
        kind: "keyword".to_string(),
        value: "  ACME   Security  ".to_string(),
    })
    .unwrap();
    assert_eq!(kw.value, "acme security");
    assert!(
        parse_scope_input(ScopeInput {
            kind: "ip".to_string(),
            value: "example.com".to_string(),
        })
        .is_err(),
        "typed IP must reject a domain"
    );
    // Explicit domain/ip/cidr kind agreement.
    assert_eq!(
        parse_scope_input(ScopeInput {
            kind: "domain".to_string(),
            value: "example.com".to_string(),
        })
        .unwrap()
        .domain,
        "example.com"
    );
    assert!(parse_scope_input(ScopeInput {
        kind: "cidr".to_string(),
        value: "1.2.3.4".to_string(),
    })
    .is_err());
    assert!(parse_scope_input(ScopeInput {
        kind: "bogus".to_string(),
        value: "x".to_string(),
    })
    .is_err());
    assert_eq!(normalize_icp(" A\u{00a0}B\tC\n"), "abc");
}
