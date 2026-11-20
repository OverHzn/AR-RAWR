//! Differential tests for the traffic-evidence primitives slice of
//! `db/finding_traffic.go` (`trafficSnapshotID` / `NormalizeTrafficRefs` /
//! `TrafficEvidenceSnapshot::normalize` / `with_evidence_tx` /
//! `insert_evidence_snapshot_tx` + `open_heads` / `bump_evidence_version_tx` /
//! `add_finding_traffic_tx`). `lock_*` / `finding_traffic_tx` / `get_*` /
//! `edit_*` (D-5b) and `record_finding*` (D-5c) are later slices, so fixtures are
//! seeded via raw SQL.
//!
//! The evidence head columns are sealed at rest; this test drives the active
//! secret box directly so it does not depend on the startup key/backfill path.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::finding_evidence::{
    normalize_traffic_refs, open_snapshot_heads, traffic_snapshot_id, valid_traffic_role,
    PreparedTrafficEvidence, TrafficEvidenceSnapshot, TrafficRef, ERR_EVIDENCE_CONFLICT,
    ERR_EVIDENCE_NOT_FOUND, ERR_FINDING_NOT_FOUND, FINDING_EVIDENCE_LOCK_KEY,
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

/// Install a fixed test secret box so the evidence heads seal/decrypt.
fn install_test_secret_box() {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    let b = SecretBox::new(&key).expect("32-byte key");
    set_active_secret_box(b);
}

#[test]
fn consts_and_validators() {
    // Sentinels are distinct and non-empty.
    assert!(!ERR_EVIDENCE_CONFLICT.is_empty());
    assert!(!ERR_EVIDENCE_NOT_FOUND.is_empty());
    assert!(!ERR_FINDING_NOT_FOUND.is_empty());
    assert_eq!(FINDING_EVIDENCE_LOCK_KEY, 7_337_741_004);
    // Roles.
    for r in ["baseline", "proof", "verification", "supporting"] {
        assert!(valid_traffic_role(r), "valid role {r}");
    }
    assert!(!valid_traffic_role("other"));
    assert!(!valid_traffic_role(""));
}

#[test]
fn normalize_traffic_refs_defaults_and_dedup() {
    // Empty id -> error.
    let bad = vec![TrafficRef {
        traffic_id: "  ".to_string(),
        role: String::new(),
        note: String::new(),
    }];
    assert!(normalize_traffic_refs(&bad).is_err(), "empty id rejected");

    // Bad role -> error.
    let bad = vec![TrafficRef {
        traffic_id: "t1".to_string(),
        role: "nope".to_string(),
        note: String::new(),
    }];
    assert!(normalize_traffic_refs(&bad).is_err(), "bad role rejected");

    // Default role = supporting; trim id; dedupe (first wins).
    let refs = vec![
        TrafficRef {
            traffic_id: "  t1  ".to_string(),
            role: String::new(),
            note: "a".to_string(),
        },
        TrafficRef {
            traffic_id: "t1".to_string(),
            role: "proof".to_string(),
            note: "b".to_string(),
        },
        TrafficRef {
            traffic_id: "t2".to_string(),
            role: "baseline".to_string(),
            note: "c".to_string(),
        },
    ];
    let out = normalize_traffic_refs(&refs).expect("normalize");
    assert_eq!(out.len(), 2, "duplicate t1 dropped");
    assert_eq!(out[0].traffic_id, "t1");
    assert_eq!(out[0].role, "supporting", "default role applied to first");
    assert_eq!(out[0].note, "a");
    assert_eq!(out[1].traffic_id, "t2");
    assert_eq!(out[1].role, "baseline");
}

#[test]
fn snapshot_id_is_deterministic_and_normalized() {
    let mut s = TrafficEvidenceSnapshot {
        source_traffic_id: "src-1".to_string(),
        captured_at: 1_700_000_000,
        url: "https://x/y".to_string(),
        method: "GET".to_string(),
        status: 200,
        content_type: "text/html".to_string(),
        req_head: "GET /y HTTP/1.1\r\n".to_string(),
        resp_head: "HTTP/1.1 200 OK\r\n".to_string(),
        req_hash: "aa".to_string(),
        resp_hash: "bb".to_string(),
        req_len: 14,
        resp_len: 17,
        ..TrafficEvidenceSnapshot::default()
    };
    // A NUL in a head is stripped before hashing; two equal-after-normalize
    // snapshots hash the same.
    s.req_head.push('\0');
    let id1 = traffic_snapshot_id(&s);
    let mut s2 = s.clone();
    s2.req_head = "GET /y HTTP/1.1\r\n".to_string(); // already clean
    let id2 = traffic_snapshot_id(&s2);
    assert_eq!(id1, id2, "NUL stripped before hashing -> same id");
    assert_eq!(id1.len(), 64, "sha256 hex");
    // Distinct content -> distinct id.
    let mut s3 = s2.clone();
    s3.status = 404;
    assert_ne!(traffic_snapshot_id(&s3), id1);
    // Empty heads are omitempty: a snapshot with empty heads hashes without them,
    // so toggling empty<->non-empty changes the id (different JSON).
    let mut e1 = s2.clone();
    e1.req_head = String::new();
    e1.resp_head = String::new();
    let mut e2 = e1.clone();
    e2.req_head = "x".to_string();
    assert_ne!(traffic_snapshot_id(&e1), traffic_snapshot_id(&e2));
}

#[tokio::test]
async fn insert_seals_heads_and_binds_with_version_bump() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let t = d.create_task("ev-prim", "goal", None, 0, 0).await.unwrap();
    let fid = d
        .add_finding(t.id, 0, "rce", "", "critical", "s", "e", "w", &[])
        .await
        .unwrap();

    // Build a prepared snapshot with a computed id.
    let mut snap = TrafficEvidenceSnapshot {
        source_traffic_id: format!("src-{fid}"),
        captured_at: 1_700_000_000,
        url: "https://x/y".to_string(),
        method: "GET".to_string(),
        status: 200,
        content_type: "text/html".to_string(),
        req_head: "GET /y HTTP/1.1\r\nHost: x\r\n".to_string(),
        resp_head: "HTTP/1.1 200 OK\r\n\r\nbody".to_string(),
        req_hash: "aa".to_string(),
        resp_hash: "bb".to_string(),
        req_len: 20,
        resp_len: 25,
        ..TrafficEvidenceSnapshot::default()
    };
    snap.id = traffic_snapshot_id(&snap);
    let sid = snap.id.clone();
    let prepared = vec![PreparedTrafficEvidence {
        reference: TrafficRef {
            traffic_id: "t".to_string(),
            role: "proof".to_string(),
            note: "the note".to_string(),
        },
        snapshot: snap.clone(),
    }];

    let mut client = d.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    d.add_finding_traffic_tx(&tx, fid, &prepared)
        .await
        .expect("add traffic");
    tx.commit().await.unwrap();

    // Head columns sealed at rest; metadata in clear.
    let row = d
        .query_one(
            "SELECT url, method, req_head, resp_head FROM traffic_evidence_snapshots WHERE id=$1",
            &[&sid],
        )
        .await
        .unwrap();
    let url: String = row.get(0);
    let method: String = row.get(1);
    let req_head: String = row.get(2);
    let resp_head: String = row.get(3);
    assert_eq!(url, "https://x/y", "url in clear");
    assert_eq!(method, "GET", "method in clear");
    assert!(
        req_head.starts_with("enc:v1:"),
        "req_head sealed: {req_head}"
    );
    assert!(
        resp_head.starts_with("enc:v1:"),
        "resp_head sealed: {resp_head}"
    );

    // open_snapshot_heads decrypts back to the cleartext.
    let mut opened = TrafficEvidenceSnapshot {
        req_head: req_head.clone(),
        resp_head: resp_head.clone(),
        ..TrafficEvidenceSnapshot::default()
    };
    open_snapshot_heads(&mut opened).expect("open heads");
    assert_eq!(opened.req_head, snap.req_head, "req_head round-trips");
    assert_eq!(opened.resp_head, snap.resp_head, "resp_head round-trips");

    // Binding row exists with role/note/position; evidence_version bumped once.
    let (role, note, position): (String, String, i32) = {
        let r = d
            .query_one(
                "SELECT role, note, position FROM finding_traffic_bindings WHERE finding_id=$1",
                &[&fid],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    };
    assert_eq!(role, "proof");
    assert_eq!(note, "the note");
    assert_eq!(position, 0);
    let version: i64 = d
        .query_one("SELECT evidence_version FROM findings WHERE id=$1", &[&fid])
        .await
        .unwrap()
        .get(0);
    assert_eq!(version, 1, "evidence_version bumped once");

    // Re-inserting the SAME prepared evidence is idempotent (no double-bump).
    let mut client = d.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    d.add_finding_traffic_tx(&tx, fid, &prepared)
        .await
        .expect("re-add");
    tx.commit().await.unwrap();
    let version: i64 = d
        .query_one("SELECT evidence_version FROM findings WHERE id=$1", &[&fid])
        .await
        .unwrap()
        .get(0);
    assert_eq!(version, 1, "idempotent re-insert -> no bump");
    let count: i64 = d
        .query_one(
            "SELECT count(*) FROM finding_traffic_bindings WHERE finding_id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1, "binding not duplicated");

    // Cleanup.
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    let _ = d
        .execute(
            "DELETE FROM traffic_evidence_snapshots WHERE id=$1",
            &[&sid],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn insert_rejects_hash_mismatch() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let t = d
        .create_task("ev-mismatch", "goal", None, 0, 0)
        .await
        .unwrap();
    let fid = d
        .add_finding(t.id, 0, "x", "", "low", "", "", "", &[])
        .await
        .unwrap();

    // id does not match the content hash -> rejected.
    let snap = TrafficEvidenceSnapshot {
        id: "deadbeef".to_string(),
        source_traffic_id: "s".to_string(),
        url: "u".to_string(),
        method: "GET".to_string(),
        ..TrafficEvidenceSnapshot::default()
    };
    let prepared = vec![PreparedTrafficEvidence {
        reference: TrafficRef {
            traffic_id: "t".to_string(),
            role: "supporting".to_string(),
            note: String::new(),
        },
        snapshot: snap,
    }];
    let mut client = d.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    let err = d
        .add_finding_traffic_tx(&tx, fid, &prepared)
        .await
        .expect_err("hash mismatch rejected");
    assert!(
        err.to_string().contains("metadata hash mismatch"),
        "got {err}"
    );
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    d.delete_task(t.id).await.unwrap();
}
