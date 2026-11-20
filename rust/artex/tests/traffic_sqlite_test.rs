//! Smoke test for the raw libsqlite3 FFI wrapper, including FTS5 trigram
//! (the exact feature combination the traffic index relies on).

use artex::traffic::sqlite::{Conn, Value};

#[test]
fn sqlite_ffi_round_trip_and_fts5() {
    let db = Conn::open_memory().unwrap();
    db.exec("CREATE TABLE t(id TEXT PRIMARY KEY, n INTEGER, body BLOB)")
        .unwrap();
    let n = db
        .execute(
            "INSERT INTO t(id,n,body) VALUES(?,?,?)",
            &[
                Value::Text("a".into()),
                Value::Int(7),
                Value::Blob(vec![0, 1, 255]),
            ],
        )
        .unwrap();
    assert_eq!(n, 1);

    let rows = db
        .query(
            "SELECT id,n,body FROM t WHERE id=?",
            &[Value::Text("a".into())],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text(0), "a");
    assert_eq!(rows[0].int(1), 7);
    assert_eq!(rows[0].blob(2), vec![0, 1, 255]);

    let nulls = db.query("SELECT NULL, 42, 'x'", &[]).unwrap();
    assert!(nulls[0].get(0).is_null());
    assert_eq!(nulls[0].int(1), 42);
    assert_eq!(nulls[0].text(2), "x");

    db.exec("CREATE VIRTUAL TABLE f USING fts5(content, tokenize='trigram', content='', contentless_delete=1)").unwrap();
    db.execute(
        "INSERT INTO f(rowid,content) VALUES(?,?)",
        &[Value::Int(1), Value::Text("password ssw0r".into())],
    )
    .unwrap();
    let hits = db
        .query(
            "SELECT rowid FROM f WHERE f MATCH ?",
            &[Value::Text("ssw0r".into())],
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].int(0), 1);
    db.exec("INSERT INTO f(f) VALUES('optimize')").unwrap();
    db.exec("INSERT INTO f(f,rank) VALUES('merge', -256)")
        .unwrap();
}
