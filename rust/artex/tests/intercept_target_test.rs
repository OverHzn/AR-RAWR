//! Differential tests for `intercept/target.go` — port of Go `TestNormalizeTarget`
//! + `validHostname`. Pure — no Postgres needed.

use artex::intercept::target::{
    normalize_target, valid_hostname, Target, TARGET_CIDR, TARGET_DOMAIN, TARGET_IP,
};

fn t(kind: &str, host: &str, port: i32) -> Target {
    Target {
        kind: kind.to_string(),
        host: host.to_string(),
        port,
    }
}

#[test]
fn normalize_target_cases() {
    // (name, raw, want) — from Go TestNormalizeTarget.
    let cases: &[(&str, &str, Target)] = &[
        (
            "case",
            "HTTPS://App.Example.COM/Admin",
            t(TARGET_DOMAIN, "app.example.com", 443),
        ),
        (
            "trailing dot",
            "https://app.example.com./",
            t(TARGET_DOMAIN, "app.example.com", 443),
        ),
        (
            "userinfo",
            "https://user:p@ss@app.example.com/x",
            t(TARGET_DOMAIN, "app.example.com", 443),
        ),
        (
            "userinfo that looks like a host",
            "http://pinned.example@evil.example/",
            t(TARGET_DOMAIN, "evil.example", 80),
        ),
        (
            "ipv6 brackets",
            "http://[2001:DB8::1]/",
            t(TARGET_IP, "2001:db8::1", 80),
        ),
        (
            "ipv6 brackets and port",
            "http://[2001:db8::1]:8443/x",
            t(TARGET_IP, "2001:db8::1", 8443),
        ),
        (
            "ipv4-mapped ipv6",
            "http://[::ffff:203.0.113.7]/",
            t(TARGET_IP, "203.0.113.7", 80),
        ),
        (
            "decimal ipv4",
            "http://2130706433/",
            t(TARGET_IP, "127.0.0.1", 80),
        ),
        (
            "hex ipv4",
            "http://0x7f000001/",
            t(TARGET_IP, "127.0.0.1", 80),
        ),
        (
            "octal ipv4",
            "http://0177.0.0.1/",
            t(TARGET_IP, "127.0.0.1", 80),
        ),
        ("short ipv4", "http://127.1/", t(TARGET_IP, "127.0.0.1", 80)),
        (
            "mixed ipv4 forms",
            "http://0xa9.254.0xa9fe/",
            t(TARGET_IP, "169.254.169.254", 80),
        ),
        (
            "punycode",
            "https://bücher.example/",
            t(TARGET_DOMAIN, "xn--bcher-kva.example", 443),
        ),
        (
            "punycode kept",
            "https://xn--bcher-kva.example/",
            t(TARGET_DOMAIN, "xn--bcher-kva.example", 443),
        ),
        (
            "default port http",
            "http://app.example.com",
            t(TARGET_DOMAIN, "app.example.com", 80),
        ),
        (
            "default port https",
            "https://app.example.com?x=1",
            t(TARGET_DOMAIN, "app.example.com", 443),
        ),
        (
            "default port ws",
            "ws://app.example.com/socket",
            t(TARGET_DOMAIN, "app.example.com", 80),
        ),
        (
            "default port wss",
            "wss://app.example.com/socket",
            t(TARGET_DOMAIN, "app.example.com", 443),
        ),
        (
            "default port ftp",
            "ftp://files.example.com/pub",
            t(TARGET_DOMAIN, "files.example.com", 21),
        ),
        (
            "explicit port wins",
            "https://app.example.com:8443/",
            t(TARGET_DOMAIN, "app.example.com", 8443),
        ),
        (
            "empty port is the default",
            "https://app.example.com:/",
            t(TARGET_DOMAIN, "app.example.com", 443),
        ),
        (
            "port 0: scheme without a default",
            "ssh://git.example.com/repo",
            t(TARGET_DOMAIN, "git.example.com", 0),
        ),
        (
            "port 0: bare host",
            "app.example.com",
            t(TARGET_DOMAIN, "app.example.com", 0),
        ),
        (
            "host and port",
            "App.Example.com:8080",
            t(TARGET_DOMAIN, "app.example.com", 8080),
        ),
        (
            "ip and port",
            "203.0.113.7:22",
            t(TARGET_IP, "203.0.113.7", 22),
        ),
        ("bare ipv6", "2001:db8::1", t(TARGET_IP, "2001:db8::1", 0)),
        (
            "ipv6 zone dropped",
            "fe80::1%eth0",
            t(TARGET_IP, "fe80::1", 0),
        ),
        (
            "cidr is masked",
            "10.1.2.3/8",
            t(TARGET_CIDR, "10.0.0.0/8", 0),
        ),
        ("ipv6 cidr", "FD00::/8", t(TARGET_CIDR, "fd00::/8", 0)),
        (
            "underscore label",
            "http://my_host.internal/",
            t(TARGET_DOMAIN, "my_host.internal", 80),
        ),
        (
            "percent-encoded host",
            "http://%65vil.example/",
            t(TARGET_DOMAIN, "evil.example", 80),
        ),
        (
            "percent-encoded address",
            "http://127.0.0.1%2e/x",
            t(TARGET_IP, "127.0.0.1", 80),
        ),
        (
            "a host no profile accepts is kept",
            "https://-odd-.example/",
            t(TARGET_DOMAIN, "-odd-.example", 443),
        ),
    ];
    for (name, raw, want) in cases {
        let got = normalize_target(raw);
        assert_eq!(
            got.as_ref(),
            Some(want),
            "{name}: NormalizeTarget({raw:?}) = {got:?}"
        );
    }
    for raw in [
        "",
        "file:///etc/passwd",
        "http:///path",
        "http://app.example.com:99999/",
        "http://app.example.com:http/",
        "http://[::1",
        "10.0.0.0/99",
    ] {
        assert!(
            normalize_target(raw).is_none(),
            "NormalizeTarget({raw:?}) should be no target"
        );
    }
}

#[test]
fn valid_hostname_cases() {
    assert!(valid_hostname("app.example.com"));
    assert!(valid_hostname("my_host.internal"));
    assert!(!valid_hostname("-odd-.example"));
    assert!(!valid_hostname(""), "empty");
    assert!(!valid_hostname("a..b"), "empty label");
    assert!(!valid_hostname("*.example.com"), "wildcard");
    assert!(!valid_hostname(&"a".repeat(64)), "label > 63");
}
