//! Differential tests for the DB-level exploration aggregates — fresh spec for the
//! `*DB` exploration stat methods in `db/exploration.go` (ExplorationDiag /
//! TokenTotalsAll / LastActivityAll / GoalCountsAll).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up its own rows (tasks before explorations,
//! due to the RESTRICT FK).

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::GoalCounts;
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

#[allow(clippy::too_many_arguments)]
async fn seed_result(
    d: &DB,
    exp_id: i64,
    worker: &str,
    input: i32,
    output: i32,
    cache_read: i32,
    cache_write: i32,
    created_at: &str,
) {
    let at = artex::db::gotime::parse_rfc3339(created_at).expect("parse rfc3339");
    d.execute(
        "INSERT INTO activity(exploration_id, worker, kind, summary, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, created_at) \
         VALUES ($1,$2,'result','seed',$3,$4,$5,$6,$7)",
        &[
            &exp_id,
            &worker,
            &input,
            &output,
            &cache_read,
            &cache_write,
            &at,
        ],
    )
    .await
    .expect("seed result");
}

#[tokio::test]
async fn exploration_db_aggregates() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let e1 = d
        .create_exploration("d1", &format!("g-{stamp}-1"))
        .await
        .expect("e1");
    let e2 = d
        .create_exploration("d2", &format!("g-{stamp}-2"))
        .await
        .expect("e2");
    let store1 = d.exploration(e1);
    let store2 = d.exploration(e2);

    // ---- Goals: e1 has 2 goals (1 met), e2 has 1 goal (0 met).
    let g1a = store1
        .add_goal(&json!({"text": "a"}), "p")
        .await
        .expect("g1a");
    let _g1b = store1
        .add_goal(&json!({"text": "b"}), "p")
        .await
        .expect("g1b");
    let _g2a = store2
        .add_goal(&json!({"text": "c"}), "p")
        .await
        .expect("g2a");
    store1.set_node_state(g1a, "met").await.expect("met g1a");

    // ---- Token results + activity timestamps.
    seed_result(&d, e1, "mainagent", 100, 50, 10, 5, "2024-01-01T00:00:00Z").await;
    seed_result(&d, e1, "w1", 7, 7, 7, 7, "2024-06-01T12:00:00Z").await;
    seed_result(&d, e2, "planner", 3, 3, 3, 3, "2024-03-01T00:00:00Z").await;

    // ---- GoalCountsAll: per-exploration totals.
    let gc = d.goal_counts_all().await.expect("goal counts all");
    assert_eq!(gc.get(&e1), Some(&GoalCounts { total: 2, met: 1 }));
    assert_eq!(gc.get(&e2), Some(&GoalCounts { total: 1, met: 0 }));

    // ---- TokenTotalsAll: per-exploration token sums.
    let tt = d.token_totals_all().await.expect("token totals all");
    let t1 = tt.get(&e1).expect("e1 tokens");
    assert_eq!(
        (t1.input, t1.output, t1.cache_read, t1.cache_write),
        (107, 57, 17, 12)
    );
    let t2 = tt.get(&e2).expect("e2 tokens");
    assert_eq!(
        (t2.input, t2.output, t2.cache_read, t2.cache_write),
        (3, 3, 3, 3)
    );

    // ---- LastActivityAll: per-exploration max created_at epoch.
    let la = d.last_activity_all().await.expect("last activity all");
    // e1 latest is 2024-06-01T12:00:00Z, e2 latest is 2024-03-01T00:00:00Z.
    let e1_ts = la.get(&e1).copied().expect("e1 last");
    let e2_ts = la.get(&e2).copied().expect("e2 last");
    assert_eq!(e1_ts, 1_717_243_200, "e1 last activity epoch");
    assert_eq!(e2_ts, 1_709_251_200, "e2 last activity epoch");
    assert!(e1_ts > e2_ts);

    // ---- ExplorationDiag: live exploration with a task ref vs a missing one.
    d.execute(
        "INSERT INTO tasks(description, goal, exploration_id, name) \
         VALUES ('desc','goal',$1,$2)",
        &[&e1, &format!("task-{stamp}")],
    )
    .await
    .expect("task e1");

    let (exists, task_refs, max_id) = d.exploration_diag(e1).await.expect("diag e1");
    assert!(exists, "e1 exists");
    assert_eq!(task_refs, 1, "e1 has one task ref");
    assert!(max_id >= e2, "max exploration id at least e2");
    assert!(max_id >= e1);

    let missing = 9_000_000_000_i64 + (stamp as i64);
    let (exists, task_refs, _) = d.exploration_diag(missing).await.expect("diag missing");
    assert!(!exists, "missing exploration does not exist");
    assert_eq!(task_refs, 0, "missing exploration has no task refs");

    // Cleanup: tasks first (RESTRICT), then explorations.
    d.execute("DELETE FROM tasks WHERE exploration_id=$1", &[&e1])
        .await
        .expect("del task e1");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&e1])
        .await
        .expect("del e1");
    d.execute("DELETE FROM explorations WHERE id=$1", &[&e2])
        .await
        .expect("del e2");
}
