//! Differential tests for the notification + finding-lookup/report-version slice
//! (`RecordNotificationEventTx` / `AddNotificationEvent` +
//! `FindingIDByNodeID` / `SetFindingReportVersionByNodeID` /
//! `PopulateFindingTrafficIDs`). `RecordFindingTx` / `RecordFinding` (the atomic
//! recorder) are a later slice.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::notification::record_notification_event_tx;
use artex::db::DB;
use artex::notify::{Snapshot, EVENT_FINDING_CREATED};

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

#[tokio::test]
async fn add_and_record_notification_events() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    // Defensive: a prior panicked run may have leaked finding_id=99 rows.
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE finding_id=$1",
            &[&99i64],
        )
        .await;

    let snap = Snapshot {
        kind: EVENT_FINDING_CREATED.to_string(),
        finding_id: 42,
        task_id: 7,
        vulnclass: "sqli".to_string(),
        name: "SQLi".to_string(),
        severity: "high".to_string(),
        summary: "a summary".to_string(),
        asset_ids: vec![1, 2],
        from_status: String::new(),
        to_status: String::new(),
    };

    // AddNotificationEvent returns an id and the row reads back the snapshot JSON.
    let id = d
        .add_notification_event(EVENT_FINDING_CREATED, 42, &snap)
        .await
        .expect("add event");
    let (kind, finding_id, body): (String, i64, serde_json::Value) = {
        let r = d
            .query_one(
                "SELECT kind, finding_id, snapshot FROM notification_events WHERE id=$1",
                &[&id],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    };
    assert_eq!(kind, EVENT_FINDING_CREATED);
    assert_eq!(finding_id, 42);
    assert_eq!(body["vulnclass"], "sqli");
    assert_eq!(body["asset_ids"], serde_json::json!([1, 2]));
    assert!(
        body.get("from_status").is_none(),
        "from_status omitted when empty"
    );
    assert!(
        body.get("to_status").is_none(),
        "to_status omitted when empty"
    );

    // record_notification_event_tx writes inside a caller tx and returns true.
    let mut client = d.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    let ok = record_notification_event_tx(&tx, EVENT_FINDING_CREATED, 99, &snap).await;
    tx.commit().await.unwrap();
    assert!(ok, "tx insert succeeded");
    let n: i64 = d
        .query_one(
            "SELECT count(*) FROM notification_events WHERE finding_id=$1",
            &[&99i64],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1, "tx event persisted");

    // status-change snapshot serialises from_status/to_status when set.
    let snap2 = Snapshot {
        kind: "finding_status_changed".to_string(),
        finding_id: 42,
        from_status: "pending".to_string(),
        to_status: "fixed".to_string(),
        ..snap.clone()
    };
    let id2 = d
        .add_notification_event("finding_status_changed", 42, &snap2)
        .await
        .unwrap();
    let body2: serde_json::Value = d
        .query_one(
            "SELECT snapshot FROM notification_events WHERE id=$1",
            &[&id2],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(body2["from_status"], "pending");
    assert_eq!(body2["to_status"], "fixed");

    // Cleanup.
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE id=ANY($1::bigint[])",
            &[&vec![id, id2]],
        )
        .await;
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE finding_id=$1",
            &[&99i64],
        )
        .await;
}

#[tokio::test]
async fn finding_id_by_node_and_report_version() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("report-ver", "goal", None, 0, 0)
        .await
        .unwrap();
    // A finding node + a finding row anchored to it.
    let node_id = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1,'finding','{}'::jsonb,9,'confirmed','tester') RETURNING id",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let fid = d
        .add_finding(t.id, node_id, "rce", "", "critical", "s", "e", "w", &[])
        .await
        .unwrap();
    // Give it some evidence so evidence_version > 0.
    d.execute(
        "UPDATE findings SET evidence_version=2 WHERE id=$1",
        &[&fid],
    )
    .await
    .unwrap();

    // FindingIDByNodeID resolves the row.
    assert_eq!(d.finding_id_by_node_id(node_id).await.unwrap(), fid);
    // Missing node -> "no rows" error.
    let err = d
        .finding_id_by_node_id(99_999_999)
        .await
        .expect_err("missing");
    assert!(err.to_string().contains("no rows"), "got {err}");

    // SetFindingReportVersionByNodeID with version=None: report_evidence_version
    // pins to current (evidence_version=2 -> CASE returns -1 since !=0... wait:
    // evidence_version=2 != 0 so ELSE -1). Assert the documented semantics.
    let n = d
        .set_finding_report_version_by_node_id(node_id, "the report", None)
        .await
        .expect("set report");
    assert_eq!(n, 1, "one row updated");
    let (report, rev): (String, i64) = {
        let r = d
            .query_one(
                "SELECT report, report_evidence_version FROM findings WHERE id=$1",
                &[&fid],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(report, "the report");
    // evidence_version != 0 -> COALESCE(NULL, -1) = -1.
    assert_eq!(rev, -1, "report pinned against changed evidence");

    // With version=Some(2) matching current evidence_version -> passes the guard.
    let n = d
        .set_finding_report_version_by_node_id(node_id, "v2 report", Some(2))
        .await
        .expect("set report with version");
    assert_eq!(n, 1);
    let rev: i64 = d
        .query_one(
            "SELECT report_evidence_version FROM findings WHERE id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(rev, 2, "report_evidence_version pinned to provided version");

    // Stale version -> evidence-conflict error, no change.
    let err = d
        .set_finding_report_version_by_node_id(node_id, "stale", Some(999))
        .await
        .expect_err("stale version rejected");
    assert!(
        err.is(artex::db::finding_evidence::ERR_EVIDENCE_CONFLICT),
        "got {err}"
    );

    // Missing node (no finding) -> 0 rows, Ok.
    let n = d
        .set_finding_report_version_by_node_id(99_999_999, "x", None)
        .await
        .expect("missing node ok");
    assert_eq!(n, 0);

    // Cleanup.
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&node_id])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn populate_finding_traffic_ids_enriches_nodes() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("populate", "goal", None, 0, 0).await.unwrap();
    let node_id = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1,'finding','{}'::jsonb,9,'confirmed','tester') RETURNING id",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let fid = d
        .add_finding(t.id, node_id, "xss", "", "medium", "", "", "", &[])
        .await
        .unwrap();
    // Two traffic bindings so traffic_count=2.
    for sid in ["p-a", "p-b"] {
        d.execute(
            "INSERT INTO traffic_evidence_snapshots(id, source_traffic_id, captured_at, url, method, \
             status, req_head, resp_head, req_hash, resp_hash, req_len, resp_len) \
             VALUES ($1,$1,0,'u','GET',200,'','','','',0,0)",
            &[&sid],
        )
        .await
        .unwrap();
        d.execute(
            "INSERT INTO finding_traffic_bindings(finding_id, snapshot_id, role, position) \
             VALUES ($1,$2,'proof',0)",
            &[&fid, &sid],
        )
        .await
        .unwrap();
    }

    // Build a minimal Node list: one finding node, one non-finding node.
    let mut nodes = vec![
        artex::db::exploration::Node {
            finding_id: 0,
            finding_node_id: 0,
            traffic_count: 0,
            id: node_id,
            kind: "finding".to_string(),
            payload: serde_json::Value::Null,
            priority: 0,
            state: "confirmed".to_string(),
            origin: "tester".to_string(),
            owner: String::new(),
            blocked_reason: String::new(),
            delete_reason: String::new(),
            anchors: Vec::new(),
            created_at: std::time::SystemTime::UNIX_EPOCH,
            source_task_id: 0,
            inherited: false,
        },
        artex::db::exploration::Node {
            finding_id: 0,
            finding_node_id: 0,
            traffic_count: 0,
            id: 99_999_999,
            kind: "fact".to_string(),
            payload: serde_json::Value::Null,
            priority: 0,
            state: "confirmed".to_string(),
            origin: String::new(),
            owner: String::new(),
            blocked_reason: String::new(),
            delete_reason: String::new(),
            anchors: Vec::new(),
            created_at: std::time::SystemTime::UNIX_EPOCH,
            source_task_id: 0,
            inherited: false,
        },
    ];

    d.populate_finding_traffic_ids(&mut nodes)
        .await
        .expect("populate");
    assert_eq!(nodes[0].finding_id, fid, "finding id filled");
    assert_eq!(nodes[0].finding_node_id, node_id);
    assert_eq!(nodes[0].traffic_count, 2, "two bindings");
    // Non-finding node untouched.
    assert_eq!(nodes[1].finding_id, 0);

    // Cleanup.
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&node_id])
        .await;
    let _ = d
        .execute(
            "DELETE FROM traffic_evidence_snapshots WHERE id=ANY($1::text[])",
            &[&vec!["p-a".to_string(), "p-b".to_string()]],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}
