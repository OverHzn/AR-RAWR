//! Differential tests for the `llmhealth` + `skill_gaps` db leaf cluster —
//! focused specs (the Go package ships no `_test.go` for these two files, so this
//! is a fresh differential test against the same Postgres the Go oracle uses).
//!
//! Runs against `ARTEX_PG_DSN`, serialised behind one shared async lock, and skips
//! cleanly when the DSN is unset or unreachable. Fixtures use raw SQL with
//! process-unique keys and clean up after themselves.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::llmhealth::LLMHealth;
use artex::db::DB;
use artex::llm::Context;

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

fn ts_secs(dur: std::time::Duration) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + dur
}

// --- llm_profile_health ------------------------------------------------------

#[tokio::test]
async fn llm_health_upsert_load_clear() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // A throwaway profile (llm_profile_health.profile_id FKs llm_profiles ON
    // DELETE CASCADE, so deleting the profile cleans the health row too).
    let stamp = uniq();
    let conn = d.pool.get().await.expect("pool");
    let profile: i64 = conn
        .query_one(
            "INSERT INTO llm_profiles(name, format, model) VALUES ($1,'openai','m') RETURNING id",
            &[&format!("t-health-{stamp}")],
        )
        .await
        .expect("insert profile")
        .get(0);

    // An empty load (no health rows for this profile).
    let before: Vec<LLMHealth> = d.load_llm_health().await.expect("load empty");
    assert!(
        !before.iter().any(|h| h.profile_id == profile),
        "fresh profile should have no health row"
    );

    // Save an OPEN breaker (cooling-off in the future) → it shows up in the load.
    let open_until = ts_secs(std::time::Duration::from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    ));
    d.save_llm_health(&LLMHealth {
        profile_id: profile,
        fails: 3,
        trips: 2,
        open_until: Some(open_until),
        last_error: "boom".into(),
        last_at: std::time::UNIX_EPOCH,
    })
    .await
    .expect("save open");
    let loaded: Vec<LLMHealth> = d.load_llm_health().await.expect("load open");
    let h = loaded
        .iter()
        .find(|h| h.profile_id == profile)
        .expect("saved open health row is present");
    assert_eq!(h.fails, 3);
    assert_eq!(h.trips, 2);
    assert_eq!(h.last_error, "boom");
    assert!(h.open_until.is_some(), "open_until should be set");

    // Upsert an EXPIRED window → it is filtered out of the load (treated healthy).
    let past = ts_secs(std::time::Duration::from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(3600),
    ));
    d.save_llm_health(&LLMHealth {
        profile_id: profile,
        fails: 0,
        trips: 3,
        open_until: Some(past),
        last_error: "".into(),
        last_at: std::time::UNIX_EPOCH,
    })
    .await
    .expect("save expired");
    let loaded: Vec<LLMHealth> = d.load_llm_health().await.expect("load expired");
    assert!(
        !loaded.iter().any(|h| h.profile_id == profile),
        "expired cooling-off row must be excluded"
    );

    // A NULL open_until is also excluded (never tripped / already closed).
    d.save_llm_health(&LLMHealth {
        profile_id: profile,
        fails: 1,
        trips: 3,
        open_until: None,
        last_error: "".into(),
        last_at: std::time::UNIX_EPOCH,
    })
    .await
    .expect("save null");
    let loaded: Vec<LLMHealth> = d.load_llm_health().await.expect("load null");
    assert!(
        !loaded.iter().any(|h| h.profile_id == profile),
        "NULL open_until row must be excluded"
    );

    // Clear drops the row entirely.
    d.clear_llm_health(profile).await.expect("clear");
    let raw: i64 = conn
        .query_opt(
            "SELECT 1 FROM llm_profile_health WHERE profile_id=$1",
            &[&profile],
        )
        .await
        .expect("raw health")
        .map_or(0, |_| 1);
    assert_eq!(raw, 0, "clear should delete the health row");

    let _ = conn
        .execute("DELETE FROM llm_profiles WHERE id=$1", &[&profile])
        .await;
}

// --- skill_usage / skill_gaps ------------------------------------------------

#[tokio::test]
async fn skill_gaps_aggregates_misses_and_honours_cancellation() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let skill = format!("t-gap-skill-{stamp}");
    let other = format!("t-gap-other-{stamp}");
    let agent = format!("t-gap-agent-{stamp}");

    // Two misses + one hit for `skill` (task_id NULL → category_id 0); one miss
    // for `other` (different skill). `skill_usage.ts` is NOT NULL DEFAULT now().
    // Each insert checks out its own pooled connection (Go's `(*sql.DB).Exec`).
    async fn ins(d: &DB, skill: &str, agent: &str, found: bool) {
        d.execute(
            "INSERT INTO skill_usage(skill, agent_key, found) VALUES ($1,$2,$3)",
            &[&skill, &agent, &found],
        )
        .await
        .expect("insert skill_usage");
    }
    ins(&d, &skill, &agent, false).await;
    ins(&d, &skill, &agent, false).await;
    ins(&d, &skill, &agent, true).await;
    ins(&d, &other, &agent, false).await;

    let ctx = Context::background();
    let gaps = d.skill_gaps(&ctx, 0).await.expect("skill_gaps");
    let g = gaps
        .iter()
        .find(|g| g.skill == skill)
        .expect("skill gap present");
    assert_eq!(g.misses, 2, "misses: {g:?}");
    assert_eq!(g.hits, 1, "hits: {g:?}");
    assert_eq!(g.category_id, 0, "NULL task category coalesces to 0");
    assert_eq!(g.agent_key, agent);
    assert!(g.last_seen > 0, "last_seen epoch should be set");

    // A positive limit is respected (cap the result count).
    let capped = d.skill_gaps(&ctx, 1).await.expect("skill_gaps limit 1");
    assert!(
        capped.len() <= 1,
        "limit=1 should cap rows, got {}",
        capped.len()
    );

    // A cancelled context returns the context error (QueryContext semantics).
    let cancelled = Context::canceled();
    let err = d
        .skill_gaps(&cancelled, 0)
        .await
        .expect_err("cancelled context must error");
    assert_eq!(
        err.to_string(),
        "context canceled",
        "cancelled skill_gaps error = {err:?}"
    );

    let _ = d
        .execute("DELETE FROM skill_usage WHERE agent_key=$1", &[&agent])
        .await;
}

// --- constants (compile-time guard) -----------------------------------------

#[test]
fn exploration_constants_match_go() {
    use artex::db::constants::*;
    assert_eq!(KIND_BEGIN, "begin");
    assert_eq!(KIND_GOAL, "goal");
    assert_eq!(KIND_INTENT, "intent");
    assert_eq!(KIND_FACT, "fact");
    assert_eq!(KIND_FINDING, "finding");
    assert_eq!(KIND_HINT, "hint");
    assert_eq!(KIND_DIGEST, "digest");
    assert_eq!(STATE_DIGEST_ACTIVE, "active");
    assert_eq!(STATE_DIGEST_SUPERSEDED, "superseded");
    assert_eq!(STATE_ORIGIN, "origin");
    assert_eq!(STATE_INTENT_DELETED, "deleted");
    assert_eq!(REL_SPAWNS, "spawns");
    assert_eq!(REL_DERIVED_FROM, "derived_from");
    assert_eq!(REL_YIELDS, "yields");
    assert_eq!(REL_PROVES, "proves");
    assert_eq!(REL_COVERS, "covers");
}
