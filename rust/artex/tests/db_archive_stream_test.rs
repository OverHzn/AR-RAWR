//! Differential tests for `db/task_archives.go` streaming helpers — ports of
//! `TestQueryArchiveRowsStreamsJSON` and
//! `TestWriteArchiveRowsProducesJSONSequence`.

use artex::db::archive_stream::{encode_archive_rows, write_archive_rows};

#[test]
fn encode_archive_rows_streams_json() {
    let payload = "large request/response payload ".repeat(64 * 1024);
    let values = vec![
        serde_json::to_vec(&serde_json::json!({"id": 1i64, "body": payload})).unwrap(),
        serde_json::to_vec(&serde_json::json!({"id": 2i64, "body": "quoted: \"value\"\nline"}))
            .unwrap(),
    ];
    let rows: Vec<Result<Vec<u8>, std::io::Error>> = values.into_iter().map(Ok).collect();
    let (raw, count) = encode_archive_rows(rows).unwrap();
    assert_eq!(count, 2);
    let decoded: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(decoded.as_array().unwrap().len(), 2);
    assert_eq!(decoded[0]["id"], 1);
    assert_eq!(decoded[0]["body"], payload);
    assert_eq!(decoded[1]["id"], 2);
    assert_eq!(decoded[1]["body"], "quoted: \"value\"\nline");
}

#[test]
fn write_archive_rows_produces_json_sequence() {
    let values: Vec<Vec<u8>> = vec![
        br#"{"id":1,"body":"first"}"#.to_vec(),
        br#"{"id":2,"body":"second\nline"}"#.to_vec(),
    ];
    let rows: Vec<Result<Vec<u8>, std::io::Error>> = values.into_iter().map(Ok).collect();
    let mut out = Vec::new();
    assert_eq!(
        write_archive_rows::<_, std::io::Error, _>(rows, &mut out).unwrap(),
        2
    );
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    for (i, line) in lines.iter().enumerate() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(row["id"], (i + 1) as u64);
    }
}

#[tokio::test]
async fn query_archive_rows_against_postgres() {
    let Ok(dsn) = std::env::var("ARTEX_PG_DSN") else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    if dsn.trim().is_empty() {
        eprintln!("skipping: ARTEX_PG_DSN empty");
        return;
    }
    let Ok(d) = artex::db::DB::open(&dsn).await else {
        eprintln!("skipping: postgres unavailable");
        return;
    };
    let (raw, count) = d
        .query_archive_rows("SELECT 1::bigint AS id, 'hello'::text AS body", &[])
        .await
        .unwrap();
    assert_eq!(count, 1);
    let rows: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(rows[0]["id"], "1"); // row_to_json renders bigint as a JSON number
    assert_eq!(rows[0]["body"], "hello");
}
