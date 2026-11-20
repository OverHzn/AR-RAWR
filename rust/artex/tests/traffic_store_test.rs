//! Differential tests for the traffic index foundation — ports of Go
//! `TestRecordKeepsBodiesInIndex`, `TestGetFallsBackToLegacyTree`,
//! `TestHosts`, `TestFullTextSearchMatchesBodies`,
//! `TestLargeBodySpillsButStaysSearchable`, `TestBinaryBodyStaysOutOfIndex`.

use artex::traffic::sqlite::Value;
use artex::traffic::store::{ExchangeMeta, TrafficStore, BLOB_PREVIEW, MAX_INLINE_BODY};

fn meta(id: &str, host: &str, status: i32, url: &str) -> ExchangeMeta {
    ExchangeMeta {
        id: id.to_string(),
        ts: 1_700_000_000,
        host: host.to_string(),
        method: "GET".into(),
        url_template: "/x".into(),
        url: url.to_string(),
        status,
        content_type: "text/plain".into(),
        req_len: 0,
        resp_len: 0,
        path: String::new(),
    }
}

fn insert(store: &TrafficStore, meta: &ExchangeMeta, req: &[u8], resp: &[u8], resp_ct: &str) {
    let req_body = store.spill(req, "text/plain");
    let resp_body = store.spill(resp, resp_ct);
    let fts = format!(
        "{}\n{}\n{}\n{}\n{}",
        meta.url, "GET /x HTTP/1.1", req_body.index, "HTTP/1.1", resp_body.index
    );
    store
        .insert_exchange(
            meta,
            "GET /x HTTP/1.1",
            &req_body,
            "HTTP/1.1 200",
            &resp_body,
            req.len(),
            resp.len(),
            &fts,
        )
        .unwrap();
}

#[test]
fn record_get_count_hosts_and_fts() {
    let s = TrafficStore::open_temp().unwrap();
    let m = meta(
        "1700000000-0001",
        "api.example.com",
        200,
        "https://api.example.com/x",
    );
    insert(&s, &m, b"hello request", b"P@ssw0rd secret", "text/plain");
    assert_eq!(s.count().unwrap(), 1);

    let (req, resp) = s.get(&m.id).unwrap();
    assert!(req.contains("hello request"), "request: {req}");
    assert!(resp.contains("P@ssw0rd secret"), "response: {resp}");

    let hosts = s
        .query("SELECT host, COUNT(*) FROM exchanges GROUP BY host", &[])
        .unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].text(0), "api.example.com");
    assert_eq!(hosts[0].int(1), 1);

    if s.fts() {
        let hits = s
            .query(
                "SELECT rowid FROM ex_fts WHERE ex_fts MATCH ?",
                &[Value::Text("\"ssw0r\"".into())],
            )
            .unwrap();
        assert_eq!(hits.len(), 1, "trigram substring search");
    }
}

#[test]
fn spill_and_blob_store() {
    let s = TrafficStore::open_temp().unwrap();
    let small = s.spill(b"tiny", "text/plain");
    assert!(small.hash.is_empty());
    assert_eq!(small.inline, b"tiny");

    let big = vec![b'a'; MAX_INLINE_BODY + 1024];
    let spilled = s.spill(&big, "text/plain");
    assert_eq!(spilled.hash.len(), 64);
    assert!(spilled.inline.len() <= BLOB_PREVIEW);
    assert_eq!(spilled.index.len(), big.len());
    let path = s.blob_path(&spilled.hash).expect("blob path");
    assert_eq!(std::fs::read(&path).unwrap(), big);
    assert!(
        s.blob_path(&"z".repeat(64)).is_none(),
        "invalid hash refused"
    );

    // Binary bodies are never indexed and get a magic-byte preview.
    let mut bin_body = vec![0u8; MAX_INLINE_BODY + 8];
    bin_body[..4].copy_from_slice(&[0x89, b'P', b'N', b'G']);
    let bin = s.spill(&bin_body, "image/png");
    assert_eq!(bin.hash.len(), 64);
    assert!(bin.index.is_empty());
    assert!(String::from_utf8_lossy(&bin.inline).contains("magic=89504e47"));
}

#[test]
fn spilled_body_get_shows_blob_pointer() {
    let s = TrafficStore::open_temp().unwrap();
    let big = vec![b'z'; MAX_INLINE_BODY + 16];
    let m = meta(
        "1700000000-0002",
        "big.example.com",
        200,
        "https://big.example.com/blob",
    );
    insert(&s, &m, b"req", &big, "text/plain");
    let (_, resp) = s.get(&m.id).unwrap();
    assert!(
        resp.contains("@blob sha256:"),
        "blob pointer missing: {}",
        &resp[..resp.len().min(200)]
    );
    let refs = s
        .query(
            "SELECT hash FROM blob_refs WHERE exchange_id=?",
            &[Value::Text(m.id.clone())],
        )
        .unwrap();
    assert_eq!(refs.len(), 1);
}

#[test]
fn page_filters_sort_and_fts() {
    use artex::traffic::store::PageQuery;

    let s = TrafficStore::open_temp().unwrap();
    for (i, (host, status, body)) in [
        ("api.example.com", 200, "P@ssw0rd stored"),
        ("api.example.com", 404, "not found here"),
        ("other.test", 200, "unrelated text"),
    ]
    .iter()
    .enumerate()
    {
        let m = meta(
            &format!("1700000000-{:04}", i),
            host,
            *status,
            &format!("https://{host}/x"),
        );
        insert(&s, &m, b"req", body.as_bytes(), "text/plain");
    }

    let all = s.page(&PageQuery::default(), 0, 10).unwrap();
    assert_eq!(all.1, 3);
    assert_eq!(all.0.len(), 3);
    assert_eq!(
        all.0[0].host, "other.test",
        "default newest-first by id desc tie-break"
    );

    let filtered = s
        .page(
            &PageQuery {
                host: "api".into(),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(filtered.1, 2);
    let status = s
        .page(
            &PageQuery {
                status: "4xx".into(),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(status.1, 1);
    assert_eq!(status.0[0].status, 404);
    let asc = s
        .page(
            &PageQuery {
                order: "asc".into(),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(asc.0[0].id, "1700000000-0002", "id DESC tie-break");
    let paged = s.page(&PageQuery::default(), 1, 2).unwrap();
    assert_eq!(paged.0.len(), 1);

    if s.fts() {
        let hits = s
            .page(
                &PageQuery {
                    body: "ssw0r".into(),
                    ..Default::default()
                },
                0,
                10,
            )
            .unwrap();
        assert_eq!(hits.1, 1);
        assert_eq!(hits.0[0].host, "api.example.com");
    }
    let searched = s.search("other.test", 0, 10).unwrap();
    assert_eq!(searched.len(), 1);
}

#[test]
fn blob_range_bounds() {
    let s = TrafficStore::open_temp().unwrap();
    let big = (0u8..=255)
        .cycle()
        .take(MAX_INLINE_BODY + 2048)
        .collect::<Vec<_>>();
    let spilled = s.spill(&big, "text/plain");
    let (all, size) = s.blob_range(&spilled.hash, 0, -1).unwrap();
    assert_eq!(size as usize, big.len());
    assert_eq!(all.len(), big.len());
    let (part, total) = s.blob_range(&spilled.hash, 10, 5).unwrap();
    assert_eq!(total as usize, big.len());
    assert_eq!(part, &big[10..15]);
    let (empty, total) = s.blob_range(&spilled.hash, size, 4).unwrap();
    assert!(empty.is_empty());
    assert_eq!(total, size);
}

#[test]
fn delete_host_and_gc_blobs() {
    let s = TrafficStore::open_temp().unwrap();
    let big = vec![b'q'; MAX_INLINE_BODY + 32];
    let m1 = meta(
        "1700000000-0010",
        "victim.example.com",
        200,
        "https://victim.example.com/x",
    );
    let m2 = meta(
        "1700000000-0011",
        "keeper.example.com",
        200,
        "https://keeper.example.com/y",
    );
    insert(&s, &m1, b"req", &big, "text/plain");
    insert(&s, &m2, b"req", &big, "text/plain");
    assert_eq!(s.count().unwrap(), 2);
    let spilled = s.spill(&big, "text/plain");
    assert!(s.blob_path(&spilled.hash).is_some());

    let n = s.delete_host("victim").unwrap();
    assert_eq!(n, 1);
    assert_eq!(s.count().unwrap(), 1);
    // Shared blob survives because the keeper still references it.
    assert!(s.blob_path(&spilled.hash).is_some());

    assert_eq!(
        s.delete_hosts_exact(&["keeper.example.com".to_string()])
            .unwrap(),
        1
    );
    assert_eq!(s.count().unwrap(), 0);
    s.gc_blobs().unwrap();
    assert!(
        s.blob_path(&spilled.hash).is_none(),
        "unreferenced blob collected"
    );
}

#[test]
fn delete_all_clears_index() {
    let s = TrafficStore::open_temp().unwrap();
    for i in 0..3 {
        let m = meta(
            &format!("1700000000-{i:04}"),
            "host.test",
            200,
            "https://host.test/x",
        );
        insert(&s, &m, b"req", b"resp", "text/plain");
    }
    let (n, _reclaimed) = s.delete_all().unwrap();
    assert_eq!(n, 3);
    assert_eq!(s.count().unwrap(), 0);
    assert_eq!(
        s.page(&artex::traffic::store::PageQuery::default(), 0, 10)
            .unwrap()
            .0
            .len(),
        0
    );
}

#[test]
fn rewrite_trace_persistence_and_info() {
    use artex::traffic::rewrite_engine::{TraceLine, OUTCOME_APPLIED, OUTCOME_SKIPPED};

    let s = TrafficStore::open_temp().unwrap();
    let id = "1700000000-0100";
    let lines = vec![
        TraceLine {
            phase: "request".into(),
            rule_id: 7,
            action: "set_header".into(),
            field: "header:X-A".into(),
            outcome: OUTCOME_APPLIED.into(),
            after: Some(b"v".to_vec()),
            ..Default::default()
        },
        TraceLine {
            phase: "response".into(),
            rule_id: 9,
            outcome: OUTCOME_APPLIED.into(),
            ..Default::default()
        },
        TraceLine {
            phase: "request".into(),
            rule_id: 3,
            outcome: OUTCOME_SKIPPED.into(),
            ..Default::default()
        },
    ];
    let trace = s.spill_trace(&lines);
    s.insert_trace(id, &lines, &trace).unwrap();

    let rows = s.query(
        "SELECT seq, outcome, after_inline FROM exchange_rewrites WHERE exchange_id=? ORDER BY seq",
        &[artex::traffic::sqlite::Value::Text(id.to_string())],
    ).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].text(1), OUTCOME_APPLIED);
    assert_eq!(rows[0].blob(2), b"v");
    assert_eq!(rows[1].int(0), 1);
    assert!(s.rewrite_info(id).unwrap() == (true, vec![7, 9]));
    assert_eq!(s.rewrite_info("missing").unwrap(), (false, vec![]));
}

#[test]
fn stream_messages_round_trip() {
    use artex::traffic::store::StreamMessage;

    let s = TrafficStore::open_temp().unwrap();
    let id = "1700000000-0200";
    for seq in 0..3 {
        let m = StreamMessage {
            seq,
            ts: 1_700_000_000 + seq,
            direction: "s2c".into(),
            kind: 1,
            event: "message".into(),
            event_id: format!("e{seq}"),
            retry: 0,
            size: 5,
            payload: b"hello".to_vec(),
            blob: String::new(),
            truncated: false,
        };
        s.insert_stream_message(id, &m).unwrap();
    }
    let (rows, total) = s.stream_messages(id, 0, 10).unwrap();
    assert_eq!(total, 3);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].seq, 0);
    assert_eq!(rows[2].event_id, "e2");
    assert_eq!(rows[0].payload, b"hello");
    let (page, _) = s.stream_messages(id, 1, 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].seq, 1);
}

#[test]
fn staged_delete_commit_and_rollback() {
    let s = TrafficStore::open_temp().unwrap();
    for (i, host) in ["a.test", "b.test", "a.test"].iter().enumerate() {
        let m = meta(
            &format!("1700000000-{i:04}"),
            host,
            200,
            &format!("https://{host}/x"),
        );
        insert(&s, &m, b"req", b"resp", "text/plain");
    }
    let stage = s
        .stage_delete_hosts_exact(&["a.test".to_string(), "a.test".to_string()])
        .unwrap();
    assert_eq!(stage.deleted(), 2);
    // Rollback leaves the index untouched.
    stage.rollback();
    assert_eq!(s.count().unwrap(), 3);

    let stage = s.stage_delete_hosts_exact(&["b.test".to_string()]).unwrap();
    assert_eq!(stage.deleted(), 1);
    assert_eq!(stage.commit().unwrap(), 1);
    assert_eq!(s.count().unwrap(), 2);
    assert_eq!(
        s.query("SELECT DISTINCT host FROM exchanges", &[])
            .unwrap()
            .len(),
        1
    );

    let _ = s.reclaim(10).unwrap();
}

#[test]
fn record_external_stores_like_proxy_traffic() {
    use artex::traffic::store::ExternalExchange;

    let s = TrafficStore::open_temp().unwrap();
    let mut rh = std::collections::BTreeMap::new();
    rh.insert("content-type".to_string(), "application/json".to_string());
    rh.insert("authorization".to_string(), "Bearer xyz".to_string());
    let mut sh = std::collections::BTreeMap::new();
    sh.insert("content-type".to_string(), "application/json".to_string());
    let e = ExternalExchange {
        method: "POST".into(),
        url: "https://api.example.com/v1/login".into(),
        status: 200,
        request_headers: rh,
        request_body: r#"{"user":"u1","password":"p1"}"#.into(),
        response_headers: sh,
        response_body: r#"{"token":"tok"}"#.into(),
    };
    s.record_external(&e).unwrap();
    assert_eq!(s.count().unwrap(), 1);
    let rows = s
        .page(
            &artex::traffic::store::PageQuery {
                query: "login".into(),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(rows.1, 1);
    let (req, resp) = s.get(&rows.0[0].id).unwrap();
    assert!(
        req.contains("POST /v1/login")
            && req.contains("Bearer xyz")
            && req.contains("\"user\":\"u1\"")
    );
    assert!(resp.contains(r#"{"token":"tok"}"#));
}

#[test]
fn record_external_rejects_bad_input() {
    use artex::traffic::store::ExternalExchange;

    let s = TrafficStore::open_temp().unwrap();
    let mut bad = ExternalExchange {
        method: "GET".into(),
        url: "not a url".into(),
        ..Default::default()
    };
    assert!(s.record_external(&bad).is_err());
    bad.url = "https://example.com/".into();
    bad.method = String::new();
    assert!(s.record_external(&bad).is_err());
    bad.method = "GE T".into();
    assert!(s.record_external(&bad).is_err());
    bad.method = "GET".into();
    bad.request_headers
        .insert("X-Bad".into(), "a\r\nInjected: 1".into());
    assert!(s.record_external(&bad).is_err());
    assert_eq!(s.count().unwrap(), 0);
}
