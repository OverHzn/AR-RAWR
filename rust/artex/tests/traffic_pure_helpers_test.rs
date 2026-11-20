//! Differential tests for pure `traffic` helpers — ports of Go
//! `TestClampBlobRange`, `TestHostOnly`, `TestValidateProxyURL`, `TestProxyAddr`.

use artex::traffic::pure_helpers::{clamp_blob_range, host_only, proxy_addr, validate_proxy_url};

#[test]
fn clamp_blob_range_cases() {
    let cases: &[(&str, i64, i64, i64, i64, i64, bool)] = &[
        ("zero length means rest", 2, 0, 10, 2, 8, false),
        ("negative length means rest", 2, -1, 10, 2, 8, false),
        ("exact rest", 2, 8, 10, 2, 8, false),
        ("past end", 2, 100, 10, 2, 8, false),
        ("offset at end", 10, 4, 10, 10, 0, true),
        ("offset past end", 11, 4, 10, 11, 0, true),
        ("negative offset", -3, 4, 10, 0, 4, false),
        ("empty blob", 0, 10, 0, 0, 0, true),
        ("overflow length", 1, i64::MAX, 10, 1, 9, false),
    ];
    for (name, off, len, size, want_off, want_n, want_empty) in cases {
        assert_eq!(
            clamp_blob_range(*off, *len, *size),
            (*want_off, *want_n, *want_empty),
            "{name}"
        );
    }
}

#[test]
fn host_only_cases() {
    for (input, want) in [
        ("example.com:443", "example.com"),
        ("example.com", "example.com"),
        ("10.0.0.1:8080", "10.0.0.1"),
        ("[2001:db8::1]:443", "2001:db8::1"),
        ("2001:db8::1", "2001:db8::1"),
    ] {
        assert_eq!(host_only(input), want, "hostOnly({input:?})");
    }
}

#[test]
fn proxy_addr_cases() {
    assert_eq!(proxy_addr(":8788"), "http://127.0.0.1:8788");
    assert_eq!(proxy_addr("127.0.0.1:8788"), "http://127.0.0.1:8788");
    assert_eq!(proxy_addr("0.0.0.0:8788"), "http://0.0.0.0:8788");
}

#[test]
fn validate_proxy_url_cases() {
    for raw in [
        "http://127.0.0.1:8080",
        "https://proxy.example.com:3128",
        "socks5://10.0.0.1:1080",
        "socks5://user:pass@10.0.0.1:1080",
    ] {
        assert!(
            validate_proxy_url(raw).is_ok(),
            "ValidateProxyURL({raw:?}) should be ok"
        );
    }
    for raw in [
        "127.0.0.1:8080",
        "ftp://host:21",
        "http://",
        "socks4://10.0.0.1:1080",
    ] {
        let err = validate_proxy_url(raw).unwrap_err();
        assert!(
            err.to_string().contains("missing a scheme")
                || err.to_string().contains("unsupported proxy scheme")
                || err.to_string().contains("missing a host")
                || err.to_string().contains("parse proxy address"),
            "ValidateProxyURL({raw:?}) = {err}"
        );
    }
}

#[test]
fn body_and_name_helpers_oracle() {
    use artex::traffic::pure_helpers::{binary_tag, is_binary_body, sanitize, truncate_utf8};

    assert!(is_binary_body("image/png", b""));
    assert!(!is_binary_body("TEXT/PLAIN; charset=utf-8", b"hello"));
    assert!(is_binary_body("application/json", b"a\0b"));
    let mut long = vec![0u8; 600];
    long.push(0);
    assert!(is_binary_body("text/html", &long));

    assert_eq!(
        binary_tag("", &[0x89, b'P', b'N', b'G', 1, 2]),
        "[binary application/octet-stream, 6 bytes, magic=89504e47]"
    );
    assert_eq!(
        binary_tag(" image/jpeg ", &[0xff, 0xd8]),
        "[binary image/jpeg, 2 bytes, magic=ffd8]"
    );

    assert_eq!(truncate_utf8("héllo".as_bytes(), 3), "hé");
    assert_eq!(truncate_utf8("你好世界".as_bytes(), 5), "你");
    assert_eq!(truncate_utf8(b"abc", 10), "abc");

    assert_eq!(sanitize(""), "root");
    assert_eq!(sanitize("_"), "root");
    assert_eq!(sanitize("a/b\\c:d?e*f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
    assert_eq!(sanitize(&"x".repeat(200)).len(), 120);
}

#[test]
fn header_lines_and_applied_rules_oracle() {
    use artex::traffic::pure_helpers::{applied_rules, header_lines};
    use artex::traffic::rewrite_engine::{TraceLine, OUTCOME_APPLIED, OUTCOME_SKIPPED};

    let mut h = std::collections::BTreeMap::new();
    h.insert("X-A".to_string(), vec!["1".to_string()]);
    h.insert("X-B".to_string(), vec!["2".to_string(), "3".to_string()]);
    let lines = header_lines(&h);
    let mut got: Vec<&str> = lines.lines().collect();
    got.sort();
    assert_eq!(got, vec!["X-A: 1", "X-B: 2", "X-B: 3"]);

    let trace = vec![
        TraceLine {
            rule_id: 1,
            outcome: OUTCOME_APPLIED.into(),
            ..Default::default()
        },
        TraceLine {
            rule_id: 2,
            outcome: OUTCOME_SKIPPED.into(),
            ..Default::default()
        },
        TraceLine {
            rule_id: 3,
            outcome: OUTCOME_APPLIED.into(),
            ..Default::default()
        },
        TraceLine {
            rule_id: 3,
            outcome: OUTCOME_APPLIED.into(),
            ..Default::default()
        },
        TraceLine {
            rule_id: 0,
            outcome: OUTCOME_APPLIED.into(),
            ..Default::default()
        },
    ];
    assert_eq!(applied_rules(&trace), (true, vec![1, 3]));
}
