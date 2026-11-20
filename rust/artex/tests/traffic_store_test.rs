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
