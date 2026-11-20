//! Differential tests for `FindingFilter::build_where` + `asset_id_containments`
//! (`FindingFilter.where` / `assetIDContainments` in `db/findings.go`). The
//! asset-tree-backed `applyAssetScope` and the page/group/export queries are later
//! slices; here the WHERE builder is exercised behaviorally against the live table.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::findings::{
    asset_id_containments, FilterParam, FindingFilter, FINDING_UNASSIGNED_TASK,
};

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

/// Run a COUNT(*) under the built WHERE; returns the matched row count.
async fn count(d: &artex::db::DB, f: &FindingFilter) -> i64 {
    let (where_sql, params) = f.build_where();
    let sql =
        format!("SELECT COUNT(*) FROM findings f LEFT JOIN tasks t ON f.task_id = t.id{where_sql}");
    let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
        params.iter().map(|p| p.as_tosql()).collect();
    d.query_one(&sql, &refs).await.unwrap().get::<_, i64>(0)
}

#[test]
fn asset_id_containments_formats_jsonb_arrays() {
    assert_eq!(
        asset_id_containments(&[1, 23, 456]),
        vec!["[1]", "[23]", "[456]"]
    );
    assert!(asset_id_containments(&[]).is_empty());
}

#[test]
fn build_where_empty_filter_has_no_clause() {
    let f = FindingFilter::default();
    let (sql, params) = f.build_where();
    assert_eq!(sql, "", "empty filter -> no WHERE");
    assert!(params.is_empty());
}

#[test]
fn build_where_placeholder_ordering() {
    // Text cols, then task_id, then asset containments, then query — each $n in
    // append order.
    let f = FindingFilter {
        severity: "high".to_string(),
        status: "pending".to_string(),
        report_state: "submitted".to_string(),
        verification_status: "verified".to_string(),
        vulnclass: "sqli".to_string(),
        task_id: "5".to_string(),
        query: "poc".to_string(),
        asset_ids: vec![7, 8],
        ..FindingFilter::default()
    };
    let (sql, params) = f.build_where();
    assert!(sql.starts_with(" WHERE "), "{sql}");
    // Five text cols.
    assert!(sql.contains("f.severity = $1"));
    assert!(sql.contains("f.status = $2"));
    assert!(sql.contains("f.report_state = $3"));
    assert!(sql.contains("f.verification_status = $4"));
    assert!(sql.contains("f.vulnclass = $5"));
    // task_id is the 6th arg.
    assert!(sql.contains("f.task_id = $6"));
    // asset containments are the 7th arg.
    assert!(sql.contains("f.asset_ids @> ANY($7::jsonb[])"));
    // query is the 8th arg (one placeholder reused 5× in the ILIKE chain).
    assert!(sql.contains("ILIKE $8 ESCAPE '\\'"));
    assert_eq!(params.len(), 8);
    assert_eq!(params[0], FilterParam::Text("high".to_string()));
    assert_eq!(params[5], FilterParam::Bigint(5));
    match &params[6] {
        FilterParam::Containments(v) => {
            assert_eq!(*v, vec![serde_json::json!([7]), serde_json::json!([8])]);
        }
        other => panic!("expected containments, got {other:?}"),
    }
    assert_eq!(params[7], FilterParam::Text("%poc%".to_string()));
}

#[test]
fn build_where_escapes_like_wildcards() {
    let f = FindingFilter {
        query: "50%_off\\now".to_string(),
        ..FindingFilter::default()
    };
    let (_sql, params) = f.build_where();
    assert_eq!(
        params[0],
        FilterParam::Text("%50\\%\\_off\\\\now%".to_string()),
        "wildcards escaped"
    );
}

#[tokio::test]
async fn build_where_filters_live_rows() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // Seed a controlled set: one task with findings of varied severity/status.
    let t = d.create_task("filter", "goal", None, 0, 0).await.unwrap();
    let mut ids = Vec::new();
    for (i, (sev, status)) in [
        ("critical", "pending"),
        ("high", "pending"),
        ("high", "resolved"),
        ("low", "ignored"),
    ]
    .iter()
    .enumerate()
    {
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
                "",
                sev,
                "s",
                "e",
                "w",
                &[],
            )
            .await
            .unwrap();
        if *status != "pending" {
            d.execute("UPDATE findings SET status=$2 WHERE id=$1", &[&fid, status])
                .await
                .unwrap();
        }
        ids.push(fid);
    }

    // No filter -> all rows present (count >= our 4).
    let all = count(&d, &FindingFilter::default()).await;
    assert!(all >= 4, "baseline count {all}");

    // severity=high -> exactly our 2 high rows (others unaffected by our seeds).
    let f = FindingFilter {
        severity: "high".to_string(),
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    let n = count(&d, &f).await;
    assert_eq!(n, 2, "two high-severity findings in this task");

    // status=pending within this task -> 2 (critical + first high).
    let f = FindingFilter {
        status: "pending".to_string(),
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    assert_eq!(count(&d, &f).await, 2);

    // asset_miss -> FALSE -> 0.
    let f = FindingFilter {
        asset_miss: true,
        ..FindingFilter::default()
    };
    assert_eq!(count(&d, &f).await, 0);

    // unassigned task sentinel -> only our task's rows excluded; just assert it
    // runs and returns a number (no crash from the NULL branch).
    let f = FindingFilter {
        task_id: FINDING_UNASSIGNED_TASK.to_string(),
        ..FindingFilter::default()
    };
    let _ = count(&d, &f).await;

    // Fuzzy query matches a vulnclass substring.
    let f = FindingFilter {
        query: "vc1".to_string(),
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    let n = count(&d, &f).await;
    assert_eq!(n, 1, "vc1 substring matches one finding");

    // asset_ids containment: bind a real asset id onto one finding, filter by it.
    let asset_id = d
        .query_one(
            "INSERT INTO assets(type, domain, company_source) VALUES ('subdomain', $1, 'explicit') RETURNING id",
            &[&format!("filter-{}.example", t.id)],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    d.execute(
        "UPDATE findings SET asset_ids=$2::jsonb WHERE id=$1",
        &[&ids[0], &serde_json::json!([asset_id])],
    )
    .await
    .unwrap();
    let f = FindingFilter {
        asset_ids: vec![asset_id],
        ..FindingFilter::default()
    };
    let n = count(&d, &f).await;
    assert!(n >= 1, "containment matches the asset-tagged finding");
    // asset_none matches findings with no resolvable assets (at least the other 3).
    let f = FindingFilter {
        asset_none: true,
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    assert_eq!(count(&d, &f).await, 3, "three findings still unlinked");

    // Cleanup.
    let _ = d
        .execute("DELETE FROM findings WHERE id=ANY($1::bigint[])", &[&ids])
        .await;
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await;
    // Remove the finding nodes we created (one per id, but we only kept fids; drop by task exploration).
    let _ = d
        .execute(
            "DELETE FROM exploration_nodes WHERE exploration_id=$1 AND kind='finding'",
            &[&t.exploration_id],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}
