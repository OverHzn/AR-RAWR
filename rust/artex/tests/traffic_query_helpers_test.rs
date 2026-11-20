//! Differential tests for `traffic` query/validation helpers — oracle captured
//! live from Go (`ftsQuote`, `statusFilter`, `normalizeSearchHost`, dll).

use artex::traffic::query_helpers::{
    exchange_id, fts_filter, fts_quote, normalize_search_host, proxy_caused_err, sort_column,
    status_filter, valid_header_name,
};

#[test]
fn fts_and_status_helpers_oracle() {
    assert_eq!(fts_quote("plain"), "\"plain\"");
    assert_eq!(fts_quote("a\"b AND c"), "\"a\"\"b AND c\"");
    assert_eq!(fts_quote(""), "\"\"");
    assert_eq!(
        fts_filter(true, "ssw0r"),
        Some((
            "rowid IN (SELECT rowid FROM ex_fts WHERE ex_fts MATCH ?)".to_string(),
            "\"ssw0r\"".to_string(),
        ))
    );
    assert_eq!(fts_filter(true, "ab"), None);
    assert_eq!(fts_filter(false, "abcdef"), None);

    assert_eq!(status_filter("404"), Some(("status=?", vec![404])));
    assert_eq!(
        status_filter(" 4XX "),
        Some(("status>=? AND status<?", vec![400, 500]))
    );
    assert_eq!(status_filter("nope"), None);
    assert_eq!(status_filter(""), None);
    assert_eq!(sort_column("ts"), "ts");
    assert_eq!(sort_column("status"), "status");
    assert_eq!(sort_column("resp_len"), "resp_len");
    assert_eq!(sort_column("bogus"), "ts");
}

#[test]
fn normalize_search_host_oracle() {
    let cases: &[(&str, &str, &str, bool)] = &[
        ("", "", "", true),
        ("EXAMPLE.COM", "example.com", "", false),
        (
            "https://User:pw@API.Example.com:9443/x",
            "api.example.com",
            "9443",
            false,
        ),
        ("[2001:db8::1]:8080", "2001:db8::1", "8080", false),
        ("[2001:db8::1]", "2001:db8::1", "", false),
        ("api.example.com:8443", "api.example.com", "8443", false),
        ("h.test:0", "", "", true),
        ("h.test:70000", "", "", true),
        ("h.test:abc", "", "", true),
        ("ftp://x.test", "x.test", "", false),
    ];
    for (raw, want_host, want_port, want_err) in cases {
        let got = normalize_search_host(raw);
        if *want_err {
            assert!(got.is_err(), "{raw:?}: expected error, got {got:?}");
        } else {
            let (h, p) = got.unwrap_or_default();
            assert_eq!(
                (h.as_str(), p.as_str()),
                (*want_host, *want_port),
                "{raw:?}"
            );
        }
    }
}

#[test]
fn replay_and_format_helpers_oracle() {
    assert!(proxy_caused_err(
        "protocol error: received DATA on a HEAD request"
    ));
    assert!(proxy_caused_err("http2: frame too large"));
    assert!(!proxy_caused_err("connection reset by peer"));
    assert!(valid_header_name("X-A"));
    assert!(!valid_header_name(""));
    assert!(!valid_header_name("X A"));
    assert!(valid_header_name("X_1"));
    assert_eq!(exchange_id(1_700_000_000, 12_345), "1700000000-2345");
}
