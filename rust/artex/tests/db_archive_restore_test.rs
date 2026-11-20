//! Differential tests for archived asset restore — ports of
//! `db/task_archives_restore_url_test.go` (4 cases).

use artex::db::assets::{UpsertEndpointReq, UpsertHTTPServiceReq};
use artex::db::DB;
use serde_json::{json, Value as Json};

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

async fn row_json(d: &DB, id: i64) -> Json {
    let r = d
        .query_one(
            "SELECT row_to_json(a)::jsonb FROM assets a WHERE id=$1",
            &[&id],
        )
        .await
        .unwrap();
    r.get::<_, Json>(0)
}

fn clean(rows: &[i64]) -> Vec<i64> {
    rows.to_vec()
}

#[tokio::test]
async fn restore_strips_url_userinfo() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let host = format!("restore-{t}.example.test");
    let task = 880_000_001;
    let svc_url = format!("http://{host}:8080/admin");
    let ep_url = format!("https://api.{host}/users?id=1");
    let svc = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: svc_url.clone(),
            task_id: task,
            ..Default::default()
        })
        .await
        .unwrap();
    let ep = s
        .upsert_endpoint(&UpsertEndpointReq {
            url: ep_url.clone(),
            method: "GET".into(),
            task_id: task,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut svc_row = row_json(&d, svc).await;
    let mut ep_row = row_json(&d, ep).await;
    svc_row["url"] = json!(format!("http://alice:s3cr3t-one@{host}:8080/admin"));
    ep_row["url"] = json!(format!("https://bob:s3cr3t-two@api.{host}/users?id=1"));
    let archived_fields = {
        let mut m = std::collections::HashMap::new();
        for (typ, row) in [("service", &svc_row), ("endpoint", &ep_row)] {
            let keys = ["domain", "root_domain", "port", "service_name", "method"];
            let v: serde_json::Map<String, Json> = keys
                .iter()
                .filter_map(|k| row.get(*k).map(|x| (k.to_string(), x.clone())))
                .collect();
            m.insert(typ.to_string(), Json::Object(v));
        }
        m
    };

    d.execute(
        "DELETE FROM assets WHERE id = ANY($1)",
        &[&clean(&[svc, ep])],
    )
    .await
    .unwrap();
    let raw = json!([svc_row, ep_row]);
    let (mapping, _) = d.restore_archive_assets(task, &raw).await.unwrap();

    for (old, want_url) in [(svc, &svc_url), (ep, &ep_url)] {
        let new_id = *mapping.get(&old).expect("restored id");
        let r = d.query_one(
            "SELECT url, domain, root_domain, port, service_name, method FROM assets WHERE id=$1", &[&new_id],
        ).await.unwrap();
        let got_url: String = r.get(0);
        assert_eq!(got_url, *want_url);
        assert!(
            !got_url.contains("s3cr3t"),
            "credential survived restore: {got_url}"
        );
        // Check the non-URL columns kept their archived values.
        let r = d
            .query_one(
                "SELECT row_to_json(a)::jsonb FROM assets a WHERE id=$1",
                &[&new_id],
            )
            .await
            .unwrap();
        let got: Json = r.get(0);
        let typ = got["type"].as_str().unwrap().to_string();
        let want = &archived_fields[&typ];
        for (k, v) in want.as_object().unwrap() {
            assert_eq!(&got[k], v, "{typ} {k} changed by restore");
        }
    }

    d.execute(
        "DELETE FROM assets WHERE url=$1 OR url=$2",
        &[&svc_url, &ep_url],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn restore_reuses_clean_url_row() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let host = format!("reuse-{t}.example.test");
    let live_task = 880_000_002;
    let archived_task = 880_000_003;
    let svc_url = format!("http://{host}:8080/admin");
    let ep_url = format!("https://api.{host}/users?id=1");
    let svc = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: svc_url.clone(),
            task_id: live_task,
            ..Default::default()
        })
        .await
        .unwrap();
    let ep = s
        .upsert_endpoint(&UpsertEndpointReq {
            url: ep_url.clone(),
            method: "GET".into(),
            task_id: live_task,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut svc_row = row_json(&d, svc).await;
    let mut ep_row = row_json(&d, ep).await;
    svc_row["id"] = json!(990_000_101);
    svc_row["url"] = json!(format!("http://alice:s3cr3t-one@{host}:8080/admin"));
    svc_row["task_ids"] = json!([archived_task]);
    ep_row["id"] = json!(990_000_102);
    ep_row["url"] = json!(format!("https://bob:s3cr3t-two@api.{host}/users?id=1"));
    ep_row["task_ids"] = json!([archived_task]);

    let (mapping, _) = d
        .restore_archive_assets(archived_task, &json!([svc_row, ep_row]))
        .await
        .unwrap();
    assert_eq!(mapping.get(&990_000_101), Some(&svc));
    assert_eq!(mapping.get(&990_000_102), Some(&ep));
    for id in [svc, ep] {
        let r = d
            .query_one("SELECT task_ids FROM assets WHERE id=$1", &[&id])
            .await
            .unwrap();
        let tasks: Vec<i64> = r.get(0);
        assert!(
            tasks.contains(&live_task) && tasks.contains(&archived_task),
            "task_ids={tasks:?}"
        );
        let r = d
            .query_one("SELECT url FROM assets WHERE id=$1", &[&id])
            .await
            .unwrap();
        let url: String = r.get(0);
        assert!(!url.contains("s3cr3t"));
        let legacy: i64 = d
            .query_one(
                "SELECT count(*)::bigint FROM assets WHERE id=$1",
                &[&if id == svc { 990_000_101 } else { 990_000_102 }],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(legacy, 0);
    }
    d.execute(
        "DELETE FROM assets WHERE url=$1 OR url=$2",
        &[&svc_url, &ep_url],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn restore_dedups_url_across_credentials() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let host = format!("dedup-{t}.example.test");
    let task = 880_000_004;
    let url = format!("http://{host}:8080/admin");
    let svc = s
        .upsert_http_service(&UpsertHTTPServiceReq {
            url: url.clone(),
            task_id: task,
            ..Default::default()
        })
        .await
        .unwrap();
    let base = row_json(&d, svc).await;
    d.execute("DELETE FROM assets WHERE id=$1", &[&svc])
        .await
        .unwrap();

    let mut a = base.clone();
    let mut b = base;
    a["id"] = json!(990_000_201);
    a["url"] = json!(format!("http://alice:s3cr3t-one@{host}:8080/admin"));
    b["id"] = json!(990_000_202);
    b["url"] = json!(format!("http://bob:s3cr3t-two@{host}:8080/admin"));
    let (mapping, _) = d
        .restore_archive_assets(task, &json!([a, b]))
        .await
        .unwrap();
    let id = mapping.get(&990_000_201).copied().unwrap_or(0);
    assert!(id > 0);
    assert_eq!(
        mapping.get(&990_000_202),
        Some(&id),
        "credential variants restored separately: {mapping:?}"
    );
    let n: i64 = d
        .query_one("SELECT count(*)::bigint FROM assets WHERE url=$1", &[&url])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    let r = d
        .query_one("SELECT url FROM assets WHERE id=$1", &[&id])
        .await
        .unwrap();
    assert!(!r.get::<_, String>(0).contains("s3cr3t"));
    d.execute("DELETE FROM assets WHERE url=$1", &[&url])
        .await
        .unwrap();
}

#[tokio::test]
async fn restore_leaves_domain_and_ip_urls_untouched() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let s = d.assets();
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let host = format!("urlscope-{t}.example.test");
    let ip = "198.51.100.231";
    let task = 880_000_005;
    let domain_id = s
        .upsert_root_domain(&artex::db::assets::UpsertRootDomainReq {
            domain: host.clone(),
            task_id: task,
            ..Default::default()
        })
        .await
        .unwrap();
    let ip_id = s
        .upsert_ip(&artex::db::assets::UpsertIPReq {
            ip: ip.into(),
            task_id: task,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut a = row_json(&d, domain_id).await;
    let mut b = row_json(&d, ip_id).await;
    let odd = "HTTP://Restore-Userinfo.example/";
    a["url"] = json!(odd);
    b["url"] = json!(odd);
    let want_a = [
        a["type"].clone(),
        a["domain"].clone(),
        a["root_domain"].clone(),
        a["ip"].clone(),
    ];
    let want_b = [
        b["type"].clone(),
        b["domain"].clone(),
        b["root_domain"].clone(),
        b["ip"].clone(),
    ];

    d.execute(
        "DELETE FROM assets WHERE id = ANY($1)",
        &[&clean(&[domain_id, ip_id])],
    )
    .await
    .unwrap();
    let (mapping, _) = d
        .restore_archive_assets(task, &json!([a, b]))
        .await
        .unwrap();
    for (old, want) in [(domain_id, want_a), (ip_id, want_b)] {
        let new_id = *mapping.get(&old).expect("restored id");
        let r = d
            .query_one(
                "SELECT row_to_json(a)::jsonb FROM assets a WHERE id=$1",
                &[&new_id],
            )
            .await
            .unwrap();
        let got: Json = r.get(0);
        assert_eq!(got["url"], json!(odd), "non-service URL was normalized");
        for (i, key) in ["type", "domain", "root_domain", "ip"].iter().enumerate() {
            assert_eq!(got[*key], want[i], "{key} changed by restore");
        }
    }
    d.execute("DELETE FROM assets WHERE url=$1", &[&odd.to_string()])
        .await
        .unwrap();
}

#[test]
fn remap_archive_asset_references_cases() {
    use artex::db::archive_helpers::remap_archive_asset_references;
    use std::collections::{BTreeMap, HashMap};

    let mut tables = BTreeMap::new();
    tables.insert(
        "exploration_anchors".to_string(),
        json!([{"asset_id": 11}, {"asset_id": 99}]),
    );
    tables.insert("task_asset_links".to_string(), json!([{"asset_id": 22}]));
    tables.insert(
        "exploration_nodes".to_string(),
        json!([{
            "payload": {"asset_ids": [11, 22, 99]}
        }]),
    );
    tables.insert("findings".to_string(), json!([{"asset_ids": [22, 99, 11]}]));
    tables.insert("other".to_string(), json!([{"asset_id": 11}]));
    let mapping: HashMap<i64, i64> = [(11, 101), (22, 202)].into_iter().collect();

    let out = remap_archive_asset_references(&tables, &mapping);
    assert_eq!(
        out["exploration_anchors"],
        json!([{"asset_id": 101}, {"asset_id": 99}])
    );
    assert_eq!(out["task_asset_links"], json!([{"asset_id": 202}]));
    assert_eq!(
        out["exploration_nodes"][0]["payload"]["asset_ids"],
        json!([101, 202, 99])
    );
    assert_eq!(out["findings"][0]["asset_ids"], json!([202, 99, 101]));
    assert_eq!(out["other"], tables["other"], "unrelated tables untouched");
}

#[tokio::test]
async fn insert_archive_rows_allowlist_and_empty() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    d.insert_archive_rows("findings", &json!([])).await.unwrap();
    let err = d
        .insert_archive_rows("assets", &json!([]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not allowed"), "got {err}");
}

#[tokio::test]
async fn restore_intercept_rows_normalizes_legacy() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let task = format!("restore-legacy-{t}");
    let rows = json!([{
        "task_id": task,
        "agent_name": "agent",
        "tool_name": "Bash",
        "tool_input": {"command":"fixture"},
        "status": "pending",
        "reason": "[model] fixture decision",
        "decision_source": "",
        "decided_by": null,
        "audit": {"execution_status": "awaiting_result"}
    }]);
    d.restore_intercept_rows(&rows).await.unwrap();

    let r = d.query_one(
        "SELECT status, decision_source, decided_by, reason, audit->>'execution_status' FROM intercept_pending WHERE task_id=$1",
        &[&task],
    ).await.unwrap();
    let status: String = r.get(0);
    let source: String = r.get(1);
    let actor: String = r.get(2);
    let reason: String = r.get(3);
    let exec: String = r.get(4);
    assert_eq!(status, "timeout");
    assert_eq!(source, "model");
    assert_eq!(actor, "system");
    assert_eq!(reason, "approval timed out while the task was archived");
    assert_eq!(exec, "not_executed");

    d.execute("DELETE FROM intercept_pending WHERE task_id=$1", &[&task])
        .await
        .unwrap();
}

#[tokio::test]
async fn restore_task_relation_scope_profile_warnings() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let task = d
        .create_task(&format!("restore-rel-{t}"), "goal", None, 0, 0)
        .await
        .unwrap();

    let warn = d
        .restore_task_relations(task.id, &json!([{"source_task_id": 99_999_999}]))
        .await
        .unwrap();
    assert_eq!(warn.len(), 1);
    assert!(warn[0].contains("unavailable"));

    let warn = d.restore_task_scopes(&json!([{"task_id": task.id, "kind": "keyword", "value": "x", "company_id": 99_999_999}]))
        .await.unwrap();
    assert_eq!(warn.len(), 1);
    assert!(warn[0].contains("deleted"));

    let warn = d
        .restore_task_llm_profiles(
            &json!([{"task_id": task.id, "profile_id": 99_999_999, "status": "active"}]),
        )
        .await
        .unwrap();
    assert_eq!(warn.len(), 1);
    assert!(warn[0].contains("LLM profile"));

    let _ = d.delete_task(task.id).await;
}

#[tokio::test]
async fn restore_task_archive_validation_and_happy_path() {
    use artex::db::task_archives_restore::{
        TaskArchiveSnapshot, ERR_TASK_ARCHIVE_FORMAT_MISMATCH, ERR_TASK_ARCHIVE_STATE, RESTORING,
    };

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let t = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let task = d
        .create_task(&format!("full-restore-{t}"), "goal", None, 0, 0)
        .await
        .unwrap();

    let task_row: Json = d
        .query_one(
            "SELECT row_to_json(a)::jsonb FROM tasks a WHERE id=$1",
            &[&task.id],
        )
        .await
        .unwrap()
        .get(0);
    let exp_row: Json = d
        .query_one(
            "SELECT row_to_json(a)::jsonb FROM explorations a WHERE id=$1",
            &[&task.exploration_id],
        )
        .await
        .unwrap()
        .get(0);
    let archive_id: i64 = d.query_one(
        "INSERT INTO task_archives(task_id,state,phase,format_version) VALUES($1,'restoring','restoring',3) RETURNING id",
        &[&task.id],
    ).await.unwrap().get(0);

    let mut tables = std::collections::BTreeMap::new();
    tables.insert("tasks".to_string(), json!([task_row]));
    tables.insert("explorations".to_string(), json!([exp_row]));
    let snap = TaskArchiveSnapshot {
        format_version: 3,
        task_id: task.id,
        exploration_id: task.exploration_id,
        tables,
        ..TaskArchiveSnapshot::default()
    };

    // Bad format version is rejected before touching the database.
    let mut bad = snap.clone();
    bad.format_version = 0;
    let err = d
        .restore_task_archive(archive_id, &bad, 0)
        .await
        .unwrap_err();
    assert!(err.is(ERR_TASK_ARCHIVE_FORMAT_MISMATCH), "got {err}");

    // Wrong state is rejected after the archive row is locked.
    d.execute(
        "UPDATE task_archives SET state='ready' WHERE id=$1",
        &[&archive_id],
    )
    .await
    .unwrap();
    let err = d
        .restore_task_archive(archive_id, &snap, 0)
        .await
        .unwrap_err();
    assert!(err.is(ERR_TASK_ARCHIVE_STATE), "got {err}");

    // Happy path.
    d.execute(
        "UPDATE task_archives SET state=$1 WHERE id=$2",
        &[&RESTORING, &archive_id],
    )
    .await
    .unwrap();
    let warnings = d.restore_task_archive(archive_id, &snap, 0).await.unwrap();
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let phase: String = d
        .query_one(
            "SELECT phase FROM task_archives WHERE id=$1",
            &[&archive_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(phase, "database_restored");

    d.execute("DELETE FROM task_archives WHERE id=$1", &[&archive_id])
        .await
        .unwrap();
    let _ = d.delete_task(task.id).await;
}
