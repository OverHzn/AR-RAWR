//! Differential tests for the exploration intent claim lifecycle — fresh spec for
//! the `*ExplorationStore` claim methods in `db/exploration.go` (ClaimIntent /
//! ClaimIntentToken / ClaimPausedIntentToken / HeartbeatIntentClaim /
//! SettleIntentClaim / SettleIntentClaimBlocked).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::{ERR_INTENT_CLAIM_LOST, ERR_INTENT_STATE_CONFLICT};
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

async fn node_state(d: &DB, id: i64) -> Option<String> {
    let row = d
        .query_one("SELECT state FROM exploration_nodes WHERE id=$1", &[&id])
        .await
        .expect("state query");
    row.get(0)
}

async fn owner(d: &DB, id: i64) -> String {
    let row = d
        .query_one(
            "SELECT COALESCE(owner,'') FROM exploration_nodes WHERE id=$1",
            &[&id],
        )
        .await
        .expect("owner query");
    row.get(0)
}

async fn token_of(d: &DB, id: i64) -> Option<String> {
    let row = d
        .query_one(
            "SELECT unclaim_token FROM exploration_nodes WHERE id=$1",
            &[&id],
        )
        .await
        .expect("token query");
    row.get(0)
}

async fn completed_at_null(d: &DB, id: i64) -> bool {
    let row = d
        .query_one(
            "SELECT completed_at IS NULL FROM exploration_nodes WHERE id=$1",
            &[&id],
        )
        .await
        .expect("completed_at query");
    row.get(0)
}

#[tokio::test]
async fn exploration_intent_claim_lifecycle() {
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
        .create_exploration("desc", &format!("t-claim-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // ---- ClaimIntentToken on an open intent: running + owner + token set + lease.
    let a = store
        .add_intent(&json!({"summary": "a"}), 1, &[], "planner")
        .await
        .expect("a");
    assert!(!store.claim_intent(a, "alice").await.is_err());
    // Already claimed above via claim_intent; token now current.
    let tok_a = token_of(&d, a).await.expect("a token present");
    assert_eq!(tok_a.len(), 32, "32-char hex token");
    assert!(tok_a.chars().all(|c| c.is_ascii_hexdigit()), "hex");
    assert_eq!(node_state(&d, a).await.as_deref(), Some("running"));
    assert_eq!(owner(&d, a).await, "alice");

    // ---- Claiming an already-running intent fails (not 'open'): claim_intent false.
    let claimed_again = store.claim_intent(a, "bob").await.expect("claim again");
    assert!(
        !claimed_again,
        "an open-only claim on a running intent is a no-op"
    );
    assert_eq!(owner(&d, a).await, "alice", "owner unchanged");

    // ---- Heartbeat with the correct token succeeds; a stale token -> claim lost.
    store
        .heartbeat_intent_claim(a, &tok_a)
        .await
        .expect("heartbeat ok");
    let stale = "deadbeef".repeat(4); // 32 hex chars, wrong value
    let err = store
        .heartbeat_intent_claim(a, &stale)
        .await
        .expect_err("stale heartbeat");
    assert!(err.is(ERR_INTENT_CLAIM_LOST), "stale heartbeat: {err}");

    // ---- SettleIntentClaim with a stale token -> claim lost, nothing written.
    let err = store
        .settle_intent_claim(a, &stale, "done")
        .await
        .expect_err("stale settle");
    assert!(err.is(ERR_INTENT_CLAIM_LOST), "stale settle: {err}");
    assert_eq!(
        node_state(&d, a).await.as_deref(),
        Some("running"),
        "still running"
    );

    // ---- SettleIntentClaim with the right token -> done + completed_at + token freed.
    store
        .settle_intent_claim(a, &tok_a, "done")
        .await
        .expect("settle done");
    assert_eq!(node_state(&d, a).await.as_deref(), Some("done"));
    assert!(!completed_at_null(&d, a).await, "done stamps completed_at");
    assert!(token_of(&d, a).await.is_none(), "token released");

    // ---- Non-terminal settle clears completed_at. Reach a terminal state first,
    //      reopen (exhausted->open clears completed_at), then settle non-terminal.
    let b = store
        .add_intent(&json!({"summary": "b"}), 1, &[], "planner")
        .await
        .expect("b");
    let tok_b = store.claim_intent_token(b, "carol").await.expect("claim b");
    assert!(!tok_b.is_empty());
    assert_eq!(owner(&d, b).await, "carol");
    store
        .settle_intent_claim(b, &tok_b, "exhausted")
        .await
        .expect("b exhausted");
    assert!(
        !completed_at_null(&d, b).await,
        "terminal settle stamps completed_at"
    );
    store
        .reopen_intent(b)
        .await
        .expect("reopen b (exhausted->open)");
    assert!(completed_at_null(&d, b).await, "reopen clears completed_at");
    let tok_b2 = store
        .claim_intent_token(b, "carol")
        .await
        .expect("claim b2");
    store
        .settle_intent_claim(b, &tok_b2, "open")
        .await
        .expect("settle open");
    assert_eq!(node_state(&d, b).await.as_deref(), Some("open"));
    assert!(
        completed_at_null(&d, b).await,
        "non-terminal settle clears completed_at"
    );

    // ---- SettleIntentClaimBlocked: running -> blocked with reason + completed_at.
    let c = store
        .add_intent(&json!({"summary": "c"}), 1, &[], "planner")
        .await
        .expect("c");
    let tok_c = store.claim_intent_token(c, "dan").await.expect("claim c");
    store
        .settle_intent_claim_blocked(c, &tok_c, "needs creds")
        .await
        .expect("block c");
    assert_eq!(node_state(&d, c).await.as_deref(), Some("blocked"));
    let reason: String = d
        .query_one(
            "SELECT COALESCE(blocked_reason,'') FROM exploration_nodes WHERE id=$1",
            &[&c],
        )
        .await
        .expect("reason")
        .get(0);
    assert_eq!(reason, "needs creds", "blocked_reason recorded");
    assert!(
        !completed_at_null(&d, c).await,
        "blocked stamps completed_at"
    );
    assert!(token_of(&d, c).await.is_none(), "token released on block");

    // ---- SettleIntentClaimBlocked with an empty reason stores NULLIF -> empty.
    let e = store
        .add_intent(&json!({"summary": "e"}), 1, &[], "planner")
        .await
        .expect("e");
    let tok_e = store.claim_intent_token(e, "erin").await.expect("claim e");
    store
        .settle_intent_claim_blocked(e, &tok_e, "")
        .await
        .expect("block e");
    let reason: String = d
        .query_one(
            "SELECT COALESCE(blocked_reason,'') FROM exploration_nodes WHERE id=$1",
            &[&e],
        )
        .await
        .expect("reason e")
        .get(0);
    assert_eq!(reason, "", "empty reason -> empty");

    // ---- State-conflict branch: token still current but intent left 'running'.
    // Simulate: settle matched 0 rows (state not running) while unclaim_token still
    // equals the caller's token. Claim f, then force it out of 'running' under the
    // same token by clearing running via a direct state flip that keeps the token.
    let f = store
        .add_intent(&json!({"summary": "f"}), 1, &[], "planner")
        .await
        .expect("f");
    let tok_f = store.claim_intent_token(f, "fred").await.expect("claim f");
    // Flip state to 'paused' but keep unclaim_token=tok_f (so the fenced settle's
    // WHERE state='running' matches 0, yet the token still equals the caller's).
    d.execute(
        "UPDATE exploration_nodes SET state='paused' WHERE id=$1",
        &[&f],
    )
    .await
    .expect("force paused");
    let err = store
        .settle_intent_claim(f, &tok_f, "done")
        .await
        .expect_err("state conflict");
    assert!(
        err.is(ERR_INTENT_STATE_CONFLICT),
        "still-owned token but left running -> state conflict: {err}"
    );

    // ---- ClaimPausedIntentToken: paused -> running with fresh token.
    // (f is paused above, still carrying tok_f.)
    let tok_f2 = store
        .claim_paused_intent_token(f)
        .await
        .expect("claim paused f");
    assert!(!tok_f2.is_empty(), "paused claim yields a token");
    assert_ne!(tok_f2, tok_f, "fresh token");
    assert_eq!(node_state(&d, f).await.as_deref(), Some("running"));
    assert_eq!(token_of(&d, f).await.as_deref(), Some(tok_f2.as_str()));
    // A paused-token claim on a now-running intent returns "".
    let again = store
        .claim_paused_intent_token(f)
        .await
        .expect("claim paused again");
    assert!(again.is_empty(), "no longer paused -> empty token");

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
