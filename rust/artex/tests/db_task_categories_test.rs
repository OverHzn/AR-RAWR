//! Differential tests for task categories — port of the category parts of the Go
//! `db/task_categories_test.go`. The Go specs drive `CreateTaskWithOptions`/`GetTask`
//! (tasks group, not yet ported), so this port seeds tasks via raw SQL and verifies
//! assignment through `list_task_categories` task_count + direct `tasks.category_id`
//! reads. Every category/assignment SQL path is still exercised.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Cleans up via the process-unique name prefix + cascade.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::task_categories::{
    ERR_TASK_CATEGORY_INVALID, ERR_TASK_CATEGORY_NAME_CONFLICT, ERR_TASK_CATEGORY_NOT_FOUND,
    ERR_TASK_CATEGORY_TASK_NOT_FOUND, MAX_TASK_CATEGORY_BATCH_SIZE,
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

/// Seed an exploration + task (optionally with a category); return the task id.
async fn seed_task(d: &DB, stamp: u128, suffix: &str, category_id: Option<i64>) -> i64 {
    let goal = format!("t-cat-goal-{stamp}-{suffix}");
    let exp: i64 = d
        .query_one(
            "INSERT INTO explorations(goal) VALUES ($1) RETURNING id",
            &[&goal],
        )
        .await
        .expect("insert exploration")
        .get(0);
    d.query_one(
        "INSERT INTO tasks(description, goal, exploration_id, category_id) VALUES ($1, $1, $2, $3) RETURNING id",
        &[&goal, &exp, &category_id],
    )
    .await
    .expect("insert task")
    .get(0)
}

async fn task_category_id(d: &DB, task_id: i64) -> Option<i64> {
    d.query_opt("SELECT category_id FROM tasks WHERE id=$1", &[&task_id])
        .await
        .expect("read category_id")
        .and_then(|r| r.get::<_, Option<i64>>(0))
}

async fn cleanup_task(d: &DB, task_id: i64) {
    d.execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await
        .expect("clean task");
}

#[tokio::test]
async fn category_crud_and_assignment() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let name = format!("Category {stamp}");
    let messy = format!("  {}  ", name.replace(' ', "   "));

    // Create normalizes whitespace.
    let category = d.create_task_category(&messy).await.expect("create");
    assert_eq!(category.name, name, "whitespace collapsed");
    assert_eq!(category.task_count, 0, "new category has no tasks");

    // Duplicate (case-insensitive nkey) -> NameConflict.
    let dup = d.create_task_category(&name.to_uppercase()).await;
    assert!(dup.is_err(), "duplicate name must error");
    assert!(
        dup.unwrap_err().is(ERR_TASK_CATEGORY_NAME_CONFLICT),
        "duplicate error carries NameConflict sentinel"
    );

    // Empty / overlong names -> Invalid.
    assert!(d
        .create_task_category("   ")
        .await
        .unwrap_err()
        .is(ERR_TASK_CATEGORY_INVALID));
    let long = "x".repeat(81);
    assert!(d
        .create_task_category(&long)
        .await
        .unwrap_err()
        .is(ERR_TASK_CATEGORY_INVALID));

    // List + Get.
    let listed = d.list_task_categories().await.expect("list");
    assert!(
        listed.iter().any(|c| c.id == category.id),
        "created category listed"
    );
    let got = d
        .get_task_category(category.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.name, name);
    assert!(d
        .get_task_category(i64::MAX)
        .await
        .expect("get missing")
        .is_none());

    // Seed a task and assign it.
    let task = seed_task(&d, stamp, "a", None).await;
    // NB: SetTaskCategory's returned task_count is same-snapshot (the count subquery
    // runs against the pre-UPDATE snapshot in the same statement), matching Go; the
    // authoritative count is read by a fresh query (get_task_category / list).
    let _assigned = d
        .set_task_category(task, Some(category.id))
        .await
        .expect("assign")
        .expect("assign returns category");
    assert_eq!(task_category_id(&d, task).await, Some(category.id));
    let fresh = d
        .get_task_category(category.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(
        fresh.task_count, 1,
        "fresh task_count reflects the assignment"
    );

    // Rename: task_count stays 1, name changes.
    let renamed = d
        .rename_task_category(category.id, &format!("{name} renamed"))
        .await
        .expect("rename");
    assert_eq!(renamed.task_count, 1, "task_count after rename");
    assert_eq!(renamed.name, format!("{name} renamed"));
    // Rename to an existing name -> NameConflict.
    let other = d
        .create_task_category(&format!("Other {stamp}"))
        .await
        .expect("create other");
    // Conflict against the (renamed) category's CURRENT name/nkey.
    let conflict = d
        .rename_task_category(other.id, &format!("{name} renamed"))
        .await;
    assert!(
        conflict.is_err() && conflict.unwrap_err().is(ERR_TASK_CATEGORY_NAME_CONFLICT),
        "rename conflict"
    );
    d.delete_task_category(other.id)
        .await
        .expect("delete other");

    // Clear assignment.
    d.set_task_category(task, None).await.expect("clear");
    assert_eq!(task_category_id(&d, task).await, None, "category cleared");

    // Re-assign then delete the category: the task falls back to uncategorized.
    d.set_task_category(task, Some(category.id))
        .await
        .expect("reassign");
    let deleted = d
        .delete_task_category(category.id)
        .await
        .expect("delete category");
    assert!(deleted, "delete returned true");
    assert_eq!(
        task_category_id(&d, task).await,
        None,
        "FK SET NULL cleared the task"
    );

    cleanup_task(&d, task).await;
}

#[tokio::test]
async fn set_tasks_category_batch() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let category = d
        .create_task_category(&format!("Batch {stamp}"))
        .await
        .expect("create");

    let t1 = seed_task(&d, stamp, "b1", None).await;
    let t2 = seed_task(&d, stamp, "b2", None).await;
    let t3 = seed_task(&d, stamp, "b3", None).await;
    let ids = vec![t1, t2, t3];

    // Move with a missing id appended: the live ids move, the missing one is absent.
    let missing = 1_i64 << 62;
    let mut input = ids.clone();
    input.push(missing);
    let (updated, moved) = d
        .set_tasks_category(input, Some(category.id))
        .await
        .expect("batch move");
    assert_eq!(updated.len(), ids.len(), "only live ids updated");
    assert!(!updated.contains(&missing), "missing id absent from result");
    let moved = moved.expect("category returned");
    assert_eq!(
        moved.task_count,
        ids.len() as i64,
        "task_count reflects the batch"
    );
    for id in &ids {
        assert_eq!(
            task_category_id(&d, *id).await,
            Some(category.id),
            "task {id} moved"
        );
    }

    // A nil category clears the whole batch; no category returned.
    let (cleared, moved) = d
        .set_tasks_category(ids.clone(), None)
        .await
        .expect("batch clear");
    assert_eq!(cleared.len(), ids.len(), "all cleared");
    assert!(moved.is_none(), "nil category returns no category");
    for id in &ids {
        assert_eq!(
            task_category_id(&d, *id).await,
            None,
            "task {id} uncategorized"
        );
    }

    // Missing category -> NotFound (transaction aborts, nothing changes).
    let missing_cat = 1_i64 << 62;
    let err = d
        .set_tasks_category(ids.clone(), Some(missing_cat))
        .await
        .unwrap_err();
    assert!(
        err.is(ERR_TASK_CATEGORY_NOT_FOUND),
        "missing category -> NotFound: {err:?}"
    );
    for id in &ids {
        assert_eq!(
            task_category_id(&d, *id).await,
            None,
            "rejected move left task {id} untouched"
        );
    }

    // Empty ids -> Invalid.
    assert!(d
        .set_tasks_category(vec![], None)
        .await
        .unwrap_err()
        .is(ERR_TASK_CATEGORY_INVALID));
    // Oversized -> Invalid.
    let oversized = vec![t1; MAX_TASK_CATEGORY_BATCH_SIZE + 1];
    assert!(d
        .set_tasks_category(oversized, None)
        .await
        .unwrap_err()
        .is(ERR_TASK_CATEGORY_INVALID));
    // Non-positive id -> Invalid.
    assert!(d
        .set_tasks_category(vec![0], None)
        .await
        .unwrap_err()
        .is(ERR_TASK_CATEGORY_INVALID));

    // SetTaskCategory on a missing task -> TaskNotFound.
    let err = d
        .set_task_category(missing, Some(category.id))
        .await
        .unwrap_err();
    assert!(
        err.is(ERR_TASK_CATEGORY_TASK_NOT_FOUND),
        "missing task -> TaskNotFound: {err:?}"
    );
    // SetTaskCategory to a missing category on a live task -> CategoryNotFound.
    let live = seed_task(&d, stamp, "live", None).await;
    let err = d
        .set_task_category(live, Some(missing_cat))
        .await
        .unwrap_err();
    assert!(
        err.is(ERR_TASK_CATEGORY_NOT_FOUND),
        "missing category -> CategoryNotFound: {err:?}"
    );

    for id in &ids {
        cleanup_task(&d, *id).await;
    }
    cleanup_task(&d, live).await;
    d.delete_task_category(category.id)
        .await
        .expect("delete category");
}
