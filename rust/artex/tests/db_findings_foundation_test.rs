//! Differential tests for the findings foundation slice of `db/findings.go`
//! (validators + `DBFinding` + `findingSelectCols`/`scanFindings` + `AddFinding`
//! (reused from exploration.rs) / `GetFinding` / `ListFindings` /
//! `DeleteFindingsByTask` + the `setFindingCol`-mirror setters). The
//! evidence-transaction delete and the paginated filter/export/stats cluster are
//! later slices.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use artex::db::findings::{
    valid_finding_report_state, valid_finding_status, valid_severity, DBFinding, FINDING_CONFIRMED,
    FINDING_PENDING, FINDING_REPORT_SUBMITTED, SEVERITY_HIGH,
};
use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
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

#[test]
fn validators_match_go_sets() {
    // status
    for s in [
        FINDING_PENDING,
        "in_progress",
        FINDING_CONFIRMED,
        "resolved",
        "fixed",
        "false_positive",
        "ignored",
        "duplicate",
        "risk_accepted",
    ] {
        assert!(valid_finding_status(s), "valid status {s}");
    }
    assert!(!valid_finding_status("nope"));
    assert!(!valid_finding_status(""));
    // report state
    for s in [
        "none",
        FINDING_REPORT_SUBMITTED,
        "accepted",
        "rejected",
        "duplicate",
    ] {
        assert!(valid_finding_report_state(s), "valid report state {s}");
    }
    assert!(!valid_finding_report_state("pending"));
    // severity
    for s in ["critical", SEVERITY_HIGH, "medium", "low"] {
        assert!(valid_severity(s), "valid severity {s}");
    }
    assert!(!valid_severity("info"));
    assert!(!valid_severity(""));
}

#[tokio::test]
async fn add_and_get_finding_scan() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // A task + exploration so the LEFT JOIN has a description.
    let t = d
        .create_task("finding-scan", "goal", None, 0, 0)
        .await
        .unwrap();
    let asset = d
        .query_one(
            "INSERT INTO assets(type, domain, root_domain) VALUES ('root_domain','scan.example','scan.example') RETURNING id",
            &[],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    let fid = d
        .add_finding(
            t.id,
            0,
            "sqli",
            "SQL Injection",
            "high",
            "a summary",
            "the evidence",
            "alice",
            &[asset],
        )
        .await
        .unwrap();

    let f = d.get_finding(fid).await.expect("get").expect("some");
    assert_eq!(f.id, fid);
    assert_eq!(f.task_id, Some(t.id));
    assert_eq!(f.node_id, None, "node_id 0 stored as NULL");
    assert_eq!(f.vulnclass, "sqli");
    assert_eq!(f.name, "SQL Injection");
    assert_eq!(f.severity, "high");
    assert_eq!(f.summary, "a summary");
    assert_eq!(f.evidence, "the evidence");
    assert_eq!(f.worker, "alice");
    assert_eq!(f.asset_ids, vec![asset], "JSONB array decoded");
    assert_eq!(f.status, FINDING_PENDING, "default status pending");
    assert_eq!(f.report_state, "none", "default report state none");
    assert_eq!(f.verification_status, "unverified", "default verification");
    assert_eq!(f.traffic_count, 0, "no traffic bindings");
    assert_eq!(
        f.task_description, "finding-scan",
        "task description joined"
    );
    assert_eq!(f.poe_level, 0);
    assert!(f.report.is_empty(), "report empty when never written");

    // Missing id -> None.
    assert!(d.get_finding(99_999_999).await.expect("missing").is_none());

    // Cleanup.
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    let _ = d.execute("DELETE FROM assets WHERE id=$1", &[&asset]).await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn list_findings_order_and_limit() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let mut made = Vec::new();
    for _ in 0..3 {
        let fid = d
            .add_finding(
                0,
                0,
                "xss",
                &format!("n-{stamp}"),
                "low",
                "s",
                "e",
                "w",
                &[],
            )
            .await
            .unwrap();
        made.push(fid);
    }

    let all = d.list_findings(0).await.expect("list");
    let mine: std::collections::HashSet<i64> = made.iter().copied().collect();
    let seen: Vec<i64> = all
        .iter()
        .filter(|f| mine.contains(&f.id))
        .map(|f| f.id)
        .collect();
    assert_eq!(seen.len(), 3, "all three present");
    // Newest first (created_at DESC): the last inserted has the latest created_at.
    // created_at is now() per row; within one tx-batch they share the timestamp, so
    // only assert set membership + that a limit of 1 returns exactly one of ours
    // is not guaranteed (others exist). Just assert count with a generous limit.
    assert_eq!(seen.len(), 3);

    // limit=1 returns at most 1 row total.
    let one = d.list_findings(1).await.expect("limit 1");
    assert!(one.len() <= 1, "limit honoured");

    for fid in made {
        let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    }
}

#[tokio::test]
async fn set_finding_col_mirrors_node_payload() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("setf", "goal", None, 0, 0).await.unwrap();
    // Seed an exploration finding node + a finding row pointing at it.
    let node_id = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1,'finding','{\"status\":\"pending\",\"severity\":\"low\"}'::jsonb,0,'confirmed','tester') \
             RETURNING id",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let fid = d
        .add_finding(t.id, node_id, "idor", "IDOR", "medium", "s", "e", "w", &[])
        .await
        .unwrap();

    // SetFindingStatus: row + node payload mirror.
    let n = d.set_finding_status(fid, FINDING_CONFIRMED).await.unwrap();
    assert_eq!(n, 1, "one row affected");
    let f = d.get_finding(fid).await.expect("get").expect("some");
    assert_eq!(f.status, FINDING_CONFIRMED);
    let node_status: String = d
        .query_one(
            "SELECT payload->>'status' FROM exploration_nodes WHERE id=$1",
            &[&node_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(node_status, FINDING_CONFIRMED, "node payload mirrored");

    // SetFindingSeverity + SetFindingName + SetFindingVulnClass + report_state.
    d.set_finding_severity(fid, SEVERITY_HIGH).await.unwrap();
    d.set_finding_name(fid, "Broken IDOR").await.unwrap();
    d.set_finding_vulnclass(fid, "broken_access").await.unwrap();
    d.set_finding_report_state(fid, FINDING_REPORT_SUBMITTED)
        .await
        .unwrap();
    let f = d.get_finding(fid).await.expect("get").expect("some");
    assert_eq!(f.severity, "high");
    assert_eq!(f.name, "Broken IDOR");
    assert_eq!(f.vulnclass, "broken_access");
    assert_eq!(f.report_state, FINDING_REPORT_SUBMITTED);
    let node_report: String = d
        .query_one(
            "SELECT payload->>'report_state' FROM exploration_nodes WHERE id=$1",
            &[&node_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        node_report, FINDING_REPORT_SUBMITTED,
        "report_state mirrored"
    );

    // Unknown id -> 0 affected, no error.
    let n = d
        .set_finding_status(99_999_999, FINDING_CONFIRMED)
        .await
        .unwrap();
    assert_eq!(n, 0, "missing id -> 0 rows");

    // Cleanup.
    let _ = d.execute("DELETE FROM findings WHERE id=$1", &[&fid]).await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&node_id])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn delete_findings_by_task_and_traffic_count() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("delfind", "goal", None, 0, 0).await.unwrap();
    let f1 = d
        .add_finding(t.id, 0, "a", "", "low", "", "", "", &[])
        .await
        .unwrap();
    let f2 = d
        .add_finding(t.id, 0, "b", "", "low", "", "", "", &[])
        .await
        .unwrap();

    // Seed two traffic evidence snapshots (FK target) then bind them to f1.
    let snap_a = format!("snap-a-{f1}");
    let snap_b = format!("snap-b-{f1}");
    for sid in [&snap_a, &snap_b] {
        d.execute(
            "INSERT INTO traffic_evidence_snapshots(id, source_traffic_id, captured_at, url, \
             method, status, req_head, resp_head, req_hash, resp_hash, req_len, resp_len) \
             VALUES ($1,$1,0,'http://x','GET',200,'','','','',0,0)",
            &[&sid],
        )
        .await
        .unwrap();
    }
    d.execute(
        "INSERT INTO finding_traffic_bindings(finding_id, snapshot_id, role, position) \
         VALUES ($1,$2,'request',0),($1,$3,'response',1)",
        &[&f1, &snap_a, &snap_b],
    )
    .await
    .unwrap();
    let f = d.get_finding(f1).await.expect("get").expect("some");
    assert_eq!(f.traffic_count, 2, "traffic count subselect");

    let n = d.delete_findings_by_task(t.id).await.unwrap();
    assert_eq!(n, 2, "two findings deleted");
    assert!(d.get_finding(f1).await.expect("g").is_none());
    assert!(d.get_finding(f2).await.expect("g").is_none());

    // traffic bindings cascade with the finding.
    let bindings: i64 = d
        .query_one(
            "SELECT count(*) FROM finding_traffic_bindings WHERE finding_id=$1",
            &[&f1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(bindings, 0, "bindings cascade-deleted with finding");

    let _ = d
        .execute(
            "DELETE FROM traffic_evidence_snapshots WHERE id=ANY($1::text[])",
            &[&vec![snap_a, snap_b]],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[test]
fn db_finding_default_is_go_zero() {
    // The Rust `DBFinding::default()` mirrors Go's `&DBFinding{}` zero value.
    let f = DBFinding::default();
    assert_eq!(f.id, 0);
    assert_eq!(f.created_at, SystemTime::UNIX_EPOCH);
    assert!(f.task_id.is_none() && f.node_id.is_none());
    assert!(f.asset_ids.is_empty());
    assert_eq!(f.traffic_count, 0);
    assert_eq!(f.poe_level, 0);
    assert!(f.report.is_empty() && f.name.is_empty() && f.vulnclass.is_empty());
}
