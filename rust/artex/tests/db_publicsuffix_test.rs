//! Differential tests for the vendored `publicsuffix` lookup + `RootDomain`
//! (`db/nkey.go`). Expected values were produced by running the real Go
//! `db.RootDomain` against the same PSL dataset (oracle captured live).
//!
//! Pure — no Postgres needed.

use artex::db::publicsuffix::{effective_tld_plus_one, public_suffix, root_domain};

/// (host, expected_root, expected_is_apex) from the Go oracle.
const ORACLE: &[(&str, &str, bool)] = &[
    ("example.com", "example.com", true),
    ("www.example.com", "example.com", false),
    ("a.b.example.com", "example.com", false),
    ("EXAMPLE.COM.", "example.com", true),
    ("example.co.uk", "example.co.uk", true),
    ("www.example.co.uk", "example.co.uk", false),
    ("foo.appspot.com", "foo.appspot.com", true), // PRIVATE suffix applies
    ("a.b.dyndns.org", "b.dyndns.org", false),    // PRIVATE suffix
    ("localhost", "localhost", true),
    ("127.0.0.1", "127.0.0.1", true),
    ("10.0.0.1", "10.0.0.1", true),
    ("com", "com", true),
    ("co.uk", "co.uk", true),
    ("uk", "uk", true),
    ("foo.bar.golang.org", "golang.org", false),
    ("github.io", "github.io", true),
    ("user.github.io", "user.github.io", true), // PRIVATE suffix
    ("a.b.amazonaws.com", "amazonaws.com", false),
    ("bucket.s3.amazonaws.com", "bucket.s3.amazonaws.com", true), // PRIVATE
    ("xn--fsqu00a.xn--0zwm56d", "xn--fsqu00a.xn--0zwm56d", true),
    ("foo.localhost", "foo.localhost", true),
    ("internal.example.local", "example.local", false),
    ("a.b.c.example.org", "example.org", false),
    ("sub.example.com.au", "example.com.au", false),
    ("foo.edu.cn", "foo.edu.cn", true),
    ("a.b.foo.edu.cn", "foo.edu.cn", false),
    ("192.168.1.1", "192.168.1.1", true),
    ("::1", "::1", true),
    ("  WWW.Example.COM.  ", "example.com", false),
    ("example.invalidtldxyz", "example.invalidtldxyz", true),
];

#[test]
fn root_domain_matches_go_oracle() {
    for &(host, want_root, want_apex) in ORACLE {
        let (root, apex) = root_domain(host);
        assert_eq!(root, want_root, "root_domain({host:?}) root");
        assert_eq!(apex, want_apex, "root_domain({host:?}) is_apex");
    }
}

#[test]
fn public_suffix_and_etld1_core_cases() {
    // ICANN suffixes.
    assert_eq!(public_suffix("foo.example.com"), ("com".to_string(), true));
    assert_eq!(
        public_suffix("foo.example.co.uk"),
        ("co.uk".to_string(), true)
    );
    // PRIVATE suffixes (icann=false).
    let (ps, icann) = public_suffix("foo.appspot.com");
    assert_eq!(ps, "appspot.com");
    assert!(!icann, "appspot.com is a PRIVATE suffix");
    // Unmanaged TLD -> "*" rule.
    let (ps, _) = public_suffix("a.b.unknownxyz");
    assert_eq!(ps, "unknownxyz");
    // Bare IP.
    assert_eq!(public_suffix("1.2.3.4"), ("1.2.3.4".to_string(), false));

    assert_eq!(
        effective_tld_plus_one("foo.bar.golang.org").unwrap(),
        "golang.org"
    );
    assert_eq!(
        effective_tld_plus_one("www.example.co.uk").unwrap(),
        "example.co.uk"
    );
    // Errors mirror Go's publicsuffix.EffectiveTLDPlusOne rejection.
    assert!(effective_tld_plus_one(".example.com").is_err());
    assert!(effective_tld_plus_one("example.com.").is_err());
    assert!(effective_tld_plus_one("a..b.com").is_err());
    assert!(
        effective_tld_plus_one("com").is_err(),
        "no eTLD+1 for a bare TLD"
    );
}
