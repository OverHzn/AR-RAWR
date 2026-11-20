//! Differential tests for pure `db/assets.go` + `db/nkey.go` helpers — oracle
//! values captured live from Go.

use artex::db::asset_helpers::{
    calc_c_segment, default_port, marshal_jsonb_array, marshal_port_services, marshal_string_array,
    normalize_url, parse_url, validate_asset_ip, PortService, ERR_ASSET_IP_INVALID,
};

#[test]
fn calc_c_segment_oracle() {
    assert_eq!(calc_c_segment("10.1.2.3"), "10.1.2.0/24");
    assert_eq!(calc_c_segment("2001:db8::1"), "2001:db8::/48");
    assert_eq!(calc_c_segment("bad"), "");
    assert_eq!(calc_c_segment(""), "");
    assert_eq!(calc_c_segment("1.2.3"), "");
}

#[test]
fn url_helpers_oracle() {
    assert_eq!(default_port("https", ""), 443);
    assert_eq!(default_port("http", "8080"), 8080);
    assert_eq!(default_port("ftp", ""), 0);

    assert_eq!(
        normalize_url("HTTPS://User:pw@Example.COM:8443/path?q=1"),
        "https://example.com:8443/path?q=1"
    );
    assert_eq!(normalize_url("http://example.com/"), "http://example.com");
    assert_eq!(normalize_url("https://x.example"), "https://x.example");
    assert_eq!(normalize_url("bad-url"), "bad-url");

    assert_eq!(
        parse_url("HTTPS://User:pw@Example.COM:8443/path?q=1"),
        ("example.com".to_string(), 8443, "HTTPS".to_string())
    );
    assert_eq!(
        parse_url("http://example.com/"),
        ("example.com".to_string(), 80, "HTTP".to_string())
    );
    assert_eq!(
        parse_url("https://x.example"),
        ("x.example".to_string(), 443, "HTTPS".to_string())
    );
    assert_eq!(parse_url("bad-url"), (String::new(), 0, String::new()));
}

#[test]
fn array_literals_oracle() {
    let mj = marshal_jsonb_array(&[
        serde_json::json!({"port": 80, "service": "http"}),
        serde_json::json!({"a": "b\"c"}),
    ])
    .unwrap();
    assert_eq!(
        mj,
        "{\"{\\\"port\\\":80,\\\"service\\\":\\\"http\\\"}\",\"{\\\"a\\\":\\\"b\\\\\\\"c\\\"}\"}"
    );
    assert_eq!(marshal_jsonb_array(&[]).unwrap(), "{}");

    assert_eq!(
        marshal_string_array(&["a".into(), "b\"c".into(), "d\\e".into()]),
        "{\"a\",\"b\\\"c\",\"d\\\\e\"}"
    );
    assert_eq!(marshal_string_array(&[]), "{}");

    let mp = marshal_port_services(&[
        PortService {
            port: 443,
            service: "https".into(),
        },
        PortService {
            port: 22,
            service: String::new(),
        },
    ])
    .unwrap();
    assert_eq!(mp, "{\"{\\\"port\\\":443,\\\"service\\\":\\\"https\\\"}\",\"{\\\"port\\\":22,\\\"service\\\":\\\"\\\"}\"}");
}

#[test]
fn validate_asset_ip_cases() {
    assert!(validate_asset_ip("").is_ok());
    assert!(validate_asset_ip("10.0.0.1").is_ok());
    assert!(validate_asset_ip("2001:db8::1").is_ok());
    let err = validate_asset_ip("example.com").unwrap_err();
    assert!(err.is(ERR_ASSET_IP_INVALID), "got {err}");
    assert!(err.to_string().contains("use type=subdomain"));
}
