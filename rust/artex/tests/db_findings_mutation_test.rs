//! Differential tests for the finding mutation slice of `db/findings.go`
//! (`DeleteFinding` / `SetFindingReportStates` + `setFindingReportStateTx` /
//! `SetFindingReportByNodeID`). `FindingStats` (archive-dependent) and the
//! filter/list-page/group/export cluster (asset-scope resolver) are later slices.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::finding_evidence::ERR_EVIDENCE_CONFLICT;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<artex::db::DB> {
    match artex::db::DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

/// Seed a task + finding node + finding row; returns (task_id, finding_id, node_id).
async fn seed_finding(d: &artex::db::DB, tag: &str) -> (i64, i64, i64) {
    let t = d.create_task(tag, "goal", None, 0, 0).await.unwrap();
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
    (t.id, fid, node_id)
}

#[tokio::test]
async fn delete_finding_removes_row_and_node() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let (tid, fid, node_id) = seed_finding(&d, "del-finding").await;

    // Delete returns 1 and removes both the findings row and the finding node.
    let n = d.delete_finding(fid).await.expect("delete finding");
    assert_eq!(n, 1, "one finding deleted");

    let fcount: i64 = d
        .query_one("SELECT count(*) FROM findings WHERE id=$1", &[&fid])
        .await
        .unwrap()
        .get(0);
    assert_eq!(fcount, 0, "findings row gone");
    let ncount: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_nodes WHERE id=$1 AND kind='finding'",
            &[&node_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(ncount, 0, "finding node gone");

    // Deleting a missing finding is a no-op returning 0.
    let n = d.delete_finding(99_999_999).await.expect("delete missing");
    assert_eq!(n, 0, "missing finding -> 0");

    d.delete_task(tid).await.unwrap();
}

#[tokio::test]
async fn set_finding_report_states_batch() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let (tid1, fid1, node1) = seed_finding(&d, "batch-1").await;
    let (_tid2, fid2, _node2) = seed_finding(&d, "batch-2").await;

    // First transition pending -> submitted: both found + changed, from 'none'.
    let res = d
        .set_finding_report_states(&[fid1, fid2], "submitted")
        .await
        .expect("batch set");
    assert_eq!(res.len(), 2);
    assert!(res[0].found && res[0].changed, "fid1 changed");
    assert_eq!(res[0].from, "none", "prior report_state NULL -> none");
    assert!(res[1].found && res[1].changed, "fid2 changed");

    // Node payload mirrored.
    let mirrored: serde_json::Value = d
        .query_one(
            "SELECT payload->'report_state' FROM exploration_nodes WHERE id=$1",
            &[&node1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(mirrored, serde_json::json!("submitted"));

    // Re-applying the SAME state: found but NOT changed.
    let res = d
        .set_finding_report_states(&[fid1], "submitted")
        .await
        .expect("batch noop");
    assert!(res[0].found && !res[0].changed, "same state -> unchanged");
    assert_eq!(res[0].from, "submitted");

    // A missing id comes back found=false, others unaffected.
    let res = d
        .set_finding_report_states(&[fid1, 99_999_999], "accepted")
        .await
        .expect("batch mixed");
    assert_eq!(res.len(), 2);
    assert!(res[0].found && res[0].changed, "fid1 -> accepted");
    assert_eq!(res[0].from, "submitted");
    assert!(!res[1].found, "missing id found=false");
    assert_eq!(res[1].id, 99_999_999);

    // Cleanup.
    let _ = d
        .execute(
            "DELETE FROM findings WHERE id=ANY($1::bigint[])",
            &[&vec![fid1, fid2]],
        )
        .await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&node1])
        .await;
    d.delete_task(tid1).await.unwrap();
}

#[tokio::test]
async fn set_finding_report_by_node_id_guarded() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let (tid, fid, node_id) = seed_finding(&d, "report-node").await;
    // Some evidence so evidence_version != 0 -> report_evidence_version pins to -1.
    d.execute(
        "UPDATE findings SET evidence_version=2 WHERE id=$1",
        &[&fid],
    )
    .await
    .unwrap();

    let n = d
        .set_finding_report_by_node_id(node_id, "the report")
        .await
        .expect("set report by node");
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
    assert_eq!(rev, -1, "report pinned against changed evidence");

    // Missing node (no finding) -> 0 rows.
    let n = d
        .set_finding_report_by_node_id(99_999_999, "x")
        .await
        .expect("missing node ok");
    assert_eq!(n, 0);

    // Sanity: the guard rejects a stale explicit version on the versioned setter.
    let err = d
        .set_finding_report_version_by_node_id(node_id, "stale", Some(999))
        .await
        .expect_err("stale rejected");
    assert!(err.is(ERR_EVIDENCE_CONFLICT), "got {err}");

    // Cleanup.
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&node_id])
        .await;
    d.delete_task(tid).await.unwrap();
}
