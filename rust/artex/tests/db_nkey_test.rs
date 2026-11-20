//! Differential tests for `db/nkey.go` pure helpers — oracle captured live
//! from Go.

use artex::db::nkey::{
    default_port, domain_key, endpoint_key, ip_key, normalize_param_name, parameter_key, port_key,
    root, service_key, site_key, split_url, tech_key, template_path,
};

#[test]
fn domain_and_root_keys() {
    assert_eq!(domain_key("  EXAMPLE.COM.  "), "example.com");
    assert_eq!(domain_key("a.b.example.com"), "a.b.example.com");
    assert_eq!(domain_key(""), "");
    assert_eq!(ip_key(" 10.0.0.1 "), "10.0.0.1");
    assert_eq!(ip_key("::1"), "::1");

    let cases: &[(&str, &str, bool)] = &[
        ("10.0.0.1", "10.0.0.1", true),
        ("a.b.co.uk", "b.co.uk", false),
        ("a.b.example.com", "example.com", false),
        ("example.com", "example.com", true),
        ("localhost", "localhost", true),
    ];
    for (host, want_root, want_apex) in cases {
        assert_eq!(
            root(host),
            (want_root.to_string(), *want_apex),
            "RootDomain({host:?})"
        );
    }
}

#[test]
fn composite_keys() {
    assert_eq!(port_key(42, "TCP", 443), "42|tcp|443");
    assert_eq!(service_key(7, "HTTPS"), "7|https");
    assert_eq!(
        site_key("HTTPS", "EXAMPLE.com", 8443),
        "https|example.com|8443"
    );
    assert_eq!(endpoint_key(9, "get", "/a/{id}"), "9|GET|/a/{id}");
    assert_eq!(parameter_key(3, "QUERY", "UserId"), "3|query|UserId");
    assert_eq!(normalize_param_name(" UserId "), "userid");
    assert_eq!(tech_key("NGINX", "1.2"), "nginx|1.2");
}

#[test]
fn template_path_oracle() {
    let cases: &[(&str, &str)] = &[
        ("", "/"),
        ("/", "/"),
        ("/user/123", "/user/{id}"),
        ("/u/550e8400-e29b-41d4-a716-446655440000/x", "/u/{uuid}/x"),
        ("/f/0123456789abcdef0123456789abcdef", "/f/{hex}"),
        ("/t/abcdefghijklmnopqrstuvwxyz1", "/t/{token}"),
        ("/a/b-c_d?x=1", "/a/b-c_d?x=1"),
    ];
    for (input, want) in cases {
        assert_eq!(&template_path(input), want, "TemplatePath({input:?})");
    }
}

#[test]
fn split_url_oracle() {
    assert_eq!(default_port("https", ""), 443);
    assert_eq!(default_port("http", "8080"), 8080);
    assert_eq!(default_port("ftp", "9"), 9);
    assert_eq!(default_port("https", "bad"), 443);

    let (s, h, p, t, params) =
        split_url("https://A.Example.COM:8443/a/123?q=1&b=2", "GET").unwrap();
    assert_eq!(
        (s.as_str(), h.as_str(), p, t.as_str()),
        ("https", "a.example.com", 8443, "/a/{id}")
    );
    let mut params = params;
    params.sort();
    assert_eq!(params, vec!["b", "q"]);

    let (s, h, p, t, params) = split_url("http://h.example/x", "GET").unwrap();
    assert_eq!(
        (s.as_str(), h.as_str(), p, t.as_str()),
        ("http", "h.example", 80, "/x")
    );
    assert!(params.is_empty());

    // Go url.Parse accepts a relative reference without error.
    let (s, h, p, t, params) = split_url("not-a-url", "GET").unwrap();
    assert_eq!(
        (s.as_str(), h.as_str(), p, t.as_str()),
        ("", "", 0, "not-a-url")
    );
    assert!(params.is_empty());
}
