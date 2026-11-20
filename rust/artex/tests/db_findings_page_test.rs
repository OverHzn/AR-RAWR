//! Differential tests for `ListFindingsPage` + `ListFindingGroups` (+ the partial
//! `applyAssetScope`) in `db/findings.go`. The empty/`__none__` scopes work without
//! the asset tree; the node-key scope path is deferred (returns an explicit error).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::findings::{FindingFilter, FINDING_UNASSIGNED_ASSET};

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

async fn seed_finding(
    d: &artex::db::DB,
    task_id: i64,
    exploration_id: i64,
    vulnclass: &str,
    sev: &str,
) -> i64 {
    let node_id = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1,'finding','{}'::jsonb,9,'confirmed','tester') RETURNING id",
            &[&exploration_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    d.add_finding(task_id, node_id, vulnclass, "", sev, "s", "e", "w", &[])
        .await
        .unwrap()
}

#[tokio::test]
async fn list_findings_page_filters_sorts_paginates() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("page", "goal", None, 0, 0).await.unwrap();
    // critical, high, high, low — same task.
    for (vc, sev) in [
        ("a", "critical"),
        ("b", "high"),
        ("c", "high"),
        ("e", "low"),
    ] {
        seed_finding(&d, t.id, t.exploration_id, vc, sev).await;
    }
    let task_filter = FindingFilter {
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };

    // Page 1, size 2, time sort (newest first). total=4.
    let (page1, total) = d
        .list_findings_page(task_filter.clone(), 1, 2)
        .await
        .unwrap();
    assert_eq!(total, 4, "four findings in this task");
    assert_eq!(page1.len(), 2, "page size 2");
    // Page 2 size 2 -> 2 rows, page 3 -> empty (beyond count: total_pages=2).
    let (page2, total2) = d
        .list_findings_page(task_filter.clone(), 2, 2)
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(total2, 4, "total independent of page");
    let (page3, _) = d
        .list_findings_page(task_filter.clone(), 3, 2)
        .await
        .unwrap();
    assert!(page3.is_empty(), "page beyond count -> empty");

    // Severity sort: first row must be the critical one.
    let sev_filter = FindingFilter {
        task_id: t.id.to_string(),
        sort: "severity".to_string(),
        ..FindingFilter::default()
    };
    let (spage, _) = d.list_findings_page(sev_filter, 1, 10).await.unwrap();
    assert_eq!(spage.len(), 4);
    assert_eq!(spage[0].severity, "critical", "critical sorts first");
    assert_eq!(spage[3].severity, "low", "low sorts last");

    // page <= 0 defaults to 1; page_size <= 0 defaults to 20.
    let (dpage, dtot) = d
        .list_findings_page(task_filter.clone(), 0, 0)
        .await
        .unwrap();
    assert_eq!(dtot, 4);
    assert_eq!(dpage.len(), 4, "default page size 20 holds all 4");

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
async fn list_finding_groups_buckets_and_counts() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("grp", "goal", None, 0, 0).await.unwrap();
    for (vc, sev) in [
        ("a", "critical"),
        ("b", "high"),
        ("c", "medium"),
        ("e", "low"),
    ] {
        seed_finding(&d, t.id, t.exploration_id, vc, sev).await;
    }

    // Filter to this task so the group is exactly ours.
    let f = FindingFilter {
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    let (groups, group_total, finding_total) = d.list_finding_groups(f, 1, 10).await.unwrap();
    assert_eq!(group_total, 1, "one task group for this filter");
    assert_eq!(finding_total, 4, "four findings total");
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.task_id, Some(t.id));
    assert_eq!(g.count, 4);
    assert_eq!(g.critical, 1);
    assert_eq!(g.high, 1);
    assert_eq!(g.medium, 1);
    assert_eq!(g.low, 1);

    // An unassigned-task bucket exists globally (task_id None). Just assert the
    // query runs and the sentinel filter is coherent.
    let f = FindingFilter {
        task_id: artex::db::findings::FINDING_UNASSIGNED_TASK.to_string(),
        ..FindingFilter::default()
    };
    let (_groups, _gt, _ft) = d.list_finding_groups(f, 1, 10).await.unwrap();

    // page_size <= 0 defaults to 10; a huge page is empty.
    let (empty, gt2, _) = d
        .list_finding_groups(
            FindingFilter {
                task_id: t.id.to_string(),
                ..FindingFilter::default()
            },
            99,
            10,
        )
        .await
        .unwrap();
    assert!(empty.is_empty(), "page beyond group count -> empty");
    assert_eq!(gt2, 1, "group total independent of page");

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
async fn apply_asset_scope_none_and_missing_node() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("scope", "goal", None, 0, 0).await.unwrap();
    seed_finding(&d, t.id, t.exploration_id, "a", "high").await;

    // __none__ scope -> page returns only unlinked findings. Our finding has no
    // assets, so it matches.
    let f = FindingFilter {
        task_id: t.id.to_string(),
        asset_scope: FINDING_UNASSIGNED_ASSET.to_string(),
        ..FindingFilter::default()
    };
    let (rows, total) = d.list_findings_page(f, 1, 10).await.unwrap();
    assert_eq!(total, 1, "unlinked bucket matches the asset-less finding");
    assert_eq!(rows.len(), 1);

    // A node-key scope (not __none__/empty) resolves via the asset tree. A key
    // that no longer exists under the filter yields no rows (asset_miss), NOT an
    // error and NOT an unfiltered result.
    let f = FindingFilter {
        asset_scope: "r:example-does-not-exist".to_string(),
        ..FindingFilter::default()
    };
    let (rows, total) = d
        .list_findings_page(f, 1, 10)
        .await
        .expect("node-key scope resolves");
    assert_eq!(total, 0, "missing node -> empty result");
    assert!(rows.is_empty());

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
