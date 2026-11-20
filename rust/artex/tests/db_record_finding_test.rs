//! Differential tests for the atomic finding recorder slice of
//! `db/finding_traffic.go` (`RecordFindingTx` / `RecordFinding` via the
//! `ExplorationStore` wrapper). Exercises node creation + anchors + optional
//! intent `yields` edge + findings row + same-transaction notification event +
//! (empty) traffic evidence, plus the two guard errors.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::finding_evidence::RecordFindingInput;
use artex::notify::EVENT_FINDING_CREATED;

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

/// Insert an intent node into the exploration; returns its id.
async fn insert_intent(d: &artex::db::DB, exploration_id: i64) -> i64 {
    d.query_one(
        "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
         VALUES ($1,'intent','{}'::jsonb,5,'open','tester') RETURNING id",
        &[&exploration_id],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

/// Register a subdomain asset and return its id (so the anchor FK is satisfied).
async fn insert_asset(d: &artex::db::DB, domain: &str) -> i64 {
    d.query_one(
        "INSERT INTO assets(type, domain, company_source) VALUES ('subdomain', $1, 'explicit') RETURNING id",
        &[&domain],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

#[tokio::test]
async fn record_finding_with_intent_and_anchors() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("rec-intent", "goal", None, 0, 0)
        .await
        .unwrap();
    let intent_id = insert_intent(&d, t.exploration_id).await;
    let asset_id = insert_asset(&d, &format!("rec-{}.example", t.id)).await;

    let input = RecordFindingInput {
        task_id: t.id,
        exploration_id: t.exploration_id,
        intent_id,
        vulnclass: "sqli".to_string(),
        name: "SQLi".to_string(),
        severity: "high".to_string(),
        summary: "a summary".to_string(),
        evidence: "the poc".to_string(),
        worker: "tester".to_string(),
        counterevidence: String::new(),
        confidence_rationale: String::new(),
        asset_ids: vec![asset_id],
    };

    let out = d
        .exploration(t.exploration_id)
        .record_finding(input)
        .await
        .expect("record finding");

    assert!(out.finding_id > 0, "finding row id");
    assert!(out.node_id > 0, "finding node id");
    assert_eq!(out.traffic.finding_id, out.finding_id);
    assert_eq!(out.traffic.version, 0, "no evidence -> version 0");
    assert!(out.traffic.bindings.is_empty());

    // Node is a confirmed finding node in the right exploration.
    let (kind, state, exploration_id): (String, String, i64) = {
        let r = d
            .query_one(
                "SELECT kind, state, exploration_id FROM exploration_nodes WHERE id=$1",
                &[&out.node_id],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    };
    assert_eq!(kind, "finding");
    assert_eq!(state, "confirmed");
    assert_eq!(exploration_id, t.exploration_id);

    // Anchor row present.
    let anchors: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_anchors WHERE node_id=$1 AND asset_id=$2",
            &[&out.node_id, &asset_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(anchors, 1, "asset anchored");

    // Intent -> finding yields edge present.
    let edges: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_edges WHERE exploration_id=$1 AND src_id=$2 AND rel='yields' AND dst_id=$3",
            &[&t.exploration_id, &intent_id, &out.node_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(edges, 1, "yields edge from intent to finding");

    // findings row carries the fields; task_id NOT NULL (NULLIF kept it).
    let (ftask, fvclass, fname, fsev, fassets): (i64, String, String, String, serde_json::Value) = {
        let r = d
            .query_one(
                "SELECT task_id, vulnclass, name, severity, asset_ids FROM findings WHERE id=$1",
                &[&out.finding_id],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4))
    };
    assert_eq!(ftask, t.id);
    assert_eq!(fvclass, "sqli");
    assert_eq!(fname, "SQLi");
    assert_eq!(fsev, "high");
    assert_eq!(fassets, serde_json::json!([asset_id]));

    // Same-transaction notification event was written.
    let (ekind, efinding): (String, i64) = {
        let r = d
            .query_one(
                "SELECT kind, finding_id FROM notification_events WHERE finding_id=$1",
                &[&out.finding_id],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(ekind, EVENT_FINDING_CREATED);
    assert_eq!(efinding, out.finding_id);

    // Cleanup.
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE finding_id=$1",
            &[&out.finding_id],
        )
        .await;
    let _ = d
        .execute("DELETE FROM findings WHERE id=$1", &[&out.finding_id])
        .await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&out.node_id])
        .await;
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn record_finding_rejects_task_exploration_mismatch() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("rec-mismatch", "goal", None, 0, 0)
        .await
        .unwrap();
    // A different exploration id than the task's.
    let other_exp = d.create_exploration("other", "goal").await.unwrap();

    let input = RecordFindingInput {
        task_id: t.id,
        exploration_id: other_exp,
        intent_id: 0,
        vulnclass: "x".to_string(),
        name: String::new(),
        severity: "low".to_string(),
        summary: String::new(),
        evidence: String::new(),
        worker: "tester".to_string(),
        counterevidence: String::new(),
        confidence_rationale: String::new(),
        asset_ids: Vec::new(),
    };

    let err = d
        .exploration(other_exp)
        .record_finding(input)
        .await
        .expect_err("task/exploration mismatch rejected");
    assert!(
        err.to_string().contains("does not match the exploration"),
        "got {err}"
    );

    // No finding node should have been persisted (transaction rolled back).
    let nodes: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_nodes WHERE exploration_id=$1 AND kind='finding'",
            &[&other_exp],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(nodes, 0, "no finding node persisted on error");

    // Cleanup.
    let _ = d
        .execute("DELETE FROM explorations WHERE id=$1", &[&other_exp])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn record_finding_rejects_foreign_intent() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("rec-intent-bad", "goal", None, 0, 0)
        .await
        .unwrap();
    // An intent that belongs to a DIFFERENT exploration.
    let other_exp = d.create_exploration("other2", "goal").await.unwrap();
    let foreign_intent = insert_intent(&d, other_exp).await;

    let input = RecordFindingInput {
        task_id: t.id,
        exploration_id: t.exploration_id,
        intent_id: foreign_intent,
        vulnclass: "x".to_string(),
        name: String::new(),
        severity: "low".to_string(),
        summary: String::new(),
        evidence: String::new(),
        worker: "tester".to_string(),
        counterevidence: String::new(),
        confidence_rationale: String::new(),
        asset_ids: Vec::new(),
    };

    let err = d
        .exploration(t.exploration_id)
        .record_finding(input)
        .await
        .expect_err("foreign intent rejected");
    assert!(
        err.to_string()
            .contains("intent_id must be an intent of this task"),
        "got {err}"
    );

    // Cleanup.
    let _ = d
        .execute(
            "DELETE FROM exploration_nodes WHERE id=$1",
            &[&foreign_intent],
        )
        .await;
    let _ = d
        .execute("DELETE FROM explorations WHERE id=$1", &[&other_exp])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn record_finding_null_task_when_zero() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // A task-less finding (task_id 0) — NULLIF($1,0) yields NULL.
    let exp = d.create_exploration("rec-noask", "goal").await.unwrap();
    let input = RecordFindingInput {
        task_id: 0,
        exploration_id: exp,
        intent_id: 0,
        vulnclass: "info".to_string(),
        name: String::new(),
        severity: "info".to_string(),
        summary: String::new(),
        evidence: String::new(),
        worker: "tester".to_string(),
        counterevidence: String::new(),
        confidence_rationale: String::new(),
        asset_ids: Vec::new(),
    };

    let out = d
        .exploration(exp)
        .record_finding(input)
        .await
        .expect("record task-less finding");
    let task_id: Option<i64> = d
        .query_one(
            "SELECT task_id FROM findings WHERE id=$1",
            &[&out.finding_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(task_id, None, "task_id NULL when 0");

    // Cleanup.
    let _ = d
        .execute(
            "DELETE FROM notification_events WHERE finding_id=$1",
            &[&out.finding_id],
        )
        .await;
    let _ = d
        .execute("DELETE FROM findings WHERE id=$1", &[&out.finding_id])
        .await;
    let _ = d
        .execute("DELETE FROM exploration_nodes WHERE id=$1", &[&out.node_id])
        .await;
    let _ = d
        .execute("DELETE FROM explorations WHERE id=$1", &[&exp])
        .await;
}
