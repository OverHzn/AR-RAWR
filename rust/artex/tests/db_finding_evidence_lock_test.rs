//! Differential tests for the lock/read/edit slice of `db/finding_traffic.go`
//! (`lockTaskEvidenceTx` / `lockFindingEvidenceTx` / `FindingTrafficTx` /
//! `GetFindingTraffic` / `EditFindingTraffic`), built on the D-5a primitives.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::finding_evidence::{
    lock_finding_evidence_tx, lock_task_evidence_tx, traffic_snapshot_id, PreparedTrafficEvidence,
    TrafficEvidenceSnapshot, TrafficRef, ERR_EVIDENCE_CONFLICT, ERR_EVIDENCE_NOT_FOUND,
    ERR_FINDING_NOT_FOUND, ERR_TASK_ARCHIVE_STATE,
};
use artex::db::secrets::{set_active_secret_box, SecretBox};
use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
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

fn install_test_secret_box() {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    set_active_secret_box(SecretBox::new(&key).expect("32-byte key"));
}

/// Seed a finding + one bound snapshot; returns (task_id, finding_id, snapshot_id).
async fn seed_finding_with_traffic(d: &DB, role: &str) -> (i64, i64, String) {
    let t = d.create_task("ev-lock", "goal", None, 0, 0).await.unwrap();
    let fid = d
        .add_finding(t.id, 0, "rce", "", "critical", "s", "e", "w", &[])
        .await
        .unwrap();
    let mut snap = TrafficEvidenceSnapshot {
        source_traffic_id: format!("src-{fid}"),
        captured_at: 1_700_000_000,
        url: "https://x/y".to_string(),
        method: "GET".to_string(),
        status: 200,
        content_type: "text/html".to_string(),
        req_head: "GET / HTTP/1.1\r\n".to_string(),
        resp_head: "HTTP/1.1 200 OK\r\n".to_string(),
        req_hash: "aa".to_string(),
        resp_hash: "bb".to_string(),
        req_len: 14,
        resp_len: 17,
        ..TrafficEvidenceSnapshot::default()
    };
    snap.id = traffic_snapshot_id(&snap);
    let sid = snap.id.clone();
    let prepared = vec![PreparedTrafficEvidence {
        reference: TrafficRef {
            traffic_id: "t".to_string(),
            role: role.to_string(),
            note: "n".to_string(),
        },
        snapshot: snap,
    }];
    let mut client = d.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    d.add_finding_traffic_tx(&tx, fid, &prepared).await.unwrap();
    tx.commit().await.unwrap();
    (t.id, fid, sid)
}

async fn drop_task(d: &DB, task_id: i64) {
    d.delete_task(task_id).await.unwrap();
    // Snapshots have no FK to tasks/findings (GC sets unreferenced_at); purge any
    // the test orphaned so the shared DB stays clean.
    let _ = d
        .execute(
            "DELETE FROM traffic_evidence_snapshots              WHERE NOT EXISTS (SELECT 1 FROM finding_traffic_bindings b WHERE b.snapshot_id=traffic_evidence_snapshots.id)",
            &[],
        )
        .await;
}

#[tokio::test]
async fn lock_task_evidence_states() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let mut client = d.pool.get().await.unwrap();

    // task_id 0 -> no-op OK.
    let tx = client.transaction().await.unwrap();
    lock_task_evidence_tx(&tx, 0)
        .await
        .expect("task 0 is a no-op");
    tx.rollback().await.unwrap();

    // Live task -> OK.
    let t = d
        .create_task("lock-live", "goal", None, 0, 0)
        .await
        .unwrap();
    let tx = client.transaction().await.unwrap();
    lock_task_evidence_tx(&tx, t.id)
        .await
        .expect("live task ok");
    tx.rollback().await.unwrap();

    // Archived task (state != archive_failed) -> archive-state sentinel.
    d.execute(
        "INSERT INTO task_archives(task_id, state) VALUES ($1,'ready')",
        &[&t.id],
    )
    .await
    .unwrap();
    let tx = client.transaction().await.unwrap();
    let err = lock_task_evidence_tx(&tx, t.id)
        .await
        .expect_err("archived rejected");
    assert!(
        err.is(ERR_TASK_ARCHIVE_STATE),
        "archived -> sentinel, got {err}"
    );
    tx.rollback().await.unwrap();

    // archive_failed state -> allowed.
    d.execute(
        "UPDATE task_archives SET state='archive_failed' WHERE task_id=$1",
        &[&t.id],
    )
    .await
    .unwrap();
    let tx = client.transaction().await.unwrap();
    lock_task_evidence_tx(&tx, t.id)
        .await
        .expect("archive_failed allows");
    tx.rollback().await.unwrap();
    let _ = d
        .execute("DELETE FROM task_archives WHERE task_id=$1", &[&t.id])
        .await;

    // Soft-deleted task -> archive-state sentinel.
    d.execute("UPDATE tasks SET deleted_at=now() WHERE id=$1", &[&t.id])
        .await
        .unwrap();
    let tx = client.transaction().await.unwrap();
    let err = lock_task_evidence_tx(&tx, t.id)
        .await
        .expect_err("deleted rejected");
    assert!(
        err.is(ERR_TASK_ARCHIVE_STATE),
        "deleted -> sentinel, got {err}"
    );
    tx.rollback().await.unwrap();

    // Missing task -> raw "no rows" error.
    let tx = client.transaction().await.unwrap();
    let err = lock_task_evidence_tx(&tx, 99_999_999)
        .await
        .expect_err("missing rejected");
    assert!(
        err.to_string().contains("no rows"),
        "missing -> no rows, got {err}"
    );
    tx.rollback().await.unwrap();

    drop_task(&d, t.id).await;
}

#[tokio::test]
async fn lock_finding_evidence_version_guard() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let (tid, fid, _sid) = seed_finding_with_traffic(&d, "proof").await;
    let current: i64 = d
        .query_one("SELECT evidence_version FROM findings WHERE id=$1", &[&fid])
        .await
        .unwrap()
        .get(0);

    let mut client = d.pool.get().await.unwrap();

    // Missing finding -> not-found sentinel.
    let tx = client.transaction().await.unwrap();
    let err = lock_finding_evidence_tx(&tx, 99_999_999, None)
        .await
        .expect_err("missing rejected");
    assert!(
        err.is(ERR_FINDING_NOT_FOUND),
        "missing -> sentinel, got {err}"
    );
    tx.rollback().await.unwrap();

    // Correct version -> OK.
    let tx = client.transaction().await.unwrap();
    lock_finding_evidence_tx(&tx, fid, Some(current))
        .await
        .expect("matching version ok");
    tx.rollback().await.unwrap();

    // Wrong version -> evidence-conflict sentinel.
    let tx = client.transaction().await.unwrap();
    let err = lock_finding_evidence_tx(&tx, fid, Some(current + 1))
        .await
        .expect_err("version mismatch rejected");
    assert!(
        err.is(ERR_EVIDENCE_CONFLICT),
        "mismatch -> conflict, got {err}"
    );
    tx.rollback().await.unwrap();

    // None version -> skips the optimistic check, OK.
    let tx = client.transaction().await.unwrap();
    lock_finding_evidence_tx(&tx, fid, None)
        .await
        .expect("none version ok");
    tx.rollback().await.unwrap();

    drop_task(&d, tid).await;
}

#[tokio::test]
async fn get_finding_traffic_reads_and_decrypts() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let (tid, fid, sid) = seed_finding_with_traffic(&d, "baseline").await;
    let traffic = d.get_finding_traffic(fid).await.expect("get traffic");
    assert_eq!(traffic.finding_id, fid);
    assert_eq!(traffic.version, 1, "one binding -> version 1");
    assert_eq!(traffic.bindings.len(), 1);
    let b = &traffic.bindings[0];
    assert_eq!(b.snapshot_id, sid);
    assert_eq!(b.role, "baseline");
    assert_eq!(b.position, 0);
    // Head decrypted back to cleartext.
    assert_eq!(b.snapshot.req_head, "GET / HTTP/1.1\r\n");
    assert_eq!(b.snapshot.resp_head, "HTTP/1.1 200 OK\r\n");
    assert_eq!(b.snapshot.url, "https://x/y");
    assert_eq!(b.snapshot.status, 200);

    // Missing finding -> not-found sentinel.
    let err = d
        .get_finding_traffic(99_999_999)
        .await
        .expect_err("missing");
    assert!(
        err.is(ERR_FINDING_NOT_FOUND),
        "missing -> sentinel, got {err}"
    );

    drop_task(&d, tid).await;
}

#[tokio::test]
async fn edit_finding_traffic_patch_reorder_remove() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    // Two bindings so reorder is meaningful.
    let (tid, fid, _sid) = seed_finding_with_traffic(&d, "proof").await;
    let mut snap2 = TrafficEvidenceSnapshot {
        source_traffic_id: format!("src2-{fid}"),
        captured_at: 1_700_000_001,
        url: "https://x/z".to_string(),
        method: "GET".to_string(),
        status: 200,
        req_hash: "cc".to_string(),
        resp_hash: "dd".to_string(),
        ..TrafficEvidenceSnapshot::default()
    };
    snap2.id = traffic_snapshot_id(&snap2);
    let prepared2 = vec![PreparedTrafficEvidence {
        reference: TrafficRef {
            traffic_id: "t2".to_string(),
            role: "supporting".to_string(),
            note: "n2".to_string(),
        },
        snapshot: snap2,
    }];
    let mut client = d.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    d.add_finding_traffic_tx(&tx, fid, &prepared2)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let before = d.get_finding_traffic(fid).await.unwrap();
    assert_eq!(before.bindings.len(), 2);
    let (b0, b1) = (before.bindings[0].id, before.bindings[1].id);
    let version = before.version;

    // Patch role+note of b0 (COALESCE: only provided fields change).
    d.edit_finding_traffic(
        fid,
        b0,
        version,
        Some("verification"),
        Some("patched"),
        false,
        None,
    )
    .await
    .expect("patch");
    let after = d.get_finding_traffic(fid).await.unwrap();
    let b0_new = after.bindings.iter().find(|b| b.id == b0).unwrap();
    assert_eq!(b0_new.role, "verification");
    assert_eq!(b0_new.note, "patched");
    // version bumped.
    assert!(after.version > version, "edit bumped evidence_version");

    // Invalid role -> error, no change.
    let err = d
        .edit_finding_traffic(fid, b0, after.version, Some("bogus"), None, false, None)
        .await
        .expect_err("bad role rejected");
    assert!(
        err.to_string().contains("invalid traffic role"),
        "got {err}"
    );

    // Stale version -> conflict.
    let err = d
        .edit_finding_traffic(fid, b0, version, None, None, false, None)
        .await
        .expect_err("stale version rejected");
    assert!(
        err.is(ERR_EVIDENCE_CONFLICT),
        "stale -> conflict, got {err}"
    );

    // Reorder: swap [b0,b1] -> [b1,b0].
    let cur = d.get_finding_traffic(fid).await.unwrap();
    d.edit_finding_traffic(fid, 0, cur.version, None, None, false, Some(&[b1, b0]))
        .await
        .expect("reorder");
    let reordered = d.get_finding_traffic(fid).await.unwrap();
    assert_eq!(reordered.bindings[0].id, b1, "reordered first = b1");
    assert_eq!(reordered.bindings[1].id, b0);

    // Remove b1.
    let cur = d.get_finding_traffic(fid).await.unwrap();
    d.edit_finding_traffic(fid, b1, cur.version, None, None, true, None)
        .await
        .expect("remove");
    let after = d.get_finding_traffic(fid).await.unwrap();
    assert_eq!(after.bindings.len(), 1);
    assert_eq!(after.bindings[0].id, b0);

    // Remove a missing binding -> evidence-not-found.
    let cur = d.get_finding_traffic(fid).await.unwrap();
    let err = d
        .edit_finding_traffic(fid, 99_999_999, cur.version, None, None, true, None)
        .await
        .expect_err("missing binding rejected");
    assert!(
        err.is(ERR_EVIDENCE_NOT_FOUND),
        "missing -> sentinel, got {err}"
    );

    drop_task(&d, tid).await;
}
