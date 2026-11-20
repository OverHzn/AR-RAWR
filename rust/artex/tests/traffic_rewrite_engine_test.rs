//! Ports of `traffic/rewrite/engine_test.go` (all 22 tests) against the Rust
//! engine — exact outcomes, hashes, ordering, atomicity and trace behavior.

use std::sync::Arc;

use artex::traffic::rewrite_engine::{
    ApplyResult, Header, Input, Options, Request, Response, Set, TraceLine, MAX_BODY_SIZE,
    MAX_EXPANSION, REASON_BINARY, REASON_BODY_TOO_LARGE, REASON_CONTENT_ENCODING,
    REASON_NO_BODY_ALLOWED,
};
use artex::traffic::rewrite_rule::{
    Action, ErrorLog, HeaderMatcher, Matchers, Rule, ERR_CLASS_EXPANSION, ERR_CLASS_INVALID_URL,
    ERR_CLASS_PANIC, MAX_VALUE_SIZE, PHASE_REQUEST, PHASE_RESPONSE,
};

const TEST_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn set_header(name: &str, value: &str) -> Action {
    Action {
        typ: "set_header".to_string(),
        name: name.to_string(),
        value: value.to_string(),
        ..Action::default()
    }
}

fn rule(id: i64, phase: &str, priority: i32, m: Matchers, actions: Vec<Action>) -> Rule {
    Rule {
        id,
        name: format!("r{id}"),
        enabled: true,
        phase: phase.to_string(),
        priority,
        matchers: m,
        actions,
        ..Rule::default()
    }
}

fn req(method: &str, raw: &str, header: Header, body: &str) -> Request {
    Request::new(method, raw, header, body.as_bytes())
}

fn resp(status: i32, header: Header, body: &str) -> Response {
    Response::new(status, header, body.as_bytes())
}

fn cset(rules: &[Rule], opts: Options) -> Set {
    let s = Set::compile(rules, opts);
    assert!(
        s.load_errors().is_empty(),
        "load errors: {:?}",
        s.load_errors()
    );
    s
}

fn outcomes(lines: &[TraceLine]) -> Vec<String> {
    lines.iter().map(|l| l.outcome_key()).collect()
}

fn want_lines(lines: &[TraceLine], want: &[&str]) {
    let got = outcomes(lines);
    let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    assert_eq!(got, want, "trace lines mismatch");
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    let sum = sha2::Sha256::digest(b);
    let mut s = String::with_capacity(64);
    for x in sum {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn after(l: &TraceLine) -> &[u8] {
    l.after.as_deref().unwrap_or(&[])
}

#[test]
fn matchers() {
    let mark = set_header("X-Hit", "1");
    let request = req(
        "POST",
        "https://api.example.com:8443/v1/users?id=7",
        Header::from_values(&[("X-Env", &["prod", "eu"]), ("Accept", &["*/*"])]),
        r#"{"name":"bob"}"#,
    );
    let response = resp(
        404,
        Header::from_values(&[("Server", &["nginx/1.25"])]),
        "not found",
    );
    let cases: &[(&str, &str, Matchers, bool)] = &[
        ("no matcher", PHASE_REQUEST, Matchers::default(), true),
        (
            "method, any case",
            PHASE_REQUEST,
            Matchers {
                methods: vec!["get".into(), "post".into()],
                ..Default::default()
            },
            true,
        ),
        (
            "method mismatch",
            PHASE_REQUEST,
            Matchers {
                methods: vec!["GET".into()],
                ..Default::default()
            },
            false,
        ),
        (
            "host exact, any case, port ignored",
            PHASE_REQUEST,
            Matchers {
                host: "API.example.com".into(),
                ..Default::default()
            },
            true,
        ),
        (
            "host mismatch",
            PHASE_REQUEST,
            Matchers {
                host: "example.com".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "host suffix glob",
            PHASE_REQUEST,
            Matchers {
                host: "*.example.com".into(),
                ..Default::default()
            },
            true,
        ),
        (
            "host suffix glob needs a dot boundary",
            PHASE_REQUEST,
            Matchers {
                host: "*.ample.com".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "url regex on path and query",
            PHASE_REQUEST,
            Matchers {
                url_regex: r"^/v1/users\?id=\d+$".into(),
                ..Default::default()
            },
            true,
        ),
        (
            "url regex does not see the host",
            PHASE_REQUEST,
            Matchers {
                url_regex: "example".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "header presence, any case",
            PHASE_REQUEST,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "x-env".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            true,
        ),
        (
            "header absent",
            PHASE_REQUEST,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "X-None".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            false,
        ),
        (
            "header regex on any value",
            PHASE_REQUEST,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "X-Env".into(),
                    value_regex: "^eu$".into(),
                }],
                ..Default::default()
            },
            true,
        ),
        (
            "header regex mismatch",
            PHASE_REQUEST,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "X-Env".into(),
                    value_regex: "^us$".into(),
                }],
                ..Default::default()
            },
            false,
        ),
        (
            "every header matcher must hold",
            PHASE_REQUEST,
            Matchers {
                headers: vec![
                    HeaderMatcher {
                        name: "X-Env".into(),
                        ..Default::default()
                    },
                    HeaderMatcher {
                        name: "X-None".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            false,
        ),
        (
            "body regex",
            PHASE_REQUEST,
            Matchers {
                body_regex: r#""name":"bob""#.into(),
                ..Default::default()
            },
            true,
        ),
        (
            "body regex mismatch",
            PHASE_REQUEST,
            Matchers {
                body_regex: "alice".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "matchers are ANDed",
            PHASE_REQUEST,
            Matchers {
                methods: vec!["POST".into()],
                host: "other.example".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "status code",
            PHASE_RESPONSE,
            Matchers {
                status: vec!["200".into(), "404".into()],
                ..Default::default()
            },
            true,
        ),
        (
            "status class",
            PHASE_RESPONSE,
            Matchers {
                status: vec!["4xx".into()],
                ..Default::default()
            },
            true,
        ),
        (
            "status mismatch",
            PHASE_RESPONSE,
            Matchers {
                status: vec!["5xx".into(), "200".into()],
                ..Default::default()
            },
            false,
        ),
        (
            "response header",
            PHASE_RESPONSE,
            Matchers {
                resp_headers: vec![HeaderMatcher {
                    name: "server".into(),
                    value_regex: "^nginx".into(),
                }],
                ..Default::default()
            },
            true,
        ),
        (
            "response header mismatch",
            PHASE_RESPONSE,
            Matchers {
                resp_headers: vec![HeaderMatcher {
                    name: "Server".into(),
                    value_regex: "apache".into(),
                }],
                ..Default::default()
            },
            false,
        ),
        (
            "response body regex",
            PHASE_RESPONSE,
            Matchers {
                resp_body_regex: "found$".into(),
                ..Default::default()
            },
            true,
        ),
        (
            "response body regex mismatch",
            PHASE_RESPONSE,
            Matchers {
                resp_body_regex: "^ok".into(),
                ..Default::default()
            },
            false,
        ),
        (
            "request matchers in the response phase",
            PHASE_RESPONSE,
            Matchers {
                methods: vec!["POST".into()],
                host: "*.example.com".into(),
                body_regex: "bob".into(),
                ..Default::default()
            },
            true,
        ),
        (
            "request header in the response phase",
            PHASE_RESPONSE,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "X-None".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            false,
        ),
    ];
    for (name, phase, m, want) in cases {
        let s = cset(
            &[rule(1, phase, 0, m.clone(), vec![mark.clone()])],
            Options::default(),
        );
        let res = s.apply(
            phase,
            Input {
                request: request.clone(),
                response: response.clone(),
                stream: false,
                is_binary: None,
            },
        );
        assert_eq!(
            res.changed, *want,
            "{name}: applied={}, want {}",
            res.changed, want
        );
    }
    let apex = req("GET", "http://example.com/", Header::default(), "");
    let s = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers {
                host: "*.example.com".into(),
                ..Default::default()
            },
            vec![mark],
        )],
        Options::default(),
    );
    assert!(
        !s.apply(PHASE_REQUEST, Input::new(apex)).changed,
        "*.example.com must not match example.com"
    );
}

#[test]
fn request_actions() {
    let r = req(
        "GET",
        "http://h.example/old/path?q=1",
        Header::from_values(&[
            ("X-Drop", &["a", "b"]),
            ("Cookie", &["sid=abc", "theme=dark"]),
            ("X-Keep", &["k"]),
        ]),
        "hello world",
    );
    let s = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_method".into(),
                    value: "POST".into(),
                    ..Default::default()
                },
                Action {
                    typ: "set_path_query".into(),
                    value: "/new/path?q=2".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_url".into(),
                    regex: r"q=(\d)".into(),
                    value: "q=${1}0".into(),
                    ..Default::default()
                },
                Action {
                    typ: "delete_header".into(),
                    name: "x-drop".into(),
                    ..Default::default()
                },
                set_header("x-new", "v"),
                Action {
                    typ: "replace_header".into(),
                    name: "Cookie".into(),
                    regex: "^sid=.*$".into(),
                    value: "sid=xyz".into(),
                    ..Default::default()
                },
                Action {
                    typ: "set_body".into(),
                    value: "hello there".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_body".into(),
                    regex: r"(\w+) there".into(),
                    value: "$1, $1".into(),
                    ..Default::default()
                },
            ],
        )],
        Options::default(),
    );
    let res = s.apply(PHASE_REQUEST, Input::new(r));
    let got = &res.request;
    assert!(res.changed);
    assert_eq!(got.method, "POST");
    assert_eq!(got.url.to_string(), "http://h.example/new/path?q=20");
    assert_eq!(got.body, b"hello, hello");
    assert_eq!(
        got.header,
        Header::from_values(&[
            ("Cookie", &["sid=xyz", "theme=dark"]),
            ("X-Keep", &["k"]),
            ("X-New", &["v"])
        ])
    );
    want_lines(
        &res.lines,
        &[
            "1:set_method:method:applied:",
            "1:set_path_query:url:applied:",
            "1:replace_url:url:applied:",
            "1:delete_header:header:X-Drop:applied:",
            "1:set_header:header:X-New:applied:",
            "1:replace_header:header:Cookie:applied:",
            "1:set_body:body:applied:",
            "1:replace_body:body:applied:",
        ],
    );
    let l = &res.lines[2];
    assert_eq!(after(l), b"/new/path?q=20");
    assert_eq!(l.before_hash, sha256_hex(b"/new/path?q=2"));
    assert_eq!(l.after_hash, sha256_hex(after(l)));
    let l = &res.lines[3];
    assert!(l.after.is_none());
    assert_eq!(l.after_hash, "");
    assert_eq!(l.before_hash, sha256_hex(b"a\nb"));
}

#[test]
fn response_actions() {
    let s = cset(
        &[rule(
            1,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_status".into(),
                    value: "200".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_body".into(),
                    regex: "denied".into(),
                    value: "granted".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_header".into(),
                    name: "Set-Cookie".into(),
                    regex: "; Secure".into(),
                    value: "".into(),
                    ..Default::default()
                },
                set_header("Server", "artex"),
                Action {
                    typ: "delete_header".into(),
                    name: "X-Absent".into(),
                    ..Default::default()
                },
            ],
        )],
        Options::default(),
    );
    let request = req("GET", "http://h.example/", Header::default(), "");
    let response = resp(
        403,
        Header::from_values(&[
            ("Server", &["nginx"]),
            ("Set-Cookie", &["a=1; Secure", "b=2; Secure"]),
        ]),
        "denied",
    );
    let original_url = request.url.clone();
    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request,
            response,
            stream: false,
            is_binary: None,
        },
    );
    let got = &res.response;
    assert_eq!(got.status, 200);
    assert_eq!(got.body, b"granted");
    assert_eq!(
        got.header,
        Header::from_values(&[("Server", &["artex"]), ("Set-Cookie", &["a=1", "b=2"])])
    );
    want_lines(
        &res.lines,
        &[
            "1:replace_header:header:Set-Cookie:applied:",
            "1:set_header:header:Server:applied:",
            "1:replace_body:body:applied:",
            "1:set_status:status:applied:",
        ],
    );
    assert_eq!(
        res.request.url, original_url,
        "the response phase must hand the request back untouched"
    );
}

#[test]
fn action_order_within_rule_and_in_class() {
    let s = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_body".into(),
                    value: "b".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_header".into(),
                    name: "X-A".into(),
                    regex: "$".into(),
                    value: "2".into(),
                    ..Default::default()
                },
                set_header("X-A", "1"),
                Action {
                    typ: "delete_header".into(),
                    name: "X-A".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_url".into(),
                    regex: "^/a$".into(),
                    value: "/b".into(),
                    ..Default::default()
                },
                Action {
                    typ: "set_method".into(),
                    value: "PUT".into(),
                    ..Default::default()
                },
            ],
        )],
        Options::default(),
    );
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req(
            "GET",
            "http://h.example/a",
            Header::from_values(&[("X-A", &["0"])]),
            "a",
        )),
    );
    want_lines(
        &res.lines,
        &[
            "1:replace_url:url:applied:",
            "1:set_method:method:applied:",
            "1:delete_header:header:X-A:applied:",
            "1:set_header:header:X-A:applied:",
            "1:set_body:body:applied:",
        ],
    );
    assert_eq!(res.request.header.values("X-A"), vec!["1"]);

    let s = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                set_header("X-A", "1"),
                Action {
                    typ: "replace_header".into(),
                    name: "X-A".into(),
                    regex: "$".into(),
                    value: "2".into(),
                    ..Default::default()
                },
            ],
        )],
        Options::default(),
    );
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req("GET", "http://h.example/", Header::default(), "")),
    );
    assert_eq!(res.request.header.get("X-A"), Some("12"));
}

#[test]
fn priority_order_and_pipeline() {
    let rules = vec![
        rule(
            5,
            PHASE_REQUEST,
            10,
            Matchers::default(),
            vec![set_header("X-Who", "p10")],
        ),
        rule(
            9,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![set_header("X-Who", "p0-id9")],
        ),
        rule(
            2,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![set_header("X-Who", "p0-id2")],
        ),
        rule(
            7,
            PHASE_REQUEST,
            20,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "X-Who".into(),
                    value_regex: "^p10$".into(),
                }],
                ..Default::default()
            },
            vec![set_header("X-Seen", "yes")],
        ),
        rule(
            8,
            PHASE_REQUEST,
            -5,
            Matchers {
                headers: vec![HeaderMatcher {
                    name: "X-Who".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            vec![set_header("X-Early", "yes")],
        ),
    ];
    let res = cset(&rules, Options::default()).apply(
        PHASE_REQUEST,
        Input::new(req("GET", "http://h.example/", Header::default(), "")),
    );
    want_lines(
        &res.lines,
        &[
            "2:set_header:header:X-Who:applied:",
            "9:set_header:header:X-Who:applied:",
            "5:set_header:header:X-Who:applied:",
            "7:set_header:header:X-Seen:applied:",
        ],
    );
    let h = &res.request.header;
    assert_eq!(h.get("X-Who"), Some("p10"));
    assert_eq!(h.get("X-Seen"), Some("yes"));
    assert_eq!(h.get("X-Early"), None);
}

#[test]
fn rule_atomic_fail_open() {
    let log = Arc::new(ErrorLog::default());
    let grow = "x".repeat(1024);
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![set_header("X-First", "1")],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_method".into(),
                    value: "POST".into(),
                    ..Default::default()
                },
                set_header("X-Second", "2"),
                Action {
                    typ: "replace_body".into(),
                    regex: "a".into(),
                    value: grow,
                    ..Default::default()
                },
            ],
        ),
        rule(
            3,
            PHASE_REQUEST,
            2,
            Matchers::default(),
            vec![set_header("X-Third", "3")],
        ),
    ];
    let s = cset(
        &rules,
        Options {
            errors: Some(log.clone()),
            ..Options::default()
        },
    );
    let r = req(
        "GET",
        "http://h.example/",
        Header::default(),
        &"a".repeat(2048),
    );
    let res = s.apply(PHASE_REQUEST, Input::new(r.clone()));
    want_lines(
        &res.lines,
        &[
            "1:set_header:header:X-First:applied:",
            "2:replace_body:body:error:expansion_limit",
            "3:set_header:header:X-Third:applied:",
        ],
    );
    let got = &res.request;
    assert_eq!(got.method, "GET");
    assert_eq!(got.header.get("X-Second"), None);
    assert_eq!(got.body, r.body);
    assert_eq!(got.header.get("X-First"), Some("1"));
    assert_eq!(got.header.get("X-Third"), Some("3"));
    let e = log.get(2).expect("rule 2 error");
    assert_eq!(e.class, ERR_CLASS_EXPANSION);
    assert_eq!(e.count, 1);
    assert!(log.get(1).is_none());
    s.apply(PHASE_REQUEST, Input::new(r));
    assert_eq!(log.get(2).unwrap().count, 2);
}

#[test]
fn url_rewrite_stays_on_host() {
    let r = req(
        "GET",
        "https://user:pw@h.example:8443/a?x=1",
        Header::default(),
        "",
    );
    let cases: &[(&str, &str)] = &[
        ("http://evil.example/x", ""),
        ("evil.example", ""),
        ("/a\nb", ""),
        (
            "//evil.example/x",
            "https://user:pw@h.example:8443//evil.example/x",
        ),
        ("/b?y=2", "https://user:pw@h.example:8443/b?y=2"),
    ];
    for (value, want_url) in cases {
        let s = cset(
            &[rule(
                1,
                PHASE_REQUEST,
                0,
                Matchers::default(),
                vec![Action {
                    typ: "replace_url".into(),
                    regex: "^.*$".into(),
                    value: value.to_string(),
                    ..Default::default()
                }],
            )],
            Options::default(),
        );
        let res = s.apply(PHASE_REQUEST, Input::new(r.clone()));
        if want_url.is_empty() {
            want_lines(&res.lines, &["1:replace_url:url:error:invalid_url"]);
            assert!(!res.changed);
            assert_eq!(res.request.url, r.url);
            continue;
        }
        let u = &res.request.url;
        assert_eq!(u.to_string(), *want_url);
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "h.example:8443");
    }
}

#[test]
fn panic_in_rule_fails_open() {
    let log = Arc::new(ErrorLog::default());
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                set_header("X-A", "1"),
                Action {
                    typ: "set_body".into(),
                    value: "new".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![set_header("X-B", "2")],
        ),
    ];
    let s = cset(
        &rules,
        Options {
            errors: Some(log.clone()),
            ..Options::default()
        },
    );
    let r = req("POST", "http://h.example/", Header::default(), "old");
    let res = s.apply(
        PHASE_REQUEST,
        Input {
            request: r,
            response: resp(200, Header::default(), ""),
            stream: false,
            is_binary: Some(Box::new(|_, _| panic!("boom"))),
        },
    );
    want_lines(
        &res.lines,
        &["1:::error:panic", "2:set_header:header:X-B:applied:"],
    );
    assert_eq!(res.request.body, b"old");
    assert_eq!(res.request.header.get("X-A"), None);
    assert_eq!(res.request.header.get("X-B"), Some("2"));
    assert_eq!(log.get(1).unwrap().class, ERR_CLASS_PANIC);
}

#[test]
fn inputs_not_mutated() {
    let new_input = || Input {
        request: req(
            "GET",
            "http://h.example/a?x=1",
            Header::from_values(&[("X-A", &["1", "2"]), ("Content-Length", &["3"])]),
            "abc",
        ),
        response: resp(
            200,
            Header::from_values(&[
                ("X-B", &["1"]),
                ("Etag", &["\"e\""]),
                ("Content-Length", &["3"]),
            ]),
            "xyz",
        ),
        stream: false,
        is_binary: None,
    };
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_method".into(),
                    value: "POST".into(),
                    ..Default::default()
                },
                Action {
                    typ: "set_path_query".into(),
                    value: "/b".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_header".into(),
                    name: "X-A".into(),
                    regex: "1".into(),
                    value: "9".into(),
                    ..Default::default()
                },
                Action {
                    typ: "delete_header".into(),
                    name: "X-A".into(),
                    ..Default::default()
                },
                set_header("X-N", "n"),
                Action {
                    typ: "replace_body".into(),
                    regex: "b".into(),
                    value: "B".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![Action {
                typ: "replace_body".into(),
                regex: "B".into(),
                value: "bb".into(),
                ..Default::default()
            }],
        ),
        rule(
            3,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_status".into(),
                    value: "500".into(),
                    ..Default::default()
                },
                set_header("X-B", "2"),
                Action {
                    typ: "replace_body".into(),
                    regex: "y".into(),
                    value: "Y".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            4,
            PHASE_RESPONSE,
            1,
            Matchers::default(),
            vec![
                set_header("X-C", "c"),
                Action {
                    typ: "replace_body".into(),
                    regex: "x".into(),
                    value: "x".repeat(MAX_VALUE_SIZE),
                    ..Default::default()
                },
            ],
        ),
    ];
    let s = cset(&rules, Options::default());
    let original = new_input();
    let mut in_req = new_input();
    let res = s.apply(
        PHASE_REQUEST,
        Input {
            request: in_req.request.clone(),
            response: in_req.response.clone(),
            stream: false,
            is_binary: None,
        },
    );
    assert!(res.changed);
    assert_eq!(res.request.url.to_string(), "http://h.example/b");
    assert_eq!(res.request.body, b"abbc");
    assert_eq!(res.request.header.get("Content-Length"), None);
    let mut rr = res;
    rr.request.header.set("X-After", "1");
    rr.request.body[0] = b'Z';
    assert_eq!(in_req.request.header.get("X-After"), None);
    assert_eq!(in_req.request.body[0], b'a');

    let res = s.apply(PHASE_RESPONSE, new_input());
    assert!(res.changed);
    assert_eq!(res.response.status, 500);
    assert_eq!(res.response.body, b"xYz");
    assert_eq!(res.response.header.get("X-C"), None);
    let mut rr = res;
    rr.response.header.set("X-After", "1");
    rr.response.body[0] = b'Z';
    assert_eq!(original.response.header.get("X-After"), None);
    assert_eq!(original.response.body[0], b'x');
    let _ = &mut in_req;
}

#[test]
fn body_rewrite_drops_body_headers() {
    let h = Header::from_values(&[
        ("Content-Length", &["3"]),
        ("Etag", &["\"v1\""]),
        ("Content-Md5", &["x"]),
        ("Content-Encoding", &["identity"]),
        ("Content-Type", &["text/plain"]),
    ]);
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![Action {
                typ: "set_body".into(),
                value: "longer body".into(),
                ..Default::default()
            }],
        ),
        rule(
            2,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![Action {
                typ: "replace_body".into(),
                regex: "a".into(),
                value: "bb".into(),
                ..Default::default()
            }],
        ),
    ];
    let s = cset(&rules, Options::default());
    let want = Header::from_values(&[("Content-Type", &["text/plain"])]);
    let got = s
        .apply(
            PHASE_REQUEST,
            Input::new(req("POST", "http://h.example/", h.clone(), "abc")),
        )
        .request
        .header;
    assert_eq!(got, want);
    let mut in_resp = Input {
        request: req("GET", "http://h.example/", Header::default(), ""),
        response: resp(200, h.clone(), "abc"),
        stream: false,
        is_binary: None,
    };
    let got = s
        .apply(
            PHASE_RESPONSE,
            Input {
                request: in_resp.request.clone(),
                response: in_resp.response.clone(),
                stream: false,
                is_binary: None,
            },
        )
        .response
        .header;
    assert_eq!(got, want);
    in_resp.response.body = b"xyz".to_vec();
    let res = s.apply(PHASE_RESPONSE, in_resp);
    assert!(!res.changed && res.lines.is_empty());
}

#[test]
fn no_change_no_line() {
    let s = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_method".into(),
                    value: "GET".into(),
                    ..Default::default()
                },
                Action {
                    typ: "set_path_query".into(),
                    value: "/a?x=1".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_url".into(),
                    regex: "zzz".into(),
                    value: "y".into(),
                    ..Default::default()
                },
                Action {
                    typ: "delete_header".into(),
                    name: "X-Absent".into(),
                    ..Default::default()
                },
                set_header("X-A", "1"),
                Action {
                    typ: "replace_header".into(),
                    name: "X-A".into(),
                    regex: "zzz".into(),
                    value: "y".into(),
                    ..Default::default()
                },
                Action {
                    typ: "set_body".into(),
                    value: "abc".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_body".into(),
                    regex: "zzz".into(),
                    value: "y".into(),
                    ..Default::default()
                },
            ],
        )],
        Options::default(),
    );
    let r = req(
        "GET",
        "http://h.example/a?x=1",
        Header::from_values(&[("X-A", &["1"])]),
        "abc",
    );
    let original_url = r.url.clone();
    let res = s.apply(PHASE_REQUEST, Input::new(r));
    assert!(!res.changed && res.lines.is_empty());
    assert_eq!(res.request.url, original_url);
}

#[test]
fn run_time_limits() {
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                set_header("X-A", "1"),
                Action {
                    typ: "replace_body".into(),
                    regex: "a".into(),
                    value: "b".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers {
                body_regex: "[ab]".into(),
                ..Default::default()
            },
            vec![set_header("X-B", "1")],
        ),
        rule(
            3,
            PHASE_REQUEST,
            2,
            Matchers::default(),
            vec![set_header("X-C", "1")],
        ),
    ];
    let s = cset(&rules, Options::default());
    let at_limit = req(
        "POST",
        "http://h.example/",
        Header::default(),
        &"a".repeat(MAX_BODY_SIZE),
    );
    let res = s.apply(PHASE_REQUEST, Input::new(at_limit));
    assert_eq!(res.lines.len(), 4);
    assert_eq!(res.request.body.len(), MAX_BODY_SIZE);

    let over = req(
        "POST",
        "http://h.example/",
        Header::default(),
        &"a".repeat(MAX_BODY_SIZE + 1),
    );
    let res = s.apply(PHASE_REQUEST, Input::new(over));
    want_lines(
        &res.lines,
        &[
            "1:::skipped:not rewritten: body too large",
            "2:::skipped:not rewritten: body too large",
            "3:set_header:header:X-C:applied:",
        ],
    );
    assert_eq!(res.request.header.get("X-A"), None);
    assert_eq!(res.request.header.get("X-B"), None);

    let grow = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![Action {
                typ: "replace_url".into(),
                regex: "a".into(),
                value: "b".repeat(MAX_VALUE_SIZE),
                ..Default::default()
            }],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![Action {
                typ: "replace_header".into(),
                name: "X-A".into(),
                regex: "a".into(),
                value: "b".repeat(MAX_VALUE_SIZE),
                ..Default::default()
            }],
        ),
        rule(
            3,
            PHASE_REQUEST,
            2,
            Matchers::default(),
            vec![Action {
                typ: "replace_body".into(),
                regex: "a".into(),
                value: "b".repeat(MAX_VALUE_SIZE),
                ..Default::default()
            }],
        ),
        rule(
            4,
            PHASE_REQUEST,
            3,
            Matchers::default(),
            vec![Action {
                typ: "replace_body".into(),
                regex: "^a".into(),
                value: "b".repeat(MAX_EXPANSION - 1),
                ..Default::default()
            }],
        ),
    ];
    let s = cset(&grow, Options::default());
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req(
            "POST",
            "http://h.example/aa",
            Header::from_values(&[("X-A", &["aa"])]),
            "aa",
        )),
    );
    want_lines(
        &res.lines,
        &[
            "1:replace_url:url:error:expansion_limit",
            "2:replace_header:header:X-A:error:expansion_limit",
            "3:replace_body:body:error:expansion_limit",
            "4:replace_body:body:applied:",
        ],
    );
    assert_eq!(res.request.body.len(), MAX_EXPANSION);
}

#[test]
fn header_named_in_connection() {
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                set_header("X-Other", "1"),
                Action {
                    typ: "delete_header".into(),
                    name: "X-Hop".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![set_header("X-Other", "2")],
        ),
        rule(
            3,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![set_header("X-Hop", "1")],
        ),
        rule(
            4,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![set_header("X-Resp-Hop", "1")],
        ),
    ];
    let s = cset(&rules, Options::default());
    let r = req(
        "GET",
        "http://h.example/",
        Header::from_values(&[("Connection", &["keep-alive, x-hop"]), ("X-Hop", &["v"])]),
        "",
    );
    let res = s.apply(PHASE_REQUEST, Input::new(r.clone()));
    want_lines(
        &res.lines,
        &[
            "1:::skipped:header named in Connection",
            "2:set_header:header:X-Other:applied:",
        ],
    );
    assert_eq!(res.request.header.get("X-Hop"), Some("v"));
    assert_eq!(res.request.header.get("X-Other"), Some("2"));

    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request: r,
            response: resp(
                200,
                Header::from_values(&[("Connection", &["X-Resp-Hop"])]),
                "",
            ),
            stream: false,
            is_binary: None,
        },
    );
    want_lines(
        &res.lines,
        &[
            "3:set_header:header:X-Hop:applied:",
            "4:::skipped:header named in Connection",
        ],
    );
}

#[test]
fn response_phase_sees_rewritten_request() {
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_path_query".into(),
                    value: "/internal".into(),
                    ..Default::default()
                },
                set_header("X-Role", "admin"),
            ],
        ),
        rule(
            2,
            PHASE_RESPONSE,
            0,
            Matchers {
                url_regex: "^/internal$".into(),
                headers: vec![HeaderMatcher {
                    name: "X-Role".into(),
                    value_regex: "admin".into(),
                }],
                ..Default::default()
            },
            vec![set_header("X-Matched", "1")],
        ),
    ];
    let s = cset(&rules, Options::default());
    let client = req("GET", "http://h.example/public", Header::default(), "");
    let sent = s.apply(PHASE_REQUEST, Input::new(client.clone())).request;
    let response = resp(200, Header::default(), "ok");
    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request: sent,
            response: response.clone(),
            stream: false,
            is_binary: None,
        },
    );
    assert_eq!(res.response.header.get("X-Matched"), Some("1"));
    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request: client,
            response,
            stream: false,
            is_binary: None,
        },
    );
    assert!(!res.changed);
}

#[test]
#[allow(clippy::type_complexity)]
fn body_rule_not_eligible_skipped_whole() {
    let body_rule = rule(
        1,
        PHASE_RESPONSE,
        0,
        Matchers::default(),
        vec![
            set_header("X-Body-Rule", "1"),
            Action {
                typ: "set_status".into(),
                value: "200".into(),
                ..Default::default()
            },
            Action {
                typ: "replace_body".into(),
                regex: "a".into(),
                value: "b".into(),
                ..Default::default()
            },
        ],
    );
    let matcher_rule = rule(
        2,
        PHASE_RESPONSE,
        1,
        Matchers {
            resp_body_regex: ".".into(),
            ..Default::default()
        },
        vec![set_header("X-Matcher-Rule", "1")],
    );
    let header_rule = rule(
        3,
        PHASE_RESPONSE,
        2,
        Matchers::default(),
        vec![set_header("X-Header-Rule", "1")],
    );
    let s = cset(&[body_rule, matcher_rule, header_rule], Options::default());
    let get = req("GET", "http://h.example/", Header::default(), "");
    let cases: Vec<(
        &str,
        Request,
        Response,
        Option<Box<artex::traffic::rewrite_engine::BinaryFn>>,
        &str,
    )> = vec![
        (
            "NUL byte",
            get.clone(),
            resp(500, Header::default(), "a\0a"),
            None,
            REASON_BINARY,
        ),
        (
            "binary content type",
            get.clone(),
            resp(
                500,
                Header::from_values(&[("Content-Type", &["application/zip"])]),
                "aaa",
            ),
            Some(Box::new(|ct, _| ct == "application/zip")),
            REASON_BINARY,
        ),
        (
            "too large",
            get.clone(),
            resp(500, Header::default(), &"a".repeat(MAX_BODY_SIZE + 1)),
            None,
            REASON_BODY_TOO_LARGE,
        ),
        (
            "content-encoding",
            get.clone(),
            resp(
                500,
                Header::from_values(&[("Content-Encoding", &["gzip"])]),
                "aaa",
            ),
            None,
            REASON_CONTENT_ENCODING,
        ),
        (
            "HEAD",
            req("HEAD", "http://h.example/", Header::default(), ""),
            resp(500, Header::default(), ""),
            None,
            REASON_NO_BODY_ALLOWED,
        ),
        (
            "204",
            get.clone(),
            resp(204, Header::default(), ""),
            None,
            REASON_NO_BODY_ALLOWED,
        ),
        (
            "304",
            get.clone(),
            resp(304, Header::default(), ""),
            None,
            REASON_NO_BODY_ALLOWED,
        ),
        (
            "101",
            get.clone(),
            resp(101, Header::default(), ""),
            None,
            REASON_NO_BODY_ALLOWED,
        ),
    ];
    for (name, request, response, binary, reason) in cases {
        let res = s.apply(
            PHASE_RESPONSE,
            Input {
                request,
                response: response.clone(),
                stream: false,
                is_binary: binary,
            },
        );
        want_lines(
            &res.lines,
            &[
                &format!("1:::skipped:{reason}"),
                &format!("2:::skipped:{reason}"),
                "3:set_header:header:X-Header-Rule:applied:",
            ],
        );
        let h = &res.response.header;
        assert_eq!(h.get("X-Body-Rule"), None);
        assert_eq!(h.get("X-Matcher-Rule"), None);
        assert_eq!(res.response.status, response.status, "{name}");
        assert_eq!(h.get("X-Header-Rule"), Some("1"));
    }
    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request: get.clone(),
            response: resp(
                500,
                Header::from_values(&[("Content-Encoding", &["identity"])]),
                "aaa",
            ),
            stream: false,
            is_binary: None,
        },
    );
    assert_eq!(res.response.body, b"bbb");
    assert_eq!(res.response.status, 200);

    let req_set = cset(
        &[
            rule(
                1,
                PHASE_REQUEST,
                0,
                Matchers::default(),
                vec![
                    set_header("X-A", "1"),
                    Action {
                        typ: "set_body".into(),
                        value: "new".into(),
                        ..Default::default()
                    },
                ],
            ),
            rule(
                2,
                PHASE_RESPONSE,
                0,
                Matchers {
                    body_regex: "a".into(),
                    ..Default::default()
                },
                vec![set_header("X-B", "1")],
            ),
        ],
        Options::default(),
    );
    let gz = req(
        "POST",
        "http://h.example/",
        Header::from_values(&[("Content-Encoding", &["br"])]),
        "aaa",
    );
    want_lines(
        &req_set.apply(PHASE_REQUEST, Input::new(gz.clone())).lines,
        &["1:::skipped:not rewritten: content-encoding"],
    );
    want_lines(
        &req_set
            .apply(
                PHASE_RESPONSE,
                Input {
                    request: gz.clone(),
                    response: resp(200, Header::default(), ""),
                    stream: false,
                    is_binary: None,
                },
            )
            .lines,
        &["2:::skipped:not rewritten: content-encoding"],
    );
    let miss = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers {
                methods: vec!["PUT".into()],
                ..Default::default()
            },
            vec![Action {
                typ: "set_body".into(),
                value: "new".into(),
                ..Default::default()
            }],
        )],
        Options::default(),
    );
    assert!(miss.apply(PHASE_REQUEST, Input::new(gz)).lines.is_empty());
}

#[test]
fn stream_mode() {
    let rules = vec![
        rule(
            1,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![
                set_header("X-A", "1"),
                Action {
                    typ: "set_status".into(),
                    value: "202".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            2,
            PHASE_RESPONSE,
            1,
            Matchers::default(),
            vec![
                set_header("X-B", "1"),
                Action {
                    typ: "replace_body".into(),
                    regex: "a".into(),
                    value: "b".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            3,
            PHASE_RESPONSE,
            2,
            Matchers {
                resp_body_regex: "a".into(),
                ..Default::default()
            },
            vec![set_header("X-C", "1")],
        ),
        rule(
            4,
            PHASE_RESPONSE,
            3,
            Matchers {
                body_regex: "ping".into(),
                ..Default::default()
            },
            vec![set_header("X-D", "1")],
        ),
        rule(
            5,
            PHASE_RESPONSE,
            4,
            Matchers {
                status: vec!["500".into()],
                ..Default::default()
            },
            vec![Action {
                typ: "set_body".into(),
                value: "x".into(),
                ..Default::default()
            }],
        ),
    ];
    let s = cset(&rules, Options::default());
    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request: req("POST", "http://h.example/events", Header::default(), "ping"),
            response: resp(
                200,
                Header::from_values(&[("Content-Type", &["text/event-stream"])]),
                "",
            ),
            stream: true,
            is_binary: None,
        },
    );
    want_lines(
        &res.lines,
        &[
            "1:set_header:header:X-A:applied:",
            "1:set_status:status:applied:",
            "2:::skipped:not rewritten: stream",
            "3:::skipped:not rewritten: stream",
            "4:set_header:header:X-D:applied:",
        ],
    );
    let h = &res.response.header;
    assert_eq!(res.response.status, 202);
    assert_eq!(h.get("X-A"), Some("1"));
    assert_eq!(h.get("X-B"), None);
    assert_eq!(h.get("X-C"), None);
    assert_eq!(h.get("X-D"), Some("1"));
}

#[test]
fn upgrade_request_skips_every_rule() {
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![set_header("X-A", "1")],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers {
                body_regex: "a".into(),
                ..Default::default()
            },
            vec![Action {
                typ: "set_body".into(),
                value: "x".into(),
                ..Default::default()
            }],
        ),
        rule(
            3,
            PHASE_REQUEST,
            2,
            Matchers {
                methods: vec!["POST".into()],
                ..Default::default()
            },
            vec![set_header("X-C", "1")],
        ),
    ];
    let s = cset(&rules, Options::default());
    let r = req(
        "GET",
        "http://h.example/ws",
        Header::from_values(&[
            ("Connection", &["keep-alive, Upgrade"]),
            ("Upgrade", &["websocket"]),
        ]),
        "",
    );
    let res = s.apply(PHASE_REQUEST, Input::new(r));
    want_lines(
        &res.lines,
        &[
            "1:::skipped:not rewritten: stream",
            "2:::skipped:not rewritten: stream",
        ],
    );
    assert!(!res.changed);
}

#[test]
fn sensitive_value_never_traced() {
    const SECRET: &str = "tok-SECRET-0001";
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_header".into(),
                    name: "Authorization".into(),
                    value: SECRET.into(),
                    sensitive: true,
                    ..Default::default()
                },
                Action {
                    typ: "replace_url".into(),
                    regex: "token=[^&]*".into(),
                    value: format!("token={SECRET}"),
                    sensitive: true,
                    ..Default::default()
                },
                Action {
                    typ: "set_body".into(),
                    value: SECRET.into(),
                    sensitive: true,
                    ..Default::default()
                },
                set_header("X-Plain", "visible"),
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![
                Action {
                    typ: "replace_header".into(),
                    name: "Authorization".into(),
                    regex: "^".into(),
                    value: "Bearer ".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_body".into(),
                    regex: "$".into(),
                    value: "!".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_header".into(),
                    name: "X-Plain".into(),
                    regex: "$".into(),
                    value: "!".into(),
                    ..Default::default()
                },
            ],
        ),
    ];
    let s = cset(
        &rules,
        Options {
            trace_key: TEST_KEY.to_vec(),
            ..Options::default()
        },
    );
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req(
            "GET",
            "http://h.example/a?token=old",
            Header::from_values(&[("Authorization", &["old"])]),
            "old body",
        )),
    );
    let plain = sha256_hex(SECRET.as_bytes());
    for l in &res.lines {
        let blob = serde_json::to_string(l).unwrap();
        assert!(
            !blob.contains(SECRET)
                && !after(l)
                    .windows(SECRET.len())
                    .any(|w| w == SECRET.as_bytes())
        );
        assert!(l.after_hash != plain && l.before_hash != plain);
    }
    want_lines(
        &res.lines,
        &[
            "1:replace_url:url:applied:",
            "1:set_header:header:Authorization:applied:",
            "1:set_header:header:X-Plain:applied:",
            "1:set_body:body:applied:",
            "2:replace_header:header:Authorization:applied:",
            "2:replace_header:header:X-Plain:applied:",
            "2:replace_body:body:applied:",
        ],
    );
    assert_eq!(
        res.request.header.get("Authorization"),
        Some(&format!("Bearer {SECRET}")[..])
    );
    let by_field = |field: &str| -> Vec<&TraceLine> {
        res.lines.iter().filter(|l| l.field == field).collect()
    };
    let auth = by_field("header:Authorization");
    assert_eq!(auth[0].before_hash, sha256_hex(b"old"));
    assert_eq!(auth[0].after_hash, s.fingerprint(SECRET.as_bytes()));
    assert!(auth[0].after.is_none());
    assert!(auth[1].after.is_none());
    assert_eq!(auth[1].before_hash, s.fingerprint(SECRET.as_bytes()));
    assert_eq!(
        auth[1].after_hash,
        s.fingerprint(format!("Bearer {SECRET}").as_bytes())
    );
    let body = by_field("body");
    assert!(body[0].after.is_none() && body[1].after.is_none());
    let plain_lines = by_field("header:X-Plain");
    assert_eq!(after(plain_lines[0]), b"visible");
    assert_eq!(after(plain_lines[1]), b"visible!");
    assert_eq!(plain_lines[1].after_hash, sha256_hex(b"visible!"));
    let snap = &res.lines[0].rule_snapshot;
    assert!(
        !snap.contains(SECRET)
            && snap.contains("\"sensitive\":true")
            && snap.contains("\"name\":\"Authorization\"")
    );

    let log = Arc::new(ErrorLog::default());
    let bad = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![Action {
                typ: "replace_url".into(),
                regex: "^.*$".into(),
                value: SECRET.into(),
                sensitive: true,
                ..Default::default()
            }],
        )],
        Options {
            trace_key: TEST_KEY.to_vec(),
            errors: Some(log.clone()),
        },
    );
    let res = bad.apply(
        PHASE_REQUEST,
        Input::new(req("GET", "http://h.example/a", Header::default(), "")),
    );
    want_lines(&res.lines, &["1:replace_url:url:error:invalid_url"]);
    assert_eq!(log.get(1).unwrap().class, ERR_CLASS_INVALID_URL);
}

#[test]
fn sensitive_taint_limit() {
    const SECRET: &str = "tok-SECRET-0002";
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![Action {
                typ: "set_header".into(),
                name: "X-Token".into(),
                value: SECRET.into(),
                sensitive: true,
                ..Default::default()
            }],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![set_header("X-Copy", SECRET)],
        ),
        rule(
            3,
            PHASE_RESPONSE,
            0,
            Matchers::default(),
            vec![set_header("X-Token", SECRET)],
        ),
    ];
    let s = cset(
        &rules,
        Options {
            trace_key: TEST_KEY.to_vec(),
            ..Options::default()
        },
    );
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req("GET", "http://h.example/", Header::default(), "")),
    );
    assert!(res.lines[0].after.is_none());
    assert_eq!(after(&res.lines[1]), SECRET.as_bytes());
    let res = s.apply(
        PHASE_RESPONSE,
        Input {
            request: res.request,
            response: resp(200, Header::default(), ""),
            stream: false,
            is_binary: None,
        },
    );
    assert_eq!(after(&res.lines[0]), SECRET.as_bytes());
}

#[test]
fn rule_snapshot() {
    let long = "v".repeat(4096 + 1);
    let short = "w".repeat(4096);
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            3,
            Matchers {
                host: "h.example".into(),
                headers: vec![HeaderMatcher {
                    name: "X-A".into(),
                    value_regex: "^matcher-in-clear$".into(),
                }],
                ..Default::default()
            },
            vec![
                set_header("X-Long", &long),
                set_header("X-Short", &short),
                Action {
                    typ: "set_method".into(),
                    value: "POST".into(),
                    ..Default::default()
                },
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            4,
            Matchers::default(),
            vec![set_header("X-B", "1")],
        ),
    ];
    let s = cset(&rules, Options::default());
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req(
            "GET",
            "http://h.example/",
            Header::from_values(&[("X-A", &["matcher-in-clear"])]),
            "",
        )),
    );
    assert_eq!(res.lines.len(), 4);
    for (i, want) in [true, false, false, true].iter().enumerate() {
        assert_eq!(!res.lines[i].rule_snapshot.is_empty(), *want, "line {i}");
    }
    let snap: serde_json::Value = serde_json::from_str(&res.lines[0].rule_snapshot).unwrap();
    assert_eq!(snap["id"], 1);
    assert_eq!(snap["name"], "r1");
    assert_eq!(snap["phase"], "request");
    assert_eq!(snap["priority"], 3);
    assert_eq!(
        snap["matchers"]["headers"][0]["value_regex"],
        "^matcher-in-clear$"
    );
    let a0 = &snap["actions"][0];
    assert!(a0["value"].is_null());
    assert_eq!(a0["value_len"], long.len());
    assert_eq!(a0["value_sha256"], sha256_hex(long.as_bytes()));
    let a1 = &snap["actions"][1];
    assert_eq!(a1["value"], short);
    assert_eq!(a1["value_len"].as_u64().unwrap_or(0), 0);
    assert!(!res.lines[0].rule_snapshot.contains(&long));

    let skip = cset(
        &[rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![Action {
                typ: "set_body".into(),
                value: "x".into(),
                ..Default::default()
            }],
        )],
        Options::default(),
    );
    let res = skip.apply(
        PHASE_REQUEST,
        Input::new(req("POST", "http://h.example/", Header::default(), "\0")),
    );
    assert_eq!(res.lines.len(), 1);
    assert!(!res.lines[0].rule_snapshot.is_empty());
}

#[test]
fn body_trace_on_last_body_action_only() {
    let rules = vec![
        rule(
            1,
            PHASE_REQUEST,
            0,
            Matchers::default(),
            vec![
                Action {
                    typ: "set_body".into(),
                    value: "one".into(),
                    ..Default::default()
                },
                Action {
                    typ: "replace_body".into(),
                    regex: "one".into(),
                    value: "two".into(),
                    ..Default::default()
                },
                set_header("X-A", "1"),
            ],
        ),
        rule(
            2,
            PHASE_REQUEST,
            1,
            Matchers::default(),
            vec![Action {
                typ: "replace_body".into(),
                regex: "two".into(),
                value: "three".into(),
                ..Default::default()
            }],
        ),
        rule(
            3,
            PHASE_REQUEST,
            2,
            Matchers::default(),
            vec![set_header("X-B", "1")],
        ),
    ];
    let s = cset(&rules, Options::default());
    let res = s.apply(
        PHASE_REQUEST,
        Input::new(req("POST", "http://h.example/", Header::default(), "zero")),
    );
    want_lines(
        &res.lines,
        &[
            "1:set_header:header:X-A:applied:",
            "1:set_body:body:applied:",
            "1:replace_body:body:applied:",
            "2:replace_body:body:applied:",
            "3:set_header:header:X-B:applied:",
        ],
    );
    for (i, want) in ["1", "", "", "three", "1"].iter().enumerate() {
        assert_eq!(
            std::str::from_utf8(after(&res.lines[i])).unwrap(),
            *want,
            "line {i}"
        );
    }
    let l = &res.lines[2];
    assert_eq!(l.before_hash, sha256_hex(b"one"));
    assert_eq!(l.after_hash, sha256_hex(b"two"));

    let sens = cset(
        &[
            rule(
                1,
                PHASE_REQUEST,
                0,
                Matchers::default(),
                vec![Action {
                    typ: "set_body".into(),
                    value: "one".into(),
                    ..Default::default()
                }],
            ),
            rule(
                2,
                PHASE_REQUEST,
                1,
                Matchers::default(),
                vec![Action {
                    typ: "set_body".into(),
                    value: "two".into(),
                    sensitive: true,
                    ..Default::default()
                }],
            ),
        ],
        Options::default(),
    );
    for l in &sens
        .apply(
            PHASE_REQUEST,
            Input::new(req("POST", "http://h.example/", Header::default(), "zero")),
        )
        .lines
    {
        assert!(l.after.is_none());
    }
}

#[test]
fn sensitive_fingerprint() {
    const SECRET: &str = "tok-SECRET-0003";
    let rules = vec![rule(
        1,
        PHASE_REQUEST,
        0,
        Matchers::default(),
        vec![Action {
            typ: "set_header".into(),
            name: "X-Token".into(),
            value: SECRET.into(),
            sensitive: true,
            ..Default::default()
        }],
    )];
    let apply = |key: &[u8]| -> TraceLine {
        let res = Set::compile(
            &rules,
            Options {
                trace_key: key.to_vec(),
                ..Options::default()
            },
        )
        .apply(
            PHASE_REQUEST,
            Input::new(req(
                "GET",
                "http://h.example/",
                Header::from_values(&[("X-Token", &["old"])]),
                "",
            )),
        );
        assert_eq!(res.lines.len(), 1);
        res.lines[0].clone()
    };
    let keyed = apply(TEST_KEY);
    let plain = sha256_hex(SECRET.as_bytes());
    assert_eq!(keyed.after_hash.len(), 32);
    assert_ne!(keyed.after_hash, plain);
    assert!(!plain.contains(&keyed.after_hash));
    assert_eq!(apply(TEST_KEY).after_hash, keyed.after_hash);
    let other = apply(b"another-key-another-key-another-k");
    assert_ne!(other.after_hash, keyed.after_hash);
    assert!(!other.after_hash.is_empty());
    let none = apply(&[]);
    assert_eq!(none.after_hash, "");
    assert_eq!(none.before_hash, sha256_hex(b"old"));
    assert!(none.after.is_none());
}

// Keep imports that are only used indirectly honest.
#[allow(dead_code)]
fn _marker(_: ApplyResult, _: Option<&'static str>, _: &str) {}
