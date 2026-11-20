//! Differential test for `ExplorationStore::CancelIntent` (+ the private
//! `intentTokenRollup`) — fresh spec for the hard-delete graph cascade in
//! `db/exploration.go`. Verifies the exclusive downward cascade (shared nodes
//! kept, goals/origin facts protected), finding-row removal, the per-intent
//! token rollup (usage/result frames grouped by UTC day, partial-fill from the
//! latest cumulative frame, trailing-frame handling), and idempotent not-found.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::IntentCleanup;
use artex::db::DB;
use serde_json::{json, Value};

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
async fn cancel_intent_cascade_and_token_rollup() {
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
        .create_exploration("desc", &format!("t-cancel-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // Protected roots: a goal + the origin fact (never deleted with an intent).
    let goal_id = store
        .add_goal(&json!({"text": "g"}), "planner")
        .await
        .expect("goal");
    let origin_id = store
        .add_node("fact", &json!({"root": true}), 0, "origin", "system", &[])
        .await
        .expect("origin fact");

    // The intent to cancel, plus a sibling intent that keeps a shared fact alive.
    let intent_a = store
        .add_intent(&json!({"goal": "a", "summary": "sum-a"}), 1, &[], "planner")
        .await
        .expect("intent a");
    let intent_b = store
        .add_intent(&json!({"goal": "b"}), 1, &[], "planner")
        .await
        .expect("intent b");
    store
        .link(goal_id, "spawns", intent_a)
        .await
        .expect("link a");
    store
        .link(goal_id, "spawns", intent_b)
        .await
        .expect("link b");

    // Exclusive descendants of intent_a: f1 (yields), f2 (derived_from f1) — both
    // reachable ONLY through intent_a -> deleted.
    let f1 = store
        .add_node("fact", &json!({"n": 1}), 0, "confirmed", "w", &[])
        .await
        .expect("f1");
    let f2 = store
        .add_node("fact", &json!({"n": 2}), 0, "confirmed", "w", &[])
        .await
        .expect("f2");
    store.link(intent_a, "yields", f1).await.expect("link f1");
    store.link(f1, "derived_from", f2).await.expect("link f2");

    // A shared fact reachable from BOTH intents -> kept when only a is cancelled.
    let shared = store
        .add_node("fact", &json!({"n": 3}), 0, "confirmed", "w", &[])
        .await
        .expect("shared");
    store
        .link(intent_a, "yields", shared)
        .await
        .expect("link shared a");
    store
        .link(intent_b, "yields", shared)
        .await
        .expect("link shared b");

    // A finding node (exclusive to intent_a) with a findings-table row -> removed.
    let finding = store
        .add_node("finding", &json!({"n": 4}), 0, "confirmed", "w", &[])
        .await
        .expect("finding");
    store
        .link(intent_a, "yields", finding)
        .await
        .expect("link finding");
    let finding_tag = format!("finding-{stamp}");
    d.execute(
        "INSERT INTO findings(node_id, name) VALUES ($1, $2)",
        &[&finding, &finding_tag],
    )
    .await
    .expect("seed finding row");

    // Activity on intent_a: a usage/result token ledger (see rollup expectations).
    // Five frames across three UTC days.
    for (kind, toks, at) in [
        ("usage", "10, 5, 1, 2", "2024-01-01T12:00:00Z"),
        ("result", "NULL, NULL, NULL, NULL", "2024-01-01T23:59:59Z"),
        ("usage", "100, 50, 4, 8", "2024-01-02T06:00:00Z"),
        ("result", "100, 50, NULL, NULL", "2024-01-02T07:00:00Z"),
        ("usage", "7, 7, 7, 7", "2024-01-03T09:00:00Z"),
    ] {
        d.execute(
            &format!(
                "INSERT INTO activity(exploration_id, node_id, kind, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, created_at) \
             VALUES ($1, $2, '{kind}', {toks}, '{at}')"
            ),
            &[&exp_id, &intent_a],
        )
        .await
        .expect("seed activity frame");
    }

    // ---- Cancel intent_a.
    let cleanup = store.cancel_intent(intent_a).await.expect("cancel a");
    assert_eq!(
        cleanup,
        IntentCleanup {
            intents: 1,
            facts: 2,
            findings: 1,
            activities: 5,
        },
        "cleanup counts: 1 intent + 2 facts + 1 finding, 5 activity rows removed"
    );

    // Exclusive descendants + the finding node are gone.
    for gone in [intent_a, f1, f2, finding] {
        assert!(
            store.get_node(gone).await.expect("get").is_none(),
            "node {gone} should be deleted"
        );
    }
    // The shared fact, the sibling intent, and the protected roots survive.
    for kept in [intent_b, shared, goal_id, origin_id] {
        assert!(
            store.get_node(kept).await.expect("get").is_some(),
            "node {kept} should survive"
        );
    }

    // The findings-table row was deleted (not orphaned to node_id NULL).
    let findings_left: i64 = d
        .query_one(
            "SELECT COUNT(*) FROM findings WHERE name=$1",
            &[&finding_tag],
        )
        .await
        .expect("count findings")
        .get(0);
    assert_eq!(findings_left, 0, "finding row removed with its node");

    // ---- Token rollup: one 'token-ledger' result row per UTC day, summed.
    let ledger = d
        .query(
            "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
         metadata, created_at FROM activity \
         WHERE exploration_id=$1 AND worker='token-ledger' ORDER BY created_at",
            &[&exp_id],
        )
        .await
        .expect("ledger rows");
    assert_eq!(ledger.len(), 3, "one token-ledger row per UTC day");
    let expect = [
        (10, 5, 1, 2, "2024-01-01"),
        (100, 50, 4, 8, "2024-01-02"),
        (7, 7, 7, 7, "2024-01-03"),
    ];
    for (row, (ei, eo, er, ew, day)) in ledger.iter().zip(expect) {
        let i: Option<i32> = row.get(0);
        let o: Option<i32> = row.get(1);
        let r: Option<i32> = row.get(2);
        let w: Option<i32> = row.get(3);
        assert_eq!(
            (
                i.unwrap_or(-1),
                o.unwrap_or(-1),
                r.unwrap_or(-1),
                w.unwrap_or(-1)
            ),
            (ei, eo, er, ew),
            "token sums for day {day}"
        );
        let meta: Value = row.get(4);
        assert_eq!(meta["token_day"], json!(day), "token_day {day}");
        assert_eq!(meta["token_rollup"], json!(true));
        assert_eq!(meta["cancelled_intent_id"], json!(intent_a));
    }

    // ---- Idempotent: cancelling an already-deleted intent -> not found.
    let err = store
        .cancel_intent(intent_a)
        .await
        .expect_err("second cancel errors");
    assert!(
        err.to_string().contains("intent not found"),
        "not-found message, got: {err:?}"
    );

    // Cleanup (cascade from the exploration).
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
