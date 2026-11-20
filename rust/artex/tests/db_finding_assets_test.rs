//! Differential tests for `db/finding_assets.go` — port of
//! `TestBuildFindingAssetTree` (tree shape, ancestor aggregation, unlinked bucket)
//! + asset-scope subtree resolution. Runs against `ARTEX_PG_DSN`.

use artex::db::finding_assets::FindingAssetTree;
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

fn node<'a>(
    tree: &'a FindingAssetTree,
    key: &str,
) -> Option<&'a artex::db::finding_assets::FindingAssetNode> {
    tree.nodes.iter().find(|n| n.key == key)
}

async fn seed_asset(
    d: &artex::db::DB,
    typ: &str,
    domain: &str,
    root: &str,
    url: &str,
    port: i32,
    task_id: i64,
) -> i64 {
    let port_opt = if port == 0 { None } else { Some(port) };
    d.query_one(
        "INSERT INTO assets(type, domain, root_domain, url, port, company_source, task_ids) \
         VALUES ($1,$2,$3,NULLIF($4,''),$5,'explicit',ARRAY[$6::bigint]) RETURNING id",
        &[&typ, &domain, &root, &url, &port_opt, &task_id],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

#[tokio::test]
async fn build_finding_asset_tree_shape_and_scope() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = format!("tree-{tag}.example");
    let sub = format!("api.{root}");
    let _ = d
        .execute("DELETE FROM assets WHERE root_domain=$1", &[&root])
        .await;
    let t = d
        .create_task("asset tree test", "goal", None, 0, 0)
        .await
        .unwrap();
    let _ = d
        .execute("DELETE FROM findings WHERE task_id=$1", &[&t.id])
        .await;

    let root_id = seed_asset(&d, "root_domain", &root, &root, "", 0, t.id).await;
    let sub_id = seed_asset(&d, "subdomain", &sub, &root, "", 0, t.id).await;
    let svc_id = seed_asset(
        &d,
        "service",
        &sub,
        &root,
        &format!("https://{sub}"),
        443,
        t.id,
    )
    .await;
    let ep_id = seed_asset(
        &d,
        "endpoint",
        &sub,
        &root,
        &format!("https://{sub}/admin"),
        443,
        t.id,
    )
    .await;
    // Sibling service with no findings -> hidden.
    seed_asset(
        &d,
        "service",
        &sub,
        &root,
        &format!("http://{sub}:8080"),
        8080,
        t.id,
    )
    .await;

    d.add_finding(
        t.id,
        0,
        "XSS",
        "reflected XSS",
        "high",
        "s",
        "e",
        "w",
        &[ep_id],
    )
    .await
    .unwrap();
    d.add_finding(
        t.id,
        0,
        "Info",
        "information disclosure",
        "low",
        "s",
        "e",
        "w",
        &[svc_id],
    )
    .await
    .unwrap();
    // Orphan asset id -> unlinked bucket.
    d.add_finding(
        t.id,
        0,
        "Misc",
        "orphan",
        "medium",
        "s",
        "e",
        "w",
        &[999_000_111],
    )
    .await
    .unwrap();

    let f = FindingFilter {
        task_id: t.id.to_string(),
        ..FindingFilter::default()
    };
    let tree = d.build_finding_asset_tree(&f).await.unwrap();
    assert_eq!(tree.finding_total, 3);

    let (rk, sk, vk, ek) = (
        format!("a:{root_id}"),
        format!("a:{sub_id}"),
        format!("a:{svc_id}"),
        format!("a:{ep_id}"),
    );
    let (rn, sn, vn, en) = (
        node(&tree, &rk).expect("root"),
        node(&tree, &sk).expect("sub"),
        node(&tree, &vk).expect("svc"),
        node(&tree, &ek).expect("ep"),
    );
    // Parent chain: endpoint -> service -> subdomain -> root_domain.
    assert_eq!(en.parent, vk);
    assert_eq!(vn.parent, sk);
    assert_eq!(sn.parent, rk);
    assert!(rn.parent.is_empty(), "root is top-level");
    // Aggregation (dedup ancestor chain).
    assert_eq!((rn.total, rn.high, rn.low, rn.self_count), (2, 1, 1, 0));
    assert_eq!((vn.total, vn.self_count), (2, 1));
    assert_eq!((en.total, en.self_count), (1, 1));
    // Asset without findings hidden.
    assert!(tree
        .nodes
        .iter()
        .all(|n| n.label != format!("http://{sub}:8080")));
    // Unlinked bucket.
    let none = node(&tree, FINDING_UNASSIGNED_ASSET).expect("unlinked");
    assert_eq!((none.total, none.medium), (1, 1));

    // Asset-scope: selecting the root node narrows to its whole subtree.
    let scoped = d
        .apply_asset_scope(FindingFilter {
            task_id: t.id.to_string(),
            asset_scope: rk.clone(),
            ..FindingFilter::default()
        })
        .await
        .unwrap();
    assert!(!scoped.asset_miss, "root node exists under the filter");
    for id in [root_id, sub_id, svc_id, ep_id] {
        assert!(
            scoped.asset_ids.contains(&id),
            "subtree includes asset {id}"
        );
    }
    assert!(!scoped.asset_ids.contains(&999_000_111));
    let (page, total) = d.list_findings_page(scoped, 1, 20).await.unwrap();
    assert_eq!(total, 2, "root scope selects the 2 attached findings");
    assert_eq!(page.len(), 2);

    // __none__ sentinel -> unlinked bucket only (1 medium finding).
    let scoped = d
        .apply_asset_scope(FindingFilter {
            task_id: t.id.to_string(),
            asset_scope: FINDING_UNASSIGNED_ASSET.to_string(),
            ..FindingFilter::default()
        })
        .await
        .unwrap();
    assert!(scoped.asset_none);
    let (page, total) = d.list_findings_page(scoped, 1, 20).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(page.len(), 1);

    let _ = d
        .execute("DELETE FROM findings WHERE task_id=$1", &[&t.id])
        .await;
    let _ = d
        .execute("DELETE FROM assets WHERE root_domain=$1", &[&root])
        .await;
    d.delete_task(t.id).await.unwrap();
}
