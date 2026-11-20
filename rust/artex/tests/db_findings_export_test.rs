//! Differential tests for `ListFindingsForExport` + `FindingMetaByNodeID` +
//! `FindingQualityByNodeID` (+ `FindingQuality::ReportStale`) in `db/findings.go`.
//! `FindingStats` (archive-dependent) and `AddFindingFollowUpIntent` (Activity
//! machinery) are later slices.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::findings::{FindingFilter, FindingQuality};

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

#[test]
fn finding_quality_report_stale_logic() {
    // Empty report is never stale, even with mismatched versions.
    let q = FindingQuality {
        report: String::new(),
        evidence_version: 5,
        report_evidence_version: 3,
        ..FindingQuality::default()
    };
    assert!(!q.report_stale(), "empty report -> not stale");
    // Non-empty report with equal versions is current.
    let q = FindingQuality {
        report: "body".to_string(),
        evidence_version: 5,
        report_evidence_version: 5,
        ..FindingQuality::default()
    };
    assert!(!q.report_stale(), "equal versions -> current");
    // Non-empty report with mismatched versions is stale.
    let q = FindingQuality {
        report: "body".to_string(),
        evidence_version: 5,
        report_evidence_version: 3,
        ..FindingQuality::default()
    };
    assert!(q.report_stale(), "mismatched versions -> stale");
}

#[tokio::test]
async fn list_findings_for_export_by_ids_and_filter() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("export", "goal", None, 0, 0).await.unwrap();
    let mut ids = Vec::new();
    for (i, sev) in ["low", "critical", "high"].iter().enumerate() {
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
            .add_finding(
                t.id,
                node_id,
                &format!("vc{i}"),
                "n",
                sev,
                "s",
                "e",
                "w",
                &[],
            )
            .await
            .unwrap();
        ids.push(fid);
    }
    // Attach a report to one finding so export carries it.
    d.execute(
        "UPDATE findings SET report='the report' WHERE id=$1",
        &[&ids[1]],
    )
    .await
    .unwrap();

    // By ids (explicit selection): exactly those, severity desc then time.
    let by_ids = d
        .list_findings_for_export(FindingFilter::default(), &ids)
        .await
        .unwrap();
    assert_eq!(by_ids.len(), 3, "exported exactly the selected ids");
    assert_eq!(by_ids[0].severity, "critical", "severity desc first");
    // The report column rides along.
    let with_report = by_ids.iter().find(|f| f.report == "the report");
    assert!(with_report.is_some(), "export carries the report body");

    // By filter (empty ids): the task filter scopes it; apply_asset_scope is a no-op
    // for empty scope.
    let f = FindingFilter {
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    let by_filter = d.list_findings_for_export(f, &[]).await.unwrap();
    assert_eq!(
        by_filter.len(),
        3,
        "filter path exports the task's findings"
    );

    // A subset of ids exports just that subset (ignores filter).
    let subset = d
        .list_findings_for_export(FindingFilter::default(), &ids[..1])
        .await
        .unwrap();
    assert_eq!(subset.len(), 1, "subset export honors the id set");

    // Cleanup.
    let _ = d
        .execute("DELETE FROM findings WHERE task_id=$1", &[&t.id])
        .await;
    let _ = d
        .execute(
            "DELETE FROM exploration_nodes WHERE exploration_id=$1 AND kind='finding'",
            &[&t.exploration_id],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn finding_meta_and_quality_by_node_id() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("meta", "goal", None, 0, 0).await.unwrap();
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
        .add_finding(
            t.id,
            node_id,
            "rce",
            "the name",
            "critical",
            "sum",
            "ev",
            "wkr",
            &[],
        )
        .await
        .unwrap();
    // Set the report-only fields + a report body + poe level.
    d.execute(
        "UPDATE findings SET report='body', poe_level=3, verification_status='verified', \
         verification_note='vn', counterevidence='ce', confidence_rationale='cr', evidence_version=7, \
         report_evidence_version=4 WHERE id=$1",
        &[&fid],
    )
    .await
    .unwrap();
    // Bind one traffic snapshot so the traffic_count subselect is exercised.
    let snap = "meta-snap";
    d.execute(
        "INSERT INTO traffic_evidence_snapshots(id, source_traffic_id, captured_at, url, method, \
         status, req_head, resp_head, req_hash, resp_hash, req_len, resp_len) \
         VALUES ($1,$1,0,'u','GET',200,'','','','',0,0) \
         ON CONFLICT (id) DO NOTHING",
        &[&snap],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO finding_traffic_bindings(finding_id, snapshot_id, role, position) \
         VALUES ($1,$2,'proof',0) ON CONFLICT DO NOTHING",
        &[&fid, &snap],
    )
    .await
    .unwrap();

    // task_id <= 0 -> empty maps.
    assert!(d.finding_meta_by_node_id(0).await.unwrap().is_empty());
    assert!(d.finding_quality_by_node_id(-1).await.unwrap().is_empty());

    let meta = d.finding_meta_by_node_id(t.id).await.unwrap();
    assert_eq!(meta.len(), 1);
    let m = meta.get(&node_id).expect("node present");
    assert_eq!(m.id, fid);
    assert_eq!(m.status, "pending", "COALESCE(status,'pending')");
    assert_eq!(m.traffic_count, 1, "one bound snapshot");

    let qual = d.finding_quality_by_node_id(t.id).await.unwrap();
    assert_eq!(qual.len(), 1);
    let q = qual.get(&node_id).expect("node present");
    assert_eq!(q.meta.id, fid);
    assert_eq!(q.poe_level, 3);
    assert_eq!(q.verification_status, "verified");
    assert_eq!(q.verification_note, "vn");
    assert_eq!(q.counterevidence, "ce");
    assert_eq!(q.confidence_rationale, "cr");
    assert_eq!(q.report, "body");
    assert_eq!(q.evidence_version, 7);
    assert_eq!(q.report_evidence_version, 4);
    assert!(q.report_stale(), "7 != 4 -> stale");

    // Cleanup (snapshots have no FK; remove the binding then the orphan).
    let _ = d
        .execute(
            "DELETE FROM finding_traffic_bindings WHERE finding_id=$1",
            &[&fid],
        )
        .await;
    let _ = d
        .execute(
            "DELETE FROM traffic_evidence_snapshots WHERE id=$1",
            &[&snap],
        )
        .await;
    let _ = d
        .execute("DELETE FROM findings WHERE task_id=$1", &[&t.id])
        .await;
    let _ = d
        .execute(
            "DELETE FROM exploration_nodes WHERE exploration_id=$1 AND kind='finding'",
            &[&t.exploration_id],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}
