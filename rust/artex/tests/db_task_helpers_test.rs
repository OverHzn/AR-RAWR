//! Differential tests for pure `db/task_assets.go` + `db/task_scope.go`
//! helpers — oracle captured live from Go.

use artex::db::error::Error as DbError;
use artex::db::task_helpers::{
    asset_key, company_key, coverage_node_label, host_port_of, ip_to_host_cidr,
    normalize_task_asset_ids, normalize_task_asset_source, strip_host_port, CoverageGraphNode,
    ERR_TASK_ASSET_INVALID, MAX_TASK_ASSET_MUTATION_COUNT, MAX_TASK_ASSET_SUMMARY_RUNES,
};

#[test]
fn strip_host_port_oracle() {
    let cases: &[(&str, &str)] = &[
        ("10.0.188.136:3000", "10.0.188.136"),
        ("10.0.188.136", "10.0.188.136"),
        ("[2001:db8::1]:8080", "2001:db8::1"),
        ("2001:db8::1", "2001:db8::1"),
        (" 1.2.3.4:80 ", "1.2.3.4"),
        ("example.com:443", "example.com"),
        ("api.example.com:8080", "api.example.com"),
        ("example.com", "example.com"),
    ];
    for (input, want) in cases {
        assert_eq!(&strip_host_port(input), want, "stripHostPort({input:?})");
    }
}

#[test]
fn ip_and_key_helpers_oracle() {
    let cases: &[(&str, &str)] = &[
        ("10.0.0.1", "10.0.0.1/32"),
        ("2001:db8::1", "2001:db8::1/128"),
        ("bad", ""),
        (" 1.2.3.4 ", "1.2.3.4/32"),
    ];
    for (input, want) in cases {
        assert_eq!(&ip_to_host_cidr(input), want, "ipToHostCIDR({input:?})");
    }
    assert_eq!(asset_key(42), "a:42");
    assert_eq!(company_key(7), "c:7");
}

#[test]
fn task_asset_normalization_oracle() {
    assert_eq!(
        normalize_task_asset_ids(&[3, 1, 3, 2]).unwrap(),
        vec![3, 1, 2]
    );
    let empty = normalize_task_asset_ids(&[]).unwrap_err();
    assert!(empty.is(ERR_TASK_ASSET_INVALID));
    assert_eq!(
        empty.to_string(),
        "invalid task asset association: asset_ids is required"
    );
    let neg = normalize_task_asset_ids(&[1, -2]).unwrap_err();
    assert_eq!(
        neg.to_string(),
        "invalid task asset association: asset id must be positive"
    );
    let many: Vec<i64> = (1..=MAX_TASK_ASSET_MUTATION_COUNT as i64 + 1).collect();
    assert_eq!(
        normalize_task_asset_ids(&many).unwrap_err().to_string(),
        "invalid task asset association: at most 100 assets per request"
    );

    let (source, summary) = normalize_task_asset_source("  AGENT ", "  summary  ").unwrap();
    assert_eq!(source, "agent");
    assert_eq!(summary, "summary");
    let (source, _) = normalize_task_asset_source("", "").unwrap();
    assert_eq!(source, "system");
    let long = "x".repeat(MAX_TASK_ASSET_SUMMARY_RUNES + 1);
    assert_eq!(
        normalize_task_asset_source("a", &long)
            .unwrap_err()
            .to_string(),
        "invalid task asset association: source summary exceeds 500 characters"
    );
    let _: Option<DbError> = None;
}

#[test]
fn coverage_node_helpers_oracle() {
    let endpoint = CoverageGraphNode {
        kind: "endpoint".into(),
        url: "http://a/x".into(),
        key: "e:1".into(),
        ..Default::default()
    };
    let app = CoverageGraphNode {
        kind: "app".into(),
        app_name: "MyApp".into(),
        key: "a:1".into(),
        ..Default::default()
    };
    let ip = CoverageGraphNode {
        kind: "ip".into(),
        ip: "10.0.0.1".into(),
        key: "i:1".into(),
        ..Default::default()
    };
    let other = CoverageGraphNode {
        kind: "other".into(),
        key: "x:1".into(),
        ..Default::default()
    };
    assert_eq!(coverage_node_label(&endpoint), "http://a/x");
    assert_eq!(coverage_node_label(&app), "MyApp");
    assert_eq!(coverage_node_label(&ip), "10.0.0.1");
    assert_eq!(coverage_node_label(&other), "x:1");

    assert_eq!(
        host_port_of(&CoverageGraphNode {
            domain: "a.example".into(),
            port: 8443,
            ..Default::default()
        }),
        ("a.example".to_string(), 8443)
    );
    assert_eq!(
        host_port_of(&CoverageGraphNode {
            url: "https://b.example:9443/x".into(),
            ..Default::default()
        }),
        ("b.example".to_string(), 9443)
    );
    assert_eq!(
        host_port_of(&CoverageGraphNode {
            ip: "10.0.0.1".into(),
            ..Default::default()
        }),
        ("10.0.0.1".to_string(), 0)
    );
}
