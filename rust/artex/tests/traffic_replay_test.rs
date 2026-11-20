//! Differential tests for `traffic/replay.go` pure validation — ports of
//! `TestReplayRejectsInvalidRequests` and `TestReplayRecordedHelper`.

use artex::traffic::replay::{
    replay_recorded, validate_replay_request, ReplayHeader, ReplayRequest, ERR_REPLAY_INVALID,
    ERR_REPLAY_TOO_LARGE, MAX_REPLAY_BODY, MAX_REPLAY_HEADERS,
};

fn req(method: &str, url: &str) -> ReplayRequest {
    ReplayRequest {
        method: method.into(),
        url: url.into(),
        ..Default::default()
    }
}

#[test]
fn replay_rejects_invalid_requests() {
    let many: Vec<ReplayHeader> = (0..MAX_REPLAY_HEADERS + 1)
        .map(|i| ReplayHeader {
            name: format!("X-H{i}"),
            value: "v".into(),
        })
        .collect();
    let cases = vec![
        req("GET", "ftp://example.com/x"),
        req("GET", "/x"),
        req("GET", "http:///x"),
        req("GET", "http://user:pass@example.com/"),
        req(" ", "http://example.com/"),
        req("CONNECT", "http://example.com/"),
        req("GE T", "http://example.com/"),
        ReplayRequest {
            headers: many,
            ..req("GET", "http://example.com/")
        },
        ReplayRequest {
            headers: vec![ReplayHeader {
                name: "X-A".into(),
                value: "a\r\nX-Injected: 1".into(),
            }],
            ..req("GET", "http://example.com/")
        },
        ReplayRequest {
            headers: vec![ReplayHeader {
                name: "X-A".into(),
                value: "a\nb".into(),
            }],
            ..req("GET", "http://example.com/")
        },
        ReplayRequest {
            headers: vec![ReplayHeader {
                name: "Bad Name".into(),
                value: "v".into(),
            }],
            ..req("GET", "http://example.com/")
        },
        ReplayRequest {
            headers: vec![ReplayHeader {
                name: "Upgrade".into(),
                value: "websocket".into(),
            }],
            ..req("GET", "http://example.com/")
        },
        ReplayRequest {
            headers: vec![ReplayHeader {
                name: "Host".into(),
                value: "other.example".into(),
            }],
            ..req("GET", "http://example.com/")
        },
    ];
    for (i, r) in cases.iter().enumerate() {
        let err = validate_replay_request(r).unwrap_err();
        assert!(err.is(ERR_REPLAY_INVALID), "case {i}: {err}");
    }
    let big = ReplayRequest {
        body: vec![0u8; MAX_REPLAY_BODY + 1],
        ..req("POST", "http://example.com/")
    };
    let err = validate_replay_request(&big).unwrap_err();
    assert!(
        err.is(ERR_REPLAY_TOO_LARGE) && !err.is(ERR_REPLAY_INVALID),
        "got {err}"
    );
}

#[test]
fn replay_drops_hop_by_hop_and_sets_user_agent() {
    let r = ReplayRequest {
        headers: vec![
            ReplayHeader {
                name: "x-a".into(),
                value: "1".into(),
            },
            ReplayHeader {
                name: "Connection".into(),
                value: "keep-alive, X-Hop".into(),
            },
            ReplayHeader {
                name: "X-Hop".into(),
                value: "2".into(),
            },
            ReplayHeader {
                name: "Content-Length".into(),
                value: "5".into(),
            },
            ReplayHeader {
                name: "host".into(),
                value: "example.com".into(),
            },
        ],
        ..req("GET", "http://example.com/x")
    };
    let out = validate_replay_request(&r).unwrap();
    let names: Vec<&str> = out.headers.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, vec!["X-A", "User-Agent"]);
    assert_eq!(out.headers[1].value, "");
    assert_eq!(out.host, "example.com");
}

#[test]
fn replay_recorded_helper() {
    let tunneled = |h: &str| h == "localhost";
    assert!(!replay_recorded("https", "localhost:443", tunneled));
    assert!(replay_recorded("https", "example.com:443", tunneled));
    assert!(replay_recorded("http", "localhost:80", tunneled));
    assert!(replay_recorded("https", "expired.example:443", tunneled));
}
