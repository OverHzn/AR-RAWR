//! Differential tests for the task-delete slice of `db/tasks.go`
//! (`TaskDeleteResult` + `DeleteTask` / `DeleteTaskCascade`) and the
//! `revokeTaskPins` / `settleTaskTargetRequests` history helpers.
//!
//! The Go specs (`TestTaskLifecycleAndDeleteCascade`, `TestTaskDeleteCascadeAssets`)
//! drive `Assets().UpsertRootDomain` / `HostsForTaskDeletion` / `AddFinding` which
//! are not ported yet, so assets / anchors / findings / pins / target requests are
//! seeded via raw SQL and the cascade is asserted the same way (exclusive asset
//! deleted, shared asset detached, anchor-only deletable, cross-task anchor
//! protected, findings removed, active pins revoked, open target requests settled).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Insert an asset row; returns its id.
async fn seed_asset(d: &DB, domain: &str, task_ids: &[i64]) -> i64 {
    d.query_one(
        "INSERT INTO assets(type, domain, root_domain, task_ids) \
         VALUES ('root_domain',$1,$1,$2::bigint[]) RETURNING id",
        &[&domain, &task_ids],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

/// Find the origin fact node id of an exploration.
async fn origin_node(d: &DB, exp_id: i64) -> i64 {
    d.query_one(
        "SELECT id FROM exploration_nodes WHERE exploration_id=$1 AND state='origin'",
        &[&exp_id],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

#[tokio::test]
async fn delete_task_cascades_exploration_subgraph() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("migration test", "goal X", None, 0, 0)
        .await
        .expect("create");
    // Populate the subgraph with an extra intent node + an activity row.
    d.execute(
        "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
         VALUES ($1,'intent','{\"summary\":\"x\"}'::jsonb,5,'open','planner')",
        &[&t.exploration_id],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO activity(exploration_id, kind, metadata) VALUES ($1,'note','{}'::jsonb)",
        &[&t.exploration_id],
    )
    .await
    .unwrap();

    d.delete_task(t.id).await.expect("delete");

    assert!(
        d.get_task(t.id).await.expect("get").is_none(),
        "task should be gone"
    );
    let nodes: i64 = d
        .query_one(
            "SELECT count(*) FROM exploration_nodes WHERE exploration_id=$1",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(nodes, 0, "exploration nodes cascade-deleted");
    let exps: i64 = d
        .query_one(
            "SELECT count(*) FROM explorations WHERE id=$1",
            &[&t.exploration_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(exps, 0, "exploration row deleted");
}

#[tokio::test]
async fn delete_task_cascade_assets_findings_pins() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let first = d
        .create_task("cascade delete test", "goal A", None, 0, 0)
        .await
        .unwrap();
    let second = d
        .create_task("shared asset retention", "goal B", None, 0, 0)
        .await
        .unwrap();

    // exclusive: owned only by `first`.
    let exclusive_id = seed_asset(&d, &format!("delete-{stamp}.example.test"), &[first.id]).await;
    // shared: owned by first AND second.
    let shared_id = seed_asset(
        &d,
        &format!("shared-{stamp}.example.test"),
        &[first.id, second.id],
    )
    .await;
    // anchor-only: no task_id, anchored to first's origin.
    let anchor_only_id = seed_asset(&d, &format!("anchor-only-{stamp}.example.test"), &[]).await;
    // protected: owned by first, but ALSO anchored to second's origin (other task).
    let protected_id = seed_asset(
        &d,
        &format!("other-anchor-{stamp}.example.test"),
        &[first.id],
    )
    .await;

    let first_origin = origin_node(&d, first.exploration_id).await;
    let second_origin = origin_node(&d, second.exploration_id).await;
    d.execute(
        "INSERT INTO exploration_anchors(node_id, asset_id) VALUES ($1,$2)",
        &[&first_origin, &anchor_only_id],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO exploration_anchors(node_id, asset_id) VALUES ($1,$2)",
        &[&second_origin, &protected_id],
    )
    .await
    .unwrap();

    // A finding owned by `first`, plus an active pin and an open target request.
    let finding_id = d
        .query_one(
            "INSERT INTO findings(task_id, vulnclass, name, severity, summary, evidence, worker, asset_ids) \
             VALUES ($1,'__task_delete_cascade__','','high','summary','evidence','tester',$2::jsonb) RETURNING id",
            &[&first.id, &serde_json::json!([exclusive_id])],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    d.execute(
        "INSERT INTO target_pins(task_id, host, port, status) VALUES ($1,'h',443,'active')",
        &[&first.id],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO intercept_pending(task_id, tool_name, status, target_key) \
         VALUES ($1,'http','pending','h:443')",
        &[&first.id.to_string()],
    )
    .await
    .unwrap();

    let result = d
        .delete_task_cascade(first.id, true, true, true)
        .await
        .expect("cascade");
    // Deleted: exclusive (owned only by first) + anchor-only (anchored, no owner).
    assert!(
        result.assets_deleted >= 2,
        "exclusive + anchor-only deleted: {result:?}"
    );
    // Detached: shared + protected (anchored by other task) retained, id removed.
    assert!(
        result.assets_detached >= 2,
        "shared + protected detached: {result:?}"
    );
    assert_eq!(result.findings_deleted, 1, "finding removed");

    // Remaining assets: only shared + protected.
    let remaining: Vec<i64> = d
        .query(
            "SELECT id FROM assets WHERE id=ANY($1::bigint[])",
            &[&vec![exclusive_id, shared_id, anchor_only_id, protected_id]],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<_, i64>(0))
        .collect();
    assert_eq!(remaining.len(), 2, "only shared + protected remain");
    assert!(remaining.contains(&shared_id) && remaining.contains(&protected_id));

    // shared: detached from first, still owned by second.
    let shared_tasks: Vec<i64> = d
        .query_one("SELECT task_ids FROM assets WHERE id=$1", &[&shared_id])
        .await
        .unwrap()
        .get::<_, Vec<i64>>(0);
    assert_eq!(shared_tasks, vec![second.id], "shared detached from first");

    // protected: other task's anchor preserved the asset; ownership removed.
    let protected_tasks: Vec<i64> = d
        .query_one("SELECT task_ids FROM assets WHERE id=$1", &[&protected_id])
        .await
        .unwrap()
        .get::<_, Vec<i64>>(0);
    assert!(protected_tasks.is_empty(), "protected ownership detached");

    // finding gone.
    let f: i64 = d
        .query_one("SELECT count(*) FROM findings WHERE id=$1", &[&finding_id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(f, 0, "finding deleted");

    // first task gone; second still alive.
    assert!(d.get_task(first.id).await.expect("get").is_none());
    assert!(d.get_task(second.id).await.expect("get").is_some());

    // Cleanup: drop second + the two surviving assets.
    d.delete_task(second.id).await.unwrap();
    let _ = d
        .execute(
            "DELETE FROM assets WHERE id=ANY($1::bigint[])",
            &[&vec![shared_id, protected_id]],
        )
        .await;
}

#[tokio::test]
async fn delete_task_retains_llm_usage_and_revokes_pins() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("history retention", "goal", None, 0, 0)
        .await
        .unwrap();

    // An llm_usage ledger row + llm_record + active pin + pending target request.
    d.execute(
        "INSERT INTO llm_usage(task_id, worker, input_tokens, output_tokens) \
         VALUES ($1,'planner',10,20)",
        &[&t.id.to_string()],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO llm_records(task_id, model) VALUES ($1,'m')",
        &[&t.id.to_string()],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO target_pins(task_id, host, port, status) VALUES ($1,'h',80,'active')",
        &[&t.id],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO intercept_pending(task_id, tool_name, status, target_key, audit) \
         VALUES ($1,'http','pending','h:80','{\"prior\":true}'::jsonb)",
        &[&t.id.to_string()],
    )
    .await
    .unwrap();

    // delete_records=true removes llm_records but keeps llm_usage.
    let result = d
        .delete_task_cascade(t.id, false, false, true)
        .await
        .expect("cascade");
    assert_eq!(result.llm_records_deleted, 1, "llm record removed");

    // llm_usage retained as historical accounting.
    let usage: i64 = d
        .query_one(
            "SELECT count(*) FROM llm_usage WHERE task_id=$1",
            &[&t.id.to_string()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(usage, 1, "llm_usage kept as history");

    // llm_records gone.
    let recs: i64 = d
        .query_one(
            "SELECT count(*) FROM llm_records WHERE COALESCE(task_id,'')=$1",
            &[&t.id.to_string()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(recs, 0, "llm_records deleted");

    // Pin revoked (kept as history), target request settled to denied w/ audit.
    let (status, reason): (String, String) = {
        let r = d
            .query_one(
                "SELECT status, revoked_reason FROM target_pins WHERE task_id=$1",
                &[&t.id],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(status, "revoked", "active pin revoked");
    assert_eq!(reason, "task_deleted", "revoked_reason=task_deleted");

    let (pstatus, decided_by, audit): (String, String, serde_json::Value) = {
        let r = d
            .query_one(
                "SELECT status, decided_by, audit FROM intercept_pending WHERE task_id=$1",
                &[&t.id.to_string()],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    };
    assert_eq!(pstatus, "denied", "pending target request denied");
    assert_eq!(decided_by, "system", "decided_by=system");
    assert_eq!(
        audit["decision_reason"], "task deleted",
        "audit stamped with decision reason"
    );
    assert_eq!(
        audit["prior"], true,
        "audit merged with prior JSONB (jsonb || concat)"
    );

    // Cleanup history rows + task.
    let _ = d
        .execute(
            "DELETE FROM llm_usage WHERE task_id=$1",
            &[&t.id.to_string()],
        )
        .await;
    let _ = d
        .execute("DELETE FROM target_pins WHERE task_id=$1", &[&t.id])
        .await;
    let _ = d
        .execute(
            "DELETE FROM intercept_pending WHERE task_id=$1",
            &[&t.id.to_string()],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}
