//! Differential tests for the AssetStore foundation — ports of Go
//! `TestUpsertRootDomain`, `TestUpsertRootDomainEmpty`, `TestUpsertIP`,
//! `TestAppendIPPort`, `TestQueryByType`, `TestDeleteByTaskID`.

use artex::db::asset_helpers::PortService;
use artex::db::assets::{AssetStore, UpsertIPReq, UpsertRootDomainReq};
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

fn tag() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

#[tokio::test]
async fn upsert_root_domain_dedup_and_empty() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s: AssetStore = d.assets();
    let t = tag();
    let domain = format!("roottest-{t}.io");

    let err = s
        .upsert_root_domain(&UpsertRootDomainReq {
            domain: String::new(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "domain is required");

    let id1 = s
        .upsert_root_domain(&UpsertRootDomainReq {
            domain: domain.clone(),
            icp: "A12345".into(),
            task_id: 1,
        })
        .await
        .unwrap();
    assert!(id1 > 0);
    let id2 = s
        .upsert_root_domain(&UpsertRootDomainReq {
            domain: domain.clone(),
            task_id: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id2, id1, "dedup failed");

    let row = d
        .query_one("SELECT icp, task_ids FROM assets WHERE id=$1", &[&id1])
        .await
        .unwrap();
    let icp: Option<String> = row.get(0);
    let tasks: Vec<i64> = row.get(1);
    assert_eq!(icp.as_deref(), Some("A12345"));
    assert!(tasks.contains(&1) && tasks.contains(&2));

    assert_eq!(s.delete_by_ids(&[id1]).await.unwrap(), 1);
}

#[tokio::test]
async fn upsert_ip_and_port_merge() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let ip = format!(
        "192.168.{}.{}",
        (t.as_bytes()[0] % 200) as u32 + 1,
        (t.as_bytes()[1] % 200) as u32 + 1
    );

    let id1 = s
        .upsert_ip(&UpsertIPReq {
            ip: ip.clone(),
            open_ports: vec![
                PortService {
                    port: 22,
                    service: "ssh".into(),
                },
                PortService {
                    port: 80,
                    service: "http".into(),
                },
            ],
            task_id: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let cseg: Option<String> = d
        .query_one("SELECT c_segment::text FROM assets WHERE id=$1", &[&id1])
        .await
        .unwrap()
        .get(0);
    assert!(
        cseg.as_deref().unwrap_or("").starts_with("192.168."),
        "c_segment = {cseg:?}"
    );

    let id2 = s
        .upsert_ip(&UpsertIPReq {
            ip: ip.clone(),
            open_ports: vec![PortService {
                port: 443,
                service: "https".into(),
            }],
            task_id: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id2, id1);
    let cnt: i64 = d
        .query_one(
            "SELECT cardinality(open_ports)::bigint FROM assets WHERE id=$1",
            &[&id1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(cnt, 3);

    // Invalid IP (a hostname) is rejected with the asset-ip sentinel.
    let err = s
        .upsert_ip(&UpsertIPReq {
            ip: "example.com".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(err.is(artex::db::asset_helpers::ERR_ASSET_IP_INVALID));

    assert_eq!(s.delete_by_ids(&[id1]).await.unwrap(), 1);
}

#[tokio::test]
async fn append_ip_port() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let ip = format!(
        "10.9.{}.{}",
        (t.as_bytes()[2] % 200) as u32 + 1,
        (t.as_bytes()[3] % 200) as u32 + 1
    );

    let id = s
        .upsert_ip(&UpsertIPReq {
            ip: ip.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    s.append_ip_port(&ip, 3306, "mysql").await.unwrap();
    s.append_ip_port(&ip, 22, "ssh").await.unwrap();
    let cnt: i64 = d
        .query_one(
            "SELECT cardinality(open_ports)::bigint FROM assets WHERE id=$1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(cnt, 2);
    assert_eq!(s.delete_by_ids(&[id]).await.unwrap(), 1);
}

#[tokio::test]
async fn query_by_type_and_get_by_ids() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let domain = format!("querytest-{t}.example.net");
    let id = s
        .upsert_root_domain(&UpsertRootDomainReq {
            domain: domain.clone(),
            ..Default::default()
        })
        .await
        .unwrap();

    let all = s.query_by_type("root_domain", 100000, 0).await.unwrap();
    assert!(
        all.iter().any(|a| a.id == id),
        "QueryByType: inserted asset not found"
    );
    let got = s.get_by_ids(&[id]).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].domain, domain);
    assert_eq!(s.delete_by_ids(&[id]).await.unwrap(), 1);
}

#[tokio::test]
async fn delete_by_task_id_unlinks_shared() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let task_a = 9_000_001;
    let task_b = 9_000_002;
    let solo_domain = format!("solo-del-{t}.test");
    let shared_domain = format!("shared-del-{t}.test");

    let solo = s
        .upsert_root_domain(&UpsertRootDomainReq {
            domain: solo_domain.clone(),
            task_id: task_a,
            ..Default::default()
        })
        .await
        .unwrap();
    let shared = s
        .upsert_root_domain(&UpsertRootDomainReq {
            domain: shared_domain.clone(),
            task_id: task_a,
            ..Default::default()
        })
        .await
        .unwrap();
    s.upsert_root_domain(&UpsertRootDomainReq {
        domain: shared_domain.clone(),
        task_id: task_b,
        ..Default::default()
    })
    .await
    .unwrap();

    let hosts = s.hosts_by_task(task_a).await.unwrap();
    assert!(
        hosts.contains(&solo_domain),
        "HostsByTask missing {solo_domain}: {hosts:?}"
    );
    assert!(
        hosts.contains(&shared_domain),
        "HostsByTask missing {shared_domain}: {hosts:?}"
    );

    let n = s.delete_by_task_id(task_a).await.unwrap();
    assert_eq!(n, 1, "should delete exactly the exclusive asset, got {n}");
    assert!(
        s.get_by_ids(&[solo]).await.unwrap().is_empty(),
        "solo asset should be deleted"
    );
    let kept = s.get_by_ids(&[shared]).await.unwrap();
    assert_eq!(kept.len(), 1, "shared asset should be kept");
    assert!(!kept[0].task_ids.contains(&task_a));
    assert!(kept[0].task_ids.contains(&task_b));

    assert_eq!(s.delete_by_ids(&[shared]).await.unwrap(), 1);
}

#[tokio::test]
async fn upsert_subdomain_side_effects_and_dedup() {
    use artex::db::assets::UpsertSubdomainReq;

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let domain = format!("sub-{t}.subdtest.com");
    let root_domain = "subdtest.com";
    let ip = "1.2.3.4";

    let id = s
        .upsert_subdomain(&UpsertSubdomainReq {
            domain: domain.clone(),
            record_type: "A".into(),
            record_value: vec![ip.into()],
            icp: "B99".into(),
            task_id: 1,
        })
        .await
        .unwrap();
    assert!(id > 0);
    d.execute(
        "DELETE FROM assets WHERE domain=$1 OR domain=$2 OR ip=$3",
        &[&domain, &root_domain, &ip],
    )
    .await
    .unwrap();

    let got = s
        .upsert_subdomain(&UpsertSubdomainReq {
            domain: domain.clone(),
            record_type: "A".into(),
            record_value: vec![ip.into()],
            icp: "B99".into(),
            task_id: 1,
        })
        .await
        .unwrap();
    // The first upsert's side effects were deleted, so a fresh dedup run needs
    // to be checked on its own pair of calls.
    let id2 = s
        .upsert_subdomain(&UpsertSubdomainReq {
            domain: domain.clone(),
            record_type: "A".into(),
            record_value: vec![ip.into()],
            ..Default::default()
        })
        .await
        .unwrap();
    let id3 = s
        .upsert_subdomain(&UpsertSubdomainReq {
            domain: domain.clone(),
            record_type: "A".into(),
            record_value: vec![ip.into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id3, id2, "subdomain dedup failed");

    let row = d
        .query_one("SELECT root_domain FROM assets WHERE id=$1", &[&got])
        .await
        .unwrap();
    let root: Option<String> = row.get(0);
    assert_eq!(root.as_deref(), Some(root_domain));

    let sub: Vec<artex::db::assets::Asset> = s.get_by_ids(&[got]).await.unwrap();
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0].domain, domain);

    d.execute(
        "DELETE FROM assets WHERE domain=$1 OR domain=$2 OR ip=$3",
        &[&domain, &root_domain, &ip],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn upsert_app_dedup_by_bundle_and_name() {
    use artex::db::assets::UpsertAppReq;

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let bundle = format!("com.example.app-{t}");
    let name = format!("UniqueAppNoBundle-{t}");

    let id1 = s
        .upsert_app(&UpsertAppReq {
            name: "MyApp".into(),
            bundle_id: bundle.clone(),
            category: "tools".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let id2 = s
        .upsert_app(&UpsertAppReq {
            name: "MyApp Updated".into(),
            bundle_id: bundle.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id2, id1, "app bundle dedup failed");

    let id3 = s
        .upsert_app(&UpsertAppReq {
            name: name.clone(),
            category: "utility".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let id4 = s
        .upsert_app(&UpsertAppReq {
            name: name.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id4, id3, "app name dedup failed");

    let err = s.upsert_app(&UpsertAppReq::default()).await.unwrap_err();
    assert_eq!(err.to_string(), "app name is required");

    assert_eq!(s.delete_by_ids(&[id1, id3]).await.unwrap(), 2);
}

#[tokio::test]
async fn upsert_http_service_merge_and_auth() {
    use artex::db::assets::UpsertHTTPServiceReq;

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let url = format!("https://www.httpsvc-{}.example.com/", tag());
    let domain = format!(
        "www.httpsvc-{}.example.com",
        url.split("httpsvc-")
            .nth(1)
            .unwrap()
            .split('.')
            .next()
            .unwrap()
    );
    let root_domain = "example.com";

    let id1 = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: url.clone(),
            technologies: vec!["nginx".into(), "vue".into()],
            status_code: Some(200),
            content_length: Some(1024),
            page_title: "Test Site".into(),
            task_id: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(id1 > 0);
    let tech: i64 = d
        .query_one(
            "SELECT array_length(technologies,1)::bigint FROM assets WHERE id=$1",
            &[&id1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(tech, 2);

    let id2 = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: url.clone(),
            technologies: vec!["react".into(), "webpack".into()],
            task_id: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id2, id1, "http service dedup failed");
    let tech: i64 = d
        .query_one(
            "SELECT array_length(technologies,1)::bigint FROM assets WHERE id=$1",
            &[&id1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(tech, 4, "technologies merge");

    // Auth append on a separate service.
    let auth_url = format!("http://authtest-{}.example.org/", tag());
    let a1 = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: auth_url.clone(),
            auth: vec![
                serde_json::json!({"type": "basic", "username": "admin", "password": "pass"}),
            ],
            task_id: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let a2 = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: auth_url.clone(),
            auth: vec![serde_json::json!({"type": "bearer", "token": "tok123"})],
            task_id: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(a2, a1);
    let cnt: i64 = d
        .query_one(
            "SELECT cardinality(auth)::bigint FROM assets WHERE id=$1",
            &[&a1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(cnt, 2, "auth append");

    d.execute(
        "DELETE FROM assets WHERE url=$1 OR url=$2 OR domain=$3 OR domain=$4",
        &[&url, &auth_url, &domain, &root_domain],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn upsert_other_service_dedup_auth_and_side_effect() {
    use artex::db::assets::UpsertOtherServiceReq;

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let ip = format!(
        "172.16.{}.{}",
        (t.as_bytes()[0] % 200) as u32 + 1,
        (t.as_bytes()[1] % 200) as u32 + 1
    );

    let id1 = s
        .upsert_other_service(&UpsertOtherServiceReq {
            ip: ip.clone(),
            port: 22,
            service_name: "SSH".into(),
            task_id: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(id1 > 0);
    let id2 = s
        .upsert_other_service(&UpsertOtherServiceReq {
            ip: ip.clone(),
            port: 22,
            service_name: "ssh".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id2, id1, "other service dedup failed");

    // IP side effect: open_ports got the 22/ssh entry.
    let cnt: i64 = d
        .query_one(
            "SELECT cardinality(open_ports)::bigint FROM assets WHERE type='ip' AND ip=$1",
            &[&ip],
        )
        .await
        .unwrap()
        .get(0);
    assert!(cnt > 0, "side-effect IP open_ports not populated");

    // Auth append on a separate row.
    let auth_ip = format!(
        "10.5.{}.{}",
        (t.as_bytes()[2] % 200) as u32 + 1,
        (t.as_bytes()[3] % 200) as u32 + 1
    );
    let a1 = s
        .upsert_other_service(&UpsertOtherServiceReq {
            ip: auth_ip.clone(),
            port: 3306,
            service_name: "mysql".into(),
            auth: vec![serde_json::json!({"username": "admin", "password": "secret"})],
            ..Default::default()
        })
        .await
        .unwrap();
    let a2 = s
        .upsert_other_service(&UpsertOtherServiceReq {
            ip: auth_ip.clone(),
            port: 3306,
            service_name: "mysql".into(),
            auth: vec![serde_json::json!({"username": "root", "password": "root"})],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(a2, a1);
    let auth_cnt: i64 = d
        .query_one(
            "SELECT cardinality(auth)::bigint FROM assets WHERE id=$1",
            &[&a1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(auth_cnt, 2, "auth append");

    let err = s
        .upsert_other_service(&UpsertOtherServiceReq::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "domain or ip is required");

    d.execute("DELETE FROM assets WHERE ip=$1 OR ip=$2", &[&ip, &auth_ip])
        .await
        .unwrap();
}

#[tokio::test]
async fn upsert_endpoint_dedup_params_and_required_fields() {
    use artex::db::assets::UpsertEndpointReq;

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = tag();
    let url = format!("https://api.eptest-{t}.com/users?id=1");
    let domain = format!("api.eptest-{t}.com");
    let root_domain = format!("eptest-{t}.com");

    let err = s
        .upsert_endpoint(&UpsertEndpointReq {
            method: "GET".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "url is required");
    let err = s
        .upsert_endpoint(&UpsertEndpointReq {
            url: "https://example.com/".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "method is required");

    let id1 = s
        .upsert_endpoint(&UpsertEndpointReq {
            url: url.clone(),
            method: "get".into(),
            params: vec![serde_json::json!({"location": "query", "name": "id", "value": "1"})],
            task_id: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(id1 > 0);
    let id2 = s
        .upsert_endpoint(&UpsertEndpointReq {
            url: url.clone(),
            method: "GET".into(),
            params: vec![
                serde_json::json!({"location": "query", "name": "filter", "value": "active"}),
            ],
            task_id: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(id2, id1, "endpoint dedup failed");
    let cnt: i64 = d
        .query_one(
            "SELECT cardinality(params)::bigint FROM assets WHERE id=$1",
            &[&id1],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(cnt, 2, "params merge");

    d.execute(
        "DELETE FROM assets WHERE url=$1 OR domain=$2 OR domain=$3",
        &[&url, &domain, &root_domain],
    )
    .await
    .unwrap();
}
