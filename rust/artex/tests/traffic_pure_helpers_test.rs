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
