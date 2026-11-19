//! Differential tests for the server-log sink — fresh spec for `db/logs.go`
//! (the Go package ships no `_test.go` for it).
//!
//! Runs against `ARTEX_PG_DSN`, serialised behind one shared async lock, and skips
//! cleanly when the DSN is unset or unreachable. Uses a process-unique tag prefix
//! so the shared dev DB stays usable and the test cleans up after itself.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos + (N.fetch_add(1, Ordering::Relaxed) as u128) * 7919
}

async fn open(dsn: &str) -> Option<DB> {
    match DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

#[tokio::test]
async fn insert_and_page_logs_oldest_first() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let tag = format!("t-logs-{stamp}");

    // Insert three rows (ids are monotonically increasing).
    let id1 = d.insert_log("info", &tag, "first").await.expect("insert 1");
    let id2 = d
        .insert_log("warn", &tag, "second")
        .await
        .expect("insert 2");
    let id3 = d
        .insert_log("error", &tag, "third")
        .await
        .expect("insert 3");
    assert!(id1 < id2 && id2 < id3, "ids increase: {id1},{id2},{id3}");

    // RecentLogs returns newest-first from the DB, then reversed to oldest-first.
    let recent = d.recent_logs(1000).await.expect("recent_logs");
    let mine: Vec<&artex::db::logs::DBLog> = recent.iter().filter(|l| l.tag == tag).collect();
    let texts: Vec<&str> = mine.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["first", "second", "third"],
        "oldest-first order"
    );

    // Field round-trip.
    let last = mine.last().expect("has rows");
    assert_eq!(last.level, "error");
    assert_eq!(last.id, id3);

    // ListLogsBefore pages backwards: before id3+1 limit 2 → [second, third]? No —
    // id < (id3+1) ordered DESC then reversed → oldest-first [second, third] would
    // require id < id3 to exclude third. Use before=id3 → ids {id1,id2} reversed.
    let page = d
        .list_logs_before(id3, 1000)
        .await
        .expect("list_logs_before");
    let page_texts: Vec<&str> = page
        .iter()
        .filter(|l| l.tag == tag)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        page_texts,
        vec!["first", "second"],
        "id < id3, oldest-first"
    );

    // LIMIT is respected (before=id3+1, limit=1 → newest single row = third).
    let capped = d.list_logs_before(id3 + 1, 1).await.expect("capped");
    let capped_mine: Vec<&str> = capped
        .iter()
        .filter(|l| l.tag == tag)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        capped_mine,
        vec!["third"],
        "limit=1 newest-first then reversed single"
    );

    // Cleanup only this test's rows.
    d.execute("DELETE FROM server_logs WHERE tag=$1", &[&tag])
        .await
        .expect("cleanup");
}
