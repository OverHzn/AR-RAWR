//! Differential tests for the task-registry foundation + lifecycle slice of
//! `db/tasks.go` (`IsTerminal` / `NormalizeTaskCompanyIDs` / `normalizeHeartbeat`
//! and the lifecycle mutators `SetParentRef` / `SetPaused` / `Enqueue` /
//! `Dequeue` / `SetQueued` / `SetStatus` / `SetTerminalStatusGuarded` /
//! `ClearRunEndReason` / `StampFirstRun`). The `Task` row type + Create/Get/List
//! land with the hydrate slice.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Each task gets its own exploration (deleted after).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use artex::db::tasks::{
    is_terminal, normalize_heartbeat, normalize_task_company_ids, ERR_TASK_COMPANY_IDS_INVALID,
    MAX_TASK_COMPANY_COUNT, MIN_PLAN_HEARTBEAT_SECONDS,
};
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

async fn make_task(d: &DB, stamp: u128) -> i64 {
    let exp_id = d
        .create_exploration("desc", &format!("t-task-{stamp}"))
        .await
        .expect("exploration");
    let row = d
        .query_one(
            "INSERT INTO tasks(exploration_id, description, goal) VALUES ($1,'','') RETURNING id",
            &[&exp_id],
        )
        .await
        .expect("task");
    row.get::<_, i64>(0)
}

async fn cleanup(d: &DB, task_id: i64) {
    let exp = d
        .query_opt("SELECT exploration_id FROM tasks WHERE id=$1", &[&task_id])
        .await
        .ok()
        .flatten();
    let _ = d
        .execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await;
    if let Some(r) = exp {
        let exp_id = r.get::<_, i64>(0);
        let _ = d
            .execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
            .await;
    }
}

#[tokio::test]
async fn task_registry_lifecycle() {
    // ---- Pure helpers (no DB).
    assert!(is_terminal("done") && is_terminal("failed") && is_terminal("timeout"));
    assert!(!is_terminal("running") && !is_terminal("created") && !is_terminal("paused"));

    assert_eq!(normalize_heartbeat(0), MIN_PLAN_HEARTBEAT_SECONDS);
    assert_eq!(normalize_heartbeat(60), MIN_PLAN_HEARTBEAT_SECONDS);
    assert_eq!(normalize_heartbeat(900), 900);

    assert_eq!(normalize_task_company_ids(&[]).unwrap(), Vec::<i64>::new());
    assert_eq!(
        normalize_task_company_ids(&[3, 1, 3, 2, 1]).unwrap(),
        vec![3, 1, 2],
        "dedup keeps first-seen order"
    );
    let bad = normalize_task_company_ids(&[0]).unwrap_err();
    assert!(
        bad.is(ERR_TASK_COMPANY_IDS_INVALID),
        "non-positive id rejected"
    );
    let too_many: Vec<i64> = (1..=(MAX_TASK_COMPANY_COUNT as i64 + 1)).collect();
    let err = normalize_task_company_ids(&too_many).unwrap_err();
    assert!(
        err.is(ERR_TASK_COMPANY_IDS_INVALID),
        "over company cap rejected"
    );

    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // ---- SetParentRef: set then clear (NULLIF '').
    let t = make_task(&d, uniq()).await;
    d.set_parent_ref(t, "parent-123").await.expect("set parent");
    let parent = d
        .query_one(
            "SELECT COALESCE(parent_ref,'') FROM tasks WHERE id=$1",
            &[&t],
        )
        .await
        .unwrap()
        .get::<_, String>(0);
    assert_eq!(parent, "parent-123");
    d.set_parent_ref(t, "").await.expect("clear parent");
    let parent = d
        .query_one("SELECT parent_ref FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap()
        .get::<_, Option<String>>(0);
    assert!(parent.is_none(), "empty parent ref clears to NULL");
    cleanup(&d, t).await;

    // ---- SetPaused: toggle.
    let t = make_task(&d, uniq()).await;
    d.set_paused(t, true).await.expect("pause");
    assert!(d
        .query_one("SELECT paused FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap()
        .get::<_, bool>(0));
    d.set_paused(t, false).await.expect("unpause");
    assert!(!d
        .query_one("SELECT paused FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap()
        .get::<_, bool>(0));
    cleanup(&d, t).await;

    // ---- Enqueue / Dequeue / SetQueued.
    let t = make_task(&d, uniq()).await;
    assert!(
        d.enqueue(t, "bogus").await.is_err(),
        "invalid queue mode rejected"
    );
    d.enqueue(t, "bootstrap").await.expect("enqueue bootstrap");
    let (queued, mode) = d
        .query_one("SELECT queued, queue_mode FROM tasks WHERE id=$1", &[&t])
        .await
        .map(|r| (r.get::<_, bool>(0), r.get::<_, String>(1)))
        .unwrap();
    assert!(queued && mode == "bootstrap");
    d.dequeue(t, true).await.expect("dequeue clear");
    let (queued, mode) = d
        .query_one("SELECT queued, queue_mode FROM tasks WHERE id=$1", &[&t])
        .await
        .map(|r| (r.get::<_, bool>(0), r.get::<_, String>(1)))
        .unwrap();
    assert!(!queued && mode.is_empty(), "dequeue clears mode");
    // set_queued(true) uses bootstrap.
    d.set_queued(t, true).await.expect("set_queued true");
    let mode = d
        .query_one("SELECT queue_mode FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap()
        .get::<_, String>(0);
    assert_eq!(mode, "bootstrap");
    cleanup(&d, t).await;

    // ---- SetStatus: terminal stamps completed_at; back to running clears it.
    let t = make_task(&d, uniq()).await;
    d.set_status(t, "done").await.expect("set done");
    let row = d
        .query_one("SELECT status, completed_at FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "done");
    assert!(
        row.get::<_, Option<SystemTime>>(1).is_some(),
        "done stamps completed_at"
    );
    d.set_status(t, "running").await.expect("revive");
    let row = d
        .query_one("SELECT status, completed_at FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "running");
    assert!(
        row.get::<_, Option<SystemTime>>(1).is_none(),
        "revive clears completed_at"
    );
    cleanup(&d, t).await;

    // ---- SetTerminalStatusGuarded: first writer wins.
    let t = make_task(&d, uniq()).await;
    assert!(d
        .set_terminal_status_guarded(t, "done", "")
        .await
        .expect("win"));
    assert!(
        !d.set_terminal_status_guarded(t, "timeout", "budget")
            .await
            .expect("lose"),
        "second terminal write loses"
    );
    let row = d
        .query_one("SELECT status, end_reason FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap();
    assert_eq!(
        row.get::<_, String>(0),
        "done",
        "first terminal status sticks"
    );
    assert_eq!(
        row.get::<_, String>(1),
        "",
        "losing writer stores no end_reason"
    );
    cleanup(&d, t).await;

    // ---- ClearRunEndReason: running clears; terminal keeps.
    let t = make_task(&d, uniq()).await;
    d.execute(
        "UPDATE tasks SET status='running', end_reason='budget' WHERE id=$1",
        &[&t],
    )
    .await
    .unwrap();
    d.clear_run_end_reason(t).await.expect("clear running");
    assert_eq!(
        d.query_one("SELECT end_reason FROM tasks WHERE id=$1", &[&t])
            .await
            .unwrap()
            .get::<_, String>(0),
        "",
        "running end_reason cleared"
    );
    d.execute(
        "UPDATE tasks SET status='done', end_reason='budget' WHERE id=$1",
        &[&t],
    )
    .await
    .unwrap();
    d.clear_run_end_reason(t)
        .await
        .expect("clear terminal (no-op)");
    assert_eq!(
        d.query_one("SELECT end_reason FROM tasks WHERE id=$1", &[&t])
            .await
            .unwrap()
            .get::<_, String>(0),
        "budget",
        "terminal end_reason kept"
    );
    cleanup(&d, t).await;

    // ---- StampFirstRun: stamps deadline ~ now+timeout; idempotent; <=0 -> None.
    let t = make_task(&d, uniq()).await;
    let dl1 = d
        .stamp_first_run(t, 60)
        .await
        .expect("stamp 60")
        .expect("deadline set");
    let first_run = d
        .query_one("SELECT first_run_at FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap()
        .get::<_, Option<SystemTime>>(0);
    assert!(first_run.is_some(), "first_run_at stamped");
    // deadline ~ 60s in the future (allow generous slack for slow CI).
    let now = SystemTime::now();
    let delta = dl1.duration_since(now).unwrap_or_default().as_secs();
    assert!(
        (45..=120).contains(&delta),
        "deadline ~60s ahead, got {delta}s"
    );
    // Second stamp with a different timeout keeps the original clock.
    let dl2 = d
        .stamp_first_run(t, 999)
        .await
        .expect("stamp 999")
        .expect("deadline");
    assert_eq!(dl1, dl2, "idempotent: original deadline preserved");
    cleanup(&d, t).await;

    // timeout <= 0 leaves deadline NULL (but still stamps first_run_at).
    let t = make_task(&d, uniq()).await;
    let dl = d.stamp_first_run(t, 0).await.expect("stamp 0");
    assert!(dl.is_none(), "non-positive timeout -> no deadline");
    assert!(d
        .query_one("SELECT first_run_at FROM tasks WHERE id=$1", &[&t])
        .await
        .unwrap()
        .get::<_, Option<SystemTime>>(0)
        .is_some());
    cleanup(&d, t).await;
}
