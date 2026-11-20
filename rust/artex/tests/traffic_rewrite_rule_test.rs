//! Differential tests for `traffic/rewrite` rule validation — ports of
//! `TestValidateAcceptsEveryAction`, `TestValidateForbidden`,
//! `TestValidateHostGlob`, `TestValidateErrorOmitsValue`, `TestErrorLog`.

use artex::db::error::Error as DbError;
use artex::traffic::rewrite_rule::{
    validate, Action, ErrorLog, HeaderMatcher, Matchers, Rule, ERR_CLASS_EXPANSION,
    ERR_CLASS_INVALID_URL, ERR_CLASS_PANIC, MAX_PATTERN_SIZE, MAX_VALUE_SIZE, PHASE_REQUEST,
    PHASE_RESPONSE,
};

fn set_header(name: &str, value: &str) -> Action {
    Action {
        typ: "set_header".to_string(),
        name: name.to_string(),
        value: value.to_string(),
        ..Action::default()
    }
}

fn rule(id: i64, phase: &str, m: Matchers, actions: Vec<Action>) -> Rule {
    Rule {
        id,
        name: format!("r{id}"),
        enabled: true,
        phase: phase.to_string(),
        matchers: m,
        actions,
        ..Rule::default()
    }
}

#[test]
fn validate_accepts_every_action() {
    let req = rule(
        1,
        PHASE_REQUEST,
        Matchers {
            methods: vec!["get".into()],
            host: "*.example.com".into(),
            url_regex: "^/api/".into(),
            headers: vec![
                HeaderMatcher {
                    name: "x-a".into(),
                    ..Default::default()
                },
                HeaderMatcher {
                    name: "X-B".into(),
                    value_regex: "^v".into(),
                },
            ],
            body_regex: "a".into(),
            ..Matchers::default()
        },
        vec![
            Action {
                typ: "set_method".into(),
                value: "POST".into(),
                ..Default::default()
            },
            Action {
                typ: "set_path_query".into(),
                value: "/x?y=1".into(),
                ..Default::default()
            },
            Action {
                typ: "replace_url".into(),
                regex: "a".into(),
                value: "b".into(),
                ..Default::default()
            },
            Action {
                typ: "delete_header".into(),
                name: "X-Old".into(),
                ..Default::default()
            },
            set_header("X-New", "v"),
            Action {
                typ: "replace_header".into(),
                name: "Cookie".into(),
                regex: "a".into(),
                value: "b".into(),
                sensitive: true,
            },
            Action {
                typ: "set_body".into(),
                value: "body".into(),
                ..Default::default()
            },
            Action {
                typ: "replace_body".into(),
                regex: "a".into(),
                value: "$0$0".into(),
                ..Default::default()
            },
        ],
    );
    let resp = rule(
        2,
        PHASE_RESPONSE,
        Matchers {
            status: vec!["404".into(), "5xx".into()],
            resp_headers: vec![HeaderMatcher {
                name: "Server".into(),
                ..Default::default()
            }],
            resp_body_regex: "a".into(),
            body_regex: "b".into(),
            ..Matchers::default()
        },
        vec![
            Action {
                typ: "delete_header".into(),
                name: "Server".into(),
                ..Default::default()
            },
            set_header("X-New", "v"),
            Action {
                typ: "replace_header".into(),
                name: "Set-Cookie".into(),
                regex: "a".into(),
                value: "b".into(),
                ..Default::default()
            },
            Action {
                typ: "set_body".into(),
                value: String::new(),
                ..Default::default()
            },
            Action {
                typ: "replace_body".into(),
                regex: "a".into(),
                value: "b".into(),
                ..Default::default()
            },
            Action {
                typ: "set_status".into(),
                value: "200".into(),
                ..Default::default()
            },
        ],
    );
    for r in [&req, &resp] {
        assert!(validate(r).is_ok(), "rule {}: {:?}", r.id, validate(r));
    }
}

#[test]
fn validate_forbidden() {
    let long = "a".repeat(MAX_PATTERN_SIZE + 1);
    let big = "a".repeat(MAX_VALUE_SIZE + 1);
    let ok = set_header("X-A", "v");
    let mut cases: Vec<(String, Rule)> = vec![
        (
            "empty name".to_string(),
            Rule {
                phase: PHASE_REQUEST.into(),
                actions: vec![ok.clone()],
                ..Rule::default()
            },
        ),
        (
            "bad phase".to_string(),
            Rule {
                name: "r".into(),
                phase: "both".into(),
                actions: vec![ok.clone()],
                ..Rule::default()
            },
        ),
        (
            "no action".to_string(),
            rule(1, PHASE_REQUEST, Matchers::default(), vec![]),
        ),
        (
            "unknown action".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_host".into(),
                    value: "evil.example".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "set_method CONNECT".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_method".into(),
                    value: "CONNECT".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "set_method connect".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_method".into(),
                    value: "connect".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "set_method not token".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_method".into(),
                    value: "GET /".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "set_method response".to_string(),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers::default(),
                vec![Action {
                    typ: "set_method".into(),
                    value: "POST".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "path absolute URL".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_path_query".into(),
                    value: "http://evil.example/x".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "path relative".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_path_query".into(),
                    value: "evil.example/x".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "path control char".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_path_query".into(),
                    value: "/a\r\nHost: evil".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "path response".to_string(),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers::default(),
                vec![Action {
                    typ: "set_path_query".into(),
                    value: "/x".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "replace_url response".to_string(),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers::default(),
                vec![Action {
                    typ: "replace_url".into(),
                    regex: "a".into(),
                    value: "b".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "replace no regex".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "replace_body".into(),
                    value: "b".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "replace bad regex".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "replace_body".into(),
                    regex: "(".into(),
                    value: "b".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "action regex 1 KiB".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "replace_body".into(),
                    regex: long.clone(),
                    value: "b".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "value 1 MiB".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_body".into(),
                    value: big.clone(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "replacement 1 MiB".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "replace_body".into(),
                    regex: "a".into(),
                    value: big.clone(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "header name".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![set_header("X A", "v")],
            ),
        ),
        (
            "header name empty".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "delete_header".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "header value CR".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![set_header("X-A", "a\rb")],
            ),
        ),
        (
            "header value LF".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![set_header("X-A", "a\nb")],
            ),
        ),
        (
            "header value NUL".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![set_header("X-A", "a\0b")],
            ),
        ),
        (
            "replacement LF".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "replace_header".into(),
                    name: "X-A".into(),
                    regex: "a".into(),
                    value: "b\nc".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "set_status request".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "set_status".into(),
                    value: "200".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "set_status text".to_string(),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers::default(),
                vec![Action {
                    typ: "set_status".into(),
                    value: "ok".into(),
                    ..Default::default()
                }],
            ),
        ),
        (
            "matcher regex 1 KiB".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    url_regex: long,
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "matcher bad regex".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    body_regex: "(".into(),
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "matcher header name".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    headers: vec![HeaderMatcher {
                        name: "X A".into(),
                        ..Default::default()
                    }],
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "matcher header regex".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    headers: vec![HeaderMatcher {
                        name: "X-A".into(),
                        value_regex: "(".into(),
                    }],
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "matcher method".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    methods: vec!["GET POST".into()],
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "status in request".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    status: vec!["200".into()],
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "resp header request".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    resp_headers: vec![HeaderMatcher {
                        name: "Server".into(),
                        ..Default::default()
                    }],
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "resp body request".to_string(),
            rule(
                1,
                PHASE_REQUEST,
                Matchers {
                    resp_body_regex: "a".into(),
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "status syntax".to_string(),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers {
                    status: vec!["6xx".into()],
                    ..Matchers::default()
                },
                vec![ok.clone()],
            ),
        ),
        (
            "status syntax word".to_string(),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers {
                    status: vec!["ok".into()],
                    ..Matchers::default()
                },
                vec![ok],
            ),
        ),
    ];
    for name in [
        "Host",
        "content-length",
        "Transfer-Encoding",
        "Connection",
        "Proxy-Authorization",
        "proxy-anything",
        "Keep-Alive",
        "TE",
        "Trailer",
        "Expect",
        "Upgrade",
    ] {
        cases.push((
            format!("set_header {name}"),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![set_header(name, "v")],
            ),
        ));
        cases.push((
            format!("delete_header {name}"),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers::default(),
                vec![Action {
                    typ: "delete_header".into(),
                    name: name.into(),
                    ..Default::default()
                }],
            ),
        ));
        cases.push((
            format!("replace_header {name}"),
            rule(
                1,
                PHASE_REQUEST,
                Matchers::default(),
                vec![Action {
                    typ: "replace_header".into(),
                    name: name.into(),
                    regex: "a".into(),
                    value: "b".into(),
                    ..Default::default()
                }],
            ),
        ));
    }
    for status in ["199", "204", "304", "600"] {
        cases.push((
            format!("set_status {status}"),
            rule(
                1,
                PHASE_RESPONSE,
                Matchers::default(),
                vec![Action {
                    typ: "set_status".into(),
                    value: status.into(),
                    ..Default::default()
                }],
            ),
        ));
    }
    for (name, r) in &cases {
        assert!(validate(r).is_err(), "{name}: accepted");
    }
}

#[test]
fn validate_host_glob() {
    let ok = set_header("X-A", "v");
    let cases: &[(&str, bool)] = &[
        ("example.com", true),
        ("*.a.b", true),
        ("*.example.com", true),
        ("::1", true),
        ("a.*.b", false),
        ("*", false),
        ("*.", false),
        ("*.*.example.com", false),
        ("*example.com", false),
        ("example.com:8080", false),
        ("example.com/path", false),
    ];
    for (host, want) in cases {
        let r = rule(
            1,
            PHASE_REQUEST,
            Matchers {
                host: host.to_string(),
                ..Matchers::default()
            },
            vec![ok.clone()],
        );
        assert_eq!(validate(&r).is_ok(), *want, "host {host:?}");
    }
}

#[test]
fn validate_error_omits_value() {
    const SECRET: &str = "s3cr3t-value";
    let cases = [
        Action {
            typ: "set_method".into(),
            value: format!("{SECRET} x"),
            sensitive: true,
            ..Default::default()
        },
        Action {
            typ: "set_path_query".into(),
            value: SECRET.into(),
            sensitive: true,
            ..Default::default()
        },
        Action {
            typ: "set_header".into(),
            name: "X-A".into(),
            value: format!("{SECRET}\n"),
            sensitive: true,
            ..Default::default()
        },
        Action {
            typ: "set_status".into(),
            value: SECRET.into(),
            sensitive: true,
            ..Default::default()
        },
        Action {
            typ: "nope".into(),
            value: SECRET.into(),
            sensitive: true,
            ..Default::default()
        },
    ];
    for a in cases {
        let phase = if a.typ == "set_status" {
            PHASE_RESPONSE
        } else {
            PHASE_REQUEST
        };
        let r = rule(1, phase, Matchers::default(), vec![a.clone()]);
        let err = validate(&r).expect_err(&a.typ);
        assert!(
            !err.to_string().contains(SECRET),
            "{}: error leaked value: {err}",
            a.typ
        );
    }
    let _: Option<DbError> = None;
}

#[test]
fn error_log_replay() {
    let l = ErrorLog::default();
    assert!(l.get(7).is_none());
    l.record(7, ERR_CLASS_EXPANSION);
    l.record(7, ERR_CLASS_INVALID_URL);
    let e = l.get(7).expect("entry");
    assert_eq!(e.class, ERR_CLASS_INVALID_URL);
    assert_eq!(e.count, 2);
    l.forget(7);
    assert!(l.get(7).is_none());
    let _ = ERR_CLASS_PANIC;
}
