//! Differential tests for the task read/metadata slice of `db/tasks.go`
//! (`taskCols` + `scanTask` + `TaskPatch` + `GetTask` / `ListTasks` /
//! `UpdateTask`). `CreateTask*` / `DeleteTask*` are later slices, so tasks are
//! seeded via raw SQL here; hydration is checked against the D-3d context layer.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Each task gets its own exploration (deleted after).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use artex::db::tasks::{Task, TaskPatch};
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

/// Create an exploration + task; return (task_id, exp_id).
async fn make_task(d: &DB, stamp: u128) -> (i64, i64) {
    let exp_id = d
        .create_exploration("desc", &format!("t-read-{stamp}"))
        .await
        .expect("exploration");
    let row = d
        .query_one(
            "INSERT INTO tasks(exploration_id, description, goal, name) \
             VALUES ($1, 'the-desc', 'the-goal', $2) RETURNING id",
            &[&exp_id, &format!("name-{stamp}")],
        )
        .await
        .expect("task");
    (row.get::<_, i64>(0), exp_id)
}

async fn cleanup(d: &DB, task_id: i64, exp_id: i64) {
    let _ = d
        .execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await;
    let _ = d
        .execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await;
}

#[tokio::test]
async fn get_task_scan_and_hydrate() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    // Seed a category and point the task at it.
    let cat = d
        .query_one(
            "INSERT INTO task_categories(name, nkey) VALUES ($1,$2) RETURNING id",
            &[&format!("Cat {stamp}"), &format!("cat-{stamp}")],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    let profile = d
        .query_one(
            "INSERT INTO llm_profiles(name, format, model) VALUES ($1,'openai','m') RETURNING id",
            &[&format!("read-prof-{stamp}")],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    let (task_id, exp_id) = make_task(&d, stamp).await;
    // Fill the columns scanTask reads: category, timeout/heartbeat, pinned, parent.
    d.execute(
        "UPDATE tasks SET category_id=$2, timeout_seconds=120, \
         plan_heartbeat_seconds=900, pinned_at=now(), parent_ref='parent-9', \
         active_llm_profile_id=$3, llm_profile_id=$3 WHERE id=$1",
        &[&task_id, &cat, &profile],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO task_llm_profiles(task_id, profile_id, position) VALUES ($1,$2,0)",
        &[&task_id, &profile],
    )
    .await
    .unwrap();

    let t = d.get_task(task_id).await.expect("get").expect("some");
    assert_eq!(t.id, task_id);
    assert_eq!(t.name, format!("name-{stamp}"));
    assert_eq!(t.description, "the-desc");
    assert_eq!(t.goal, "the-goal");
    assert_eq!(t.exploration_id, exp_id);
    assert_eq!(t.status, "created", "default status");
    assert_eq!(t.category_id, Some(cat));
    assert_eq!(
        t.category_name,
        format!("Cat {stamp}"),
        "category name joined"
    );
    assert_eq!(t.timeout_seconds, 120);
    assert_eq!(t.plan_heartbeat_seconds, 900);
    assert_eq!(t.parent_ref, "parent-9");
    assert!(
        t.pinned && t.pinned_at.is_some(),
        "pinned derived from pinned_at"
    );
    // Hydrated from the D-3d context layer.
    assert_eq!(t.llm_profile_ids, vec![profile]);
    assert_eq!(t.active_llm_profile_id, Some(profile));
    assert_eq!(t.llm_failover_state, "ready");

    // Missing id -> None.
    assert!(d.get_task(99_999_999).await.expect("get missing").is_none());

    // Soft-deleted task -> None.
    d.execute("UPDATE tasks SET deleted_at=now() WHERE id=$1", &[&task_id])
        .await
        .unwrap();
    assert!(d.get_task(task_id).await.expect("get deleted").is_none());
    d.execute("UPDATE tasks SET deleted_at=NULL WHERE id=$1", &[&task_id])
        .await
        .unwrap();

    cleanup(&d, task_id, exp_id).await;
    let _ = d
        .execute("DELETE FROM task_categories WHERE id=$1", &[&cat])
        .await;
    let _ = d
        .execute("DELETE FROM llm_profiles WHERE id=$1", &[&profile])
        .await;
}

#[tokio::test]
async fn list_tasks_order_and_pinning() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let mut made = Vec::new();
    for _ in 0..3 {
        made.push(make_task(&d, uniq()).await);
    }
    let (t1, e1) = made[0];
    let (t2, e2) = made[1];
    let (t3, e3) = made[2];

    let list = d.list_tasks().await.expect("list");
    let mine: std::collections::HashSet<i64> = [t1, t2, t3].into_iter().collect();
    let seen: Vec<i64> = list
        .iter()
        .map(|t| t.id)
        .filter(|id| mine.contains(id))
        .collect();
    assert_eq!(seen.len(), 3, "all created tasks present");
    // Unpinned -> id DESC.
    assert_eq!(seen, vec![t3, t2, t1], "newest id first when unpinned");

    // Pin t1 -> it sorts ahead of the others (pinned_at DESC NULLS LAST).
    d.execute("UPDATE tasks SET pinned_at=now() WHERE id=$1", &[&t1])
        .await
        .unwrap();
    let list = d.list_tasks().await.expect("list pinned");
    let seen: Vec<i64> = list
        .iter()
        .map(|t| t.id)
        .filter(|id| mine.contains(id))
        .collect();
    assert_eq!(seen[0], t1, "pinned task sorts first");
    assert_eq!(&seen[1..], &[t3, t2], "rest still id desc");

    for (id, e) in [(t1, e1), (t2, e2), (t3, e3)] {
        cleanup(&d, id, e).await;
    }
}

#[tokio::test]
async fn update_task_patch_tristate() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let (task_id, exp_id) = make_task(&d, uniq()).await;

    // No-op patch leaves every field unchanged.
    let t = d
        .update_task(task_id, TaskPatch::default())
        .await
        .expect("noop")
        .expect("some");
    assert!(!t.pinned, "still unpinned");
    assert!(t.plan_quiet_mode, "default quiet-mode on");
    assert!(!t.target_gate, "default gate off");

    // Name + quiet-mode off + gate on.
    let t = d
        .update_task(
            task_id,
            TaskPatch {
                name: Some("renamed".to_string()),
                plan_quiet_mode: Some(false),
                target_gate: Some(true),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("patch")
        .expect("some");
    assert_eq!(t.name, "renamed");
    assert!(!t.plan_quiet_mode);
    assert!(t.target_gate);

    // Pin (Some(true)) -> pinned_at stamped, pinned derived true.
    let t = d
        .update_task(
            task_id,
            TaskPatch {
                pinned: Some(true),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("pin")
        .expect("some");
    assert!(t.pinned && t.pinned_at.is_some());
    // Pin again keeps the original stamp (COALESCE).
    let first_stamp = t.pinned_at;
    let t = d
        .update_task(
            task_id,
            TaskPatch {
                pinned: Some(true),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("re-pin")
        .expect("some");
    assert_eq!(t.pinned_at, first_stamp, "re-pin keeps original stamp");

    // Unpin (Some(false)) -> pinned_at NULL.
    let t = d
        .update_task(
            task_id,
            TaskPatch {
                pinned: Some(false),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("unpin")
        .expect("some");
    assert!(!t.pinned && t.pinned_at.is_none());

    // name=None leaves the name unchanged; other fields untouched.
    let t = d
        .update_task(task_id, TaskPatch::default())
        .await
        .expect("noop2")
        .expect("some");
    assert_eq!(t.name, "renamed");
    assert!(t.target_gate, "gate persists across unrelated patch");

    // Missing task -> None.
    assert!(d
        .update_task(99_999_999, TaskPatch::default())
        .await
        .expect("missing")
        .is_none());

    cleanup(&d, task_id, exp_id).await;
}

#[tokio::test]
async fn task_defaults_match_go_zero() {
    // Pure: the Rust `Task::default()` mirrors Go's `&Task{}` zero value used by
    // hydration call sites (epoch created_at, empty strings, zero numbers).
    let t = Task::default();
    assert_eq!(t.id, 0);
    assert_eq!(t.created_at, SystemTime::UNIX_EPOCH);
    assert!(t.name.is_empty() && t.description.is_empty() && t.goal.is_empty());
    assert!(t.llm_profile_ids.is_empty() && t.source_task_ids.is_empty());
    assert_eq!(t.llm_chain_revision, 0);
    assert_eq!(t.timeout_seconds, 0);
    assert!(!t.pinned && !t.paused && !t.queued);
    assert!(t.active_llm_profile_id.is_none() && t.completed_at.is_none());
}
