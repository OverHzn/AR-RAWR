//! Differential tests for pure `egress` helpers — ports of Go
//! `TestMarkProxyURL`, `TestIsPaceMarker`, profile/env cases from
//! `TestUserAgentAndEnv`, `HostKey` cases and `TestBackoff` (pure core).

use std::time::{Duration, SystemTime};

use artex::egress::{
    backoff, host_key, is_pace_marker, mark_proxy_url, profile_by_name, profile_from_env, Profile,
};

fn enc(s: &str) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, s)
}

#[test]
fn mark_proxy_url_cases() {
    let cases: &[(&str, &str)] = &[
        (
            "http://127.0.0.1:8788",
            "http://artex-pace:artex-pace@127.0.0.1:8788",
        ),
        ("http://u:p@127.0.0.1:8788", "http://u:p@127.0.0.1:8788"),
        ("", ""),
        ("127.0.0.1:8788", "127.0.0.1:8788"),
        (
            "socks5://proxy.example.test:1080",
            "socks5://artex-pace:artex-pace@proxy.example.test:1080",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(&mark_proxy_url(input), want, "MarkProxyURL({input:?})");
    }
}

#[test]
fn is_pace_marker_cases() {
    let values: Vec<(String, bool)> = vec![
        (format!("Basic {}", enc("artex-pace:artex-pace")), true),
        (format!("basic {}", enc("artex-pace:x")), true),
        (format!("Basic {}", enc("user:pass")), false),
        (format!("Bearer {}", enc("artex-pace:artex-pace")), false),
        ("Basic !!!".to_string(), false),
        ("Basic".to_string(), false),
        (String::new(), false),
    ];
    for (v, want) in &values {
        assert_eq!(is_pace_marker(v), *want, "IsPaceMarker({v:?})");
    }
}

#[test]
fn profile_lookup_and_env() {
    let (off, ok) = profile_by_name("OFF ", "");
    assert!(ok);
    assert_eq!(off, Profile::default());
    let (bad, ok) = profile_by_name("loud", "");
    assert!(!ok);
    assert_eq!(bad, Profile::default());

    let (p, ok) = profile_by_name("Balanced", "x/1");
    assert!(ok);
    assert_eq!(p.name, "balanced");
    assert_eq!(p.rps, 5.0);
    assert_eq!(p.burst, 10);
    assert_eq!(p.cache_ttl, Duration::from_secs(30));
    assert_eq!(p.user_agent, "x/1");

    // A UA with a control character is ignored.
    let (p2, _) = profile_by_name("balanced", "x/\u{1}1");
    assert_eq!(p2.user_agent, "");

    // Env core: unset/unknown -> off.
    assert_eq!(profile_from_env("", "x/1").name, "off");
    assert_eq!(profile_from_env("loud", "x/1").name, "off");
    assert_eq!(profile_from_env("Balanced", "x/1"), p);
}

#[test]
fn host_key_cases() {
    let cases: &[(&str, &str)] = &[
        ("http://Example.COM:80/x", "example.com"),
        ("https://a.test:8443/", "a.test:8443"),
        ("https://Secure.Example/", "secure.example"),
        ("http://h.test:8080/", "h.test:8080"),
    ];
    for (raw, want) in cases {
        let u = url::Url::parse(raw).unwrap();
        assert_eq!(host_key(&u), *want, "HostKey({raw:?})");
    }
}

#[test]
fn backoff_cases() {
    let p = Profile {
        name: "balanced".into(),
        max_delay: Duration::from_secs(30),
        base_delay: Duration::from_secs(1),
        ..Profile::default()
    };
    let now = SystemTime::UNIX_EPOCH;

    let (d, ok) = backoff(Some("1"), 0, &p, now);
    assert_eq!(d, Duration::from_secs(1));
    assert!(ok);
    let (d, ok) = backoff(Some("3600"), 0, &p, now);
    assert_eq!(d, Duration::from_secs(3600));
    assert!(!ok, "Retry-After beyond MaxDelay must not be retried");
    let (d, ok) = backoff(Some("!!!"), 0, &p, now);
    assert_eq!(d, Duration::from_secs(1)); // falls back to doubling
    assert!(ok);

    // Doubling with cap: attempt 0 -> 1s, 1 -> 2s, 5 -> capped 30s.
    assert_eq!(backoff(None, 0, &p, now).0, Duration::from_secs(1));
    assert_eq!(backoff(None, 1, &p, now).0, Duration::from_secs(2));
    assert_eq!(backoff(None, 5, &p, now).0, Duration::from_secs(30));

    // HTTP-date: "Mon, 02 Jan 2006 15:04:05 GMT" is epoch 1136214245; 2s later.
    let target = SystemTime::UNIX_EPOCH + Duration::from_secs(1_136_214_245);
    let near = target - Duration::from_secs(2);
    let (d, ok) = backoff(Some("Mon, 02 Jan 2006 15:04:05 GMT"), 0, &p, near);
    assert!(ok);
    assert!(
        d >= Duration::from_secs(1) && d <= Duration::from_secs(2),
        "HTTP-date backoff = {d:?}"
    );
}
