//! Differential tests for the exploration main sessions + asset refs + standalone
//! findings — fresh spec for the remaining `*ExplorationStore` methods in
//! `db/exploration.go` (CurrentMainSeg / ListMainSessions / NewMainSession /
//! AssetRefs / AddStandaloneFinding).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::DB;
use serde_json::json;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

#[tokio::test]
async fn exploration_main_sessions_asset_refs_findings() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let exp_id = d
        .create_exploration("desc", &format!("t-sessions-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- CurrentMainSeg: 0 when no sessions created.
    assert_eq!(store.current_main_seg().await.expect("seg 0"), 0);

    // ---- ListMainSessions: just the implicit segment 0.
    let sessions = store.list_main_sessions().await.expect("list sessions");
    assert_eq!(sessions.len(), 1, "only the implicit original session");
    assert_eq!(sessions[0].seq, 0);

    // ---- NewMainSession: creates seq 1, then 2.
    let s1 = store.new_main_session().await.expect("new session 1");
    assert_eq!(s1.seq, 1, "first new segment is 1");
    let s2 = store.new_main_session().await.expect("new session 2");
    assert_eq!(s2.seq, 2, "second new segment is 2");
    assert_eq!(store.current_main_seg().await.expect("seg 2"), 2);

    // ---- ListMainSessions: newest-first, always includes segment 0.
    let sessions = store.list_main_sessions().await.expect("list sessions 2");
    assert_eq!(sessions.len(), 3, "two stored + implicit 0");
    assert_eq!(sessions[0].seq, 2, "newest first");
    assert_eq!(sessions[1].seq, 1);
    assert_eq!(sessions[2].seq, 0, "implicit original last");

    // ---- AssetRefs: anchor a node to an asset, then query.
    // Create an asset row (the assets table).
    let asset_id = d
        .query_one(
            "INSERT INTO assets(type, domain) VALUES ('subdomain', $1) RETURNING id",
            &[&format!("asset-{stamp}")],
        )
        .await
        .expect("create asset")
        .get::<_, i64>(0);

    let intent = store
        .add_intent(&json!({"summary": "test intent"}), 1, &[], "planner")
        .await
        .expect("intent");
    let fact = store
        .add_node(
            "fact",
            &json!({"text": "a fact about the asset"}),
            0,
            "confirmed",
            "w",
            &[],
        )
        .await
        .expect("fact");

    // Anchor both nodes to the asset.
    store.anchor(intent, asset_id).await.expect("anchor intent");
    store.anchor(fact, asset_id).await.expect("anchor fact");

    let refs = store.asset_refs(asset_id).await.expect("asset refs");
    assert_eq!(refs.len(), 2, "two nodes anchored");
    // Newest first (id DESC): fact was created after intent.
    assert_eq!(refs[0].id, fact, "newest first");
    assert_eq!(refs[1].id, intent);
    // Summary extraction: fact has "text", intent has "summary".
    assert_eq!(refs[0].summary, "a fact about the asset", "fact text used");
    assert_eq!(refs[1].summary, "test intent", "intent summary used");
    assert!(refs.iter().all(|r| r.kind == "fact" || r.kind == "intent"));

    // ---- AssetRefs: asset_id <= 0 -> empty.
    assert!(store.asset_refs(0).await.expect("refs 0").is_empty());
    assert!(store.asset_refs(-1).await.expect("refs neg").is_empty());

    // ---- AddStandaloneFinding: insert a finding with asset_ids.
    let fid = store
        .add_standalone_finding(
            0, // task_id (no task)
            intent,
            "sqli",
            "SQL Injection",
            "high",
            "User input flows to query",
            "GET /search?q=...",
            "w1",
            &[asset_id],
        )
        .await
        .expect("add finding");

    // Verify the finding row was inserted correctly.
    let row = d
        .query_one(
            "SELECT task_id IS NULL, node_id, vulnclass, name, severity, asset_ids \
             FROM findings WHERE id=$1",
            &[&fid],
        )
        .await
        .expect("finding row");
    assert!(row.get::<_, bool>(0), "task_id=0 -> NULL");
    assert_eq!(row.get::<_, i64>(1), intent, "node_id set");
    assert_eq!(row.get::<_, String>(2), "sqli");
    assert_eq!(row.get::<_, String>(3), "SQL Injection");
    assert_eq!(row.get::<_, String>(4), "high");
    let aids: serde_json::Value = row.get(5);
    assert_eq!(aids, json!([asset_id]), "asset_ids JSONB array");

    // ---- AddFinding with task_id > 0 and node_id = 0.
    let fid2 = store
        .add_standalone_finding(
            999_999, // non-existent task_id (stored as-is, FK will reject if enforced)
            0,       // node_id = 0 -> NULL
            "xss",
            "XSS",
            "medium",
            " reflected",
            "",
            "w2",
            &[],
        )
        .await;
    // task_id 999999 likely violates FK; just check it either succeeds or errors.
    // If it succeeds, verify node_id IS NULL and asset_ids = [].
    if let Ok(fid2) = fid2 {
        let row = d
            .query_one(
                "SELECT node_id IS NULL, asset_ids FROM findings WHERE id=$1",
                &[&fid2],
            )
            .await
            .expect("finding2 row");
        assert!(row.get::<_, bool>(0), "node_id=0 -> NULL");
        let aids: serde_json::Value = row.get(1);
        assert_eq!(aids, json!([]), "empty asset_ids -> []");
    }

    // Cleanup (findings cascade on exploration delete; asset delete too).
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
    let _ = d
        .execute("DELETE FROM assets WHERE id=$1", &[&asset_id])
        .await;
}
