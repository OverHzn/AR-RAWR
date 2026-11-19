//! Differential tests for the task-template store — port of
//! `db/task_templates_test.go` (`TestTaskTemplateCRUDAndNormalizedUniqueness`,
//! `TestTaskTemplateDisjointPatchesCompose`).
//!
//! Runs against the live Postgres from `ARTEX_PG_DSN` (the same image the Go
//! oracle uses), serialised behind one shared async lock, and skips cleanly when
//! the DSN is unset or unreachable. Each run carves out a process-unique name so
//! a leftover row from a crashed earlier run never collides.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::task_templates::{
    TaskTemplateInput, TaskTemplatePatch, ERR_TASK_TEMPLATE_INVALID,
    ERR_TASK_TEMPLATE_NAME_CONFLICT,
};
use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A process-unique suffix (nanos + a counter), so re-runs never collide.
fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos + (N.fetch_add(1, Ordering::Relaxed) as u128) * 7919
}

fn input(name: &str, description: &str, goal: &str) -> TaskTemplateInput {
    TaskTemplateInput {
        name: name.to_string(),
        description: description.to_string(),
        goal: goal.to_string(),
        ..Default::default()
    }
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
async fn task_template_crud_and_normalized_uniqueness() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let suffix = uniq();
    let name = format!("Template {suffix}");
    // Whitespace scrambled + padded, to prove normalization collapses it.
    let messy_name = format!("  {}  ", name.replace(' ', "   "));

    let created = d
        .create_task_template(input(
            &messy_name,
            "  initial description  ",
            "  initial goal  ",
        ))
        .await
        .expect("create");
    // Cleanup even on a later panic.
    let cleanup_id = created.id;

    assert_eq!(created.name, name, "name was not normalized: {created:?}");
    assert_eq!(created.description, "initial description");
    assert_eq!(created.goal, "initial goal");

    // Case-insensitive name collision via the unique nkey index.
    let upper = gocompat::strings::go_to_upper(&name);
    let err = d
        .create_task_template(input(&upper, "duplicate", "duplicate"))
        .await
        .expect_err("uppercased duplicate must conflict");
    assert!(
        err.is(ERR_TASK_TEMPLATE_NAME_CONFLICT),
        "duplicate create error = {err:?}, want name conflict"
    );

    let got = d.get_task_template(created.id).await.expect("get");
    let got = got.expect("GetTaskTemplate should find the row");
    assert_eq!(got.name, name);

    let listed = d.list_task_templates().await.expect("list");
    assert!(
        listed.iter().any(|t| t.id == created.id),
        "created template {} missing from list",
        created.id
    );

    let updated_name = format!("{name} updated");
    let updated = d
        .update_task_template(
            created.id,
            input(&updated_name, "new description", "new goal"),
        )
        .await
        .expect("update");
    assert_eq!(updated.name, updated_name);
    assert_eq!(updated.description, "new description");
    assert_eq!(updated.goal, "new goal");

    // Empty name is invalid.
    let err = d
        .create_task_template(input("", "x", "y"))
        .await
        .expect_err("empty name must be invalid");
    assert!(
        err.is(ERR_TASK_TEMPLATE_INVALID),
        "empty name error = {err:?}, want invalid"
    );

    let deleted = d.delete_task_template(created.id).await.expect("delete");
    assert!(deleted, "first delete should report existed");
    let deleted = d
        .delete_task_template(created.id)
        .await
        .expect("delete again");
    assert!(!deleted, "second delete should report absent");

    // Defensive cleanup of any partial state.
    let _ = d.delete_task_template(cleanup_id).await;
}

#[tokio::test]
async fn task_template_disjoint_patches_compose() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let name = format!("Concurrent template {}", uniq());
    let created = d
        .create_task_template(input(&name, "initial description", "initial goal"))
        .await
        .expect("create");
    let id = created.id;

    // Two disjoint patches (description-only, goal-only) race; a single CASE-based
    // UPDATE per patch must compose rather than clobber.
    let description = "description from concurrent patch".to_string();
    let goal = "goal from concurrent patch".to_string();
    let p_desc = TaskTemplatePatch {
        description: Some(description.clone()),
        ..Default::default()
    };
    let p_goal = TaskTemplatePatch {
        goal: Some(goal.clone()),
        ..Default::default()
    };
    let (r1, r2) = tokio::join!(
        d.patch_task_template(id, p_desc),
        d.patch_task_template(id, p_goal),
    );
    r1.expect("description patch");
    r2.expect("goal patch");

    let got = d.get_task_template(id).await.expect("get").expect("row");
    assert_eq!(
        got.description, description,
        "disjoint patches lost the description update: {got:?}"
    );
    assert_eq!(
        got.goal, goal,
        "disjoint patches lost the goal update: {got:?}"
    );

    let _ = d.delete_task_template(id).await;
}
