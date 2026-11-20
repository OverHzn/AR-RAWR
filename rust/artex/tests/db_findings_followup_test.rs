//! Differential tests for `AddFindingFollowUpIntent` (ExplorationStore) in
//! `db/findings.go`: atomically creates a human intent from a live finding node,
//! copies anchors, records the derived_from edge + audit activity.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::exploration::Activity;
use artex::db::findings::ERR_FINDING_ORIGIN_UNAVAILABLE;

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

#[tokio::test]
async fn add_finding_follow_up_intent_happy_path() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("followup", "goal", None, 0, 0).await.unwrap();
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
        .add_finding(t.id, node_id, "rce", "n", "critical", "s", "e", "w", &[])
        .await
        .unwrap();

    // Anchor a real asset to the finding node so anchor-copy is exercised.
    let asset_id = d
        .query_one(
            "INSERT INTO assets(type, domain, company_source) VALUES ('subdomain', $1, 'explicit') RETURNING id",
            &[&format!("followup-{}.example", t.id)],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    d.execute(
        "INSERT INTO exploration_anchors(node_id, asset_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
        &[&node_id, &asset_id],
    )
    .await
    .unwrap();

    let store = d.exploration(t.exploration_id);
    let audit = Activity::new(
        None,
        "tester",
        "intent",
        "",
        "",
        false,
        "follow up",
        "detail",
        serde_json::json!({}),
        None,
        None,
        None,
        None,
        None,
    );
    let (intent_id, out_audit) = store
        .add_finding_follow_up_intent(fid, node_id, "do the thing", audit)
        .await
        .expect("create follow-up intent");

    assert!(intent_id > 0, "intent id assigned");
    assert!(out_audit.id > 0, "activity id assigned");
    assert_eq!(
        out_audit.node_id,
        Some(intent_id),
        "audit points at the intent"
    );
    assert_eq!(
        out_audit.summary,
        format!("follow up #{intent_id}"),
        "summary suffixed with intent id"
    );

    // Intent node shape + payload.
    let row = d
        .query_one(
            "SELECT kind, priority, state, origin, payload FROM exploration_nodes WHERE id=$1",
            &[&intent_id],
        )
        .await
        .unwrap();
    let kind: String = row.get(0);
    let priority: i32 = row.get(1);
    let state: String = row.get(2);
    let origin: String = row.get(3);
    let payload: serde_json::Value = row.get(4);
    assert_eq!(kind, "intent");
    assert_eq!(priority, 10);
    assert_eq!(state, "open");
    assert_eq!(origin, "human");
    assert_eq!(payload["summary"], serde_json::json!("do the thing"));
    assert_eq!(payload["source_finding_id"], serde_json::json!(fid));
    assert_eq!(
        payload["source_finding_node_id"],
        serde_json::json!(node_id)
    );
    assert_eq!(payload["asset_ids"], serde_json::json!([asset_id]));

    // Anchors copied.
    let copied: Vec<i64> = d
        .query(
            "SELECT asset_id FROM exploration_anchors WHERE node_id=$1",
            &[&intent_id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(copied, vec![asset_id], "asset anchor copied to intent");

    // Lineage edge derived_from finding -> intent.
    let rel: String = d
        .query_one(
            "SELECT rel FROM exploration_edges WHERE src_id=$1 AND dst_id=$2",
            &[&node_id, &intent_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(rel, "derived_from");

    // Audit activity persisted.
    let act_summary: String = d
        .query_one("SELECT summary FROM activity WHERE id=$1", &[&out_audit.id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(act_summary, format!("follow up #{intent_id}"));

    // Cleanup.
    let _ = d
        .execute("DELETE FROM findings WHERE task_id=$1", &[&t.id])
        .await;
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn add_finding_follow_up_intent_requires_live_origin() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d.create_task("noorigin", "goal", None, 0, 0).await.unwrap();
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
        .add_finding(t.id, node_id, "rce", "n", "high", "s", "e", "w", &[])
        .await
        .unwrap();

    let store = d.exploration(t.exploration_id);
    // Wrong node id -> no live origin row -> sentinel error.
    let audit = Activity::default();
    let err = store
        .add_finding_follow_up_intent(fid, node_id + 999_999, "x", audit)
        .await
        .expect_err("origin unavailable");
    assert!(err.is(ERR_FINDING_ORIGIN_UNAVAILABLE), "got {err}");

    // No intent node was left behind by the rolled-back tx.
    let n: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_nodes WHERE exploration_id=$1 AND kind='intent'",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0, "failed intent left no node");

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
