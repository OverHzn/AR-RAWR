//! Differential tests for `intercept/argv.go` pure helpers — ports of
//! `TestArgvArityTable` and `TestParseArgvPattern`.

use artex::intercept::argv::{argv_prefix_len, argv_prefix_of, parse_argv_pattern};

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn argv_arity_table() {
    let cases: &[(&[&str], &[&str])] = &[
        (&["git", "push", "origin", "main"], &["git", "push"]),
        (
            &["git", "config", "user.name", "x"],
            &["git", "config", "user.name"],
        ),
        (&["npm", "install", "left-pad"], &["npm", "install"]),
        (
            &["npm", "run", "build", "--watch"],
            &["npm", "run", "build"],
        ),
        (
            &["docker", "compose", "up", "-d"],
            &["docker", "compose", "up"],
        ),
        (&["docker", "ps"], &["docker", "ps"]),
        (
            &["kubectl", "rollout", "status", "x"],
            &["kubectl", "rollout", "status"],
        ),
        (&["gh", "pr", "list", "--json"], &["gh", "pr", "list"]),
        (&["nmap", "-p", "80", "x"], &["nmap"]),
        (&["unknowntool", "a", "b"], &["unknowntool"]),
        (&["cat", "/etc/hosts"], &["cat"]),
        (&["git"], &["git"]),
        (&[], &[]),
    ];
    for (argv, want) in cases {
        assert_eq!(argv_prefix_of(&v(argv)), v(want), "prefix({argv:?})");
    }
    assert_eq!(argv_prefix_len(&[]), 0);
}

#[test]
fn parse_argv_pattern_cases() {
    assert_eq!(
        parse_argv_pattern(r#"["git","push"]"#).unwrap(),
        v(&["git", "push"])
    );
    assert_eq!(
        parse_argv_pattern(r#"["a","b","c","d","e","f","g","h"]"#)
            .unwrap()
            .len(),
        8
    );

    let bad = [
        "git push",             // not json
        "[1,2]",                // not strings
        "[]",                   // empty
        r#"["git",""]"#,        // empty token
        r#"["git","--force"]"#, // flag
        r#"["-rf"]"#,
        r#"["nmap;id"]"#,
        r#"["a&b"]"#,
        r#"["a|b"]"#,
        r#"["a>b"]"#,
        r#"["a<b"]"#,
        "[\"a`b\"]",
        r#"["$HOME"]"#,
        r#"["a(b)"]"#,
        r#"["git push"]"#,
        "[\"a\\nb\"]",
        "[\"a\\tb\"]",
        r#"["a","b","c","d","e","f","g","h","i"]"#, // nine
        "",
    ];
    for p in bad {
        assert!(parse_argv_pattern(p).is_err(), "accepted {p:?}");
    }
}
