//! Differential tests for `intercept/target_exempt.go` — port of
//! `TestValidateExemptionHost` + `TestDerivedExemptionsDefaultProxyPort`. Pure.

use artex::intercept::target_exempt::{
    derived_exemptions, validate_exemption_host, voids_exemption, Exemption,
};

#[test]
fn validate_exemption_host_cases() {
    // Allowed: private unicast, public, and names.
    for host in [
        "10.0.0.5",
        "172.16.0.1",
        "192.168.1.1",
        "fd00::1",
        "203.0.113.7",
        "2001:db8::1",
        "pypi.org",
        "internal.corp",
        "localhost",
    ] {
        assert!(validate_exemption_host(host).is_ok(), "{host} refused");
    }
    // Refused: link-local/metadata, loopback, unspecified, multicast.
    for host in [
        "169.254.169.254",
        "169.254.170.2",
        "169.254.0.1",
        "fe80::1",
        "fe80::1%eth0",
        "::ffff:169.254.169.254",
        "fd00:ec2::254",
        "100.100.100.200",
        "127.0.0.1",
        "127.8.8.8",
        "::1",
        "::ffff:127.0.0.1",
        "0.0.0.0",
        "::",
        "224.0.0.1",
        "239.255.255.250",
        "ff02::1",
        "ff05::2",
    ] {
        assert!(
            validate_exemption_host(host).is_err(),
            "{host} accepted, want refused"
        );
    }
}

#[test]
fn derived_exemptions_default_proxy_port() {
    let endpoints: Vec<String> = [
        "socks5://egress.example.net",
        "SOCKS5H://dns.example.net",
        "http://proxy.example.net",
        "https://gw.example.net",
        "bare.example.net",
        "socks5://set.example.net:9050",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let got = derived_exemptions(&endpoints);
    let want = vec![
        Exemption {
            host: "egress.example.net".to_string(),
            port: 1080,
        },
        Exemption {
            host: "dns.example.net".to_string(),
            port: 1080,
        },
        Exemption {
            host: "proxy.example.net".to_string(),
            port: 80,
        },
        Exemption {
            host: "gw.example.net".to_string(),
            port: 443,
        },
        Exemption {
            host: "set.example.net".to_string(),
            port: 9050,
        },
    ];
    assert_eq!(got, want);
}

#[test]
fn voids_exemption_cases() {
    assert!(voids_exemption("127.0.0.1"), "loopback voids");
    assert!(voids_exemption("169.254.169.254"), "metadata voids");
    assert!(voids_exemption("10.0.0.5"), "private voids");
    assert!(voids_exemption("not-an-ip"), "unparseable voids");
    assert!(!voids_exemption("203.0.113.7"), "public holds");
}
