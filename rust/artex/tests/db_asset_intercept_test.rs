//! Differential tests for the global asset intercept rule CRUD — fresh spec for
//! `db/asset_intercept.go` (the Go package ships no `_test.go` for the CRUD; only
//! the matching logic has one).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Uses a process-unique pattern prefix and cleans up.

use std::sync::atomic::{AtomicU64, Ordering};

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

#[tokio::test]
async fn asset_intercept_rule_crud() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let pattern = format!("t-aix-{stamp}.example.com");

    // Create (user rule, builtin=false, action empty).
    let created = d
        .create_asset_intercept_rule("exact_domain", &pattern, "a note", true)
        .await
        .expect("create");
    assert!(!created.builtin, "user rule must not be builtin");
    assert_eq!(created.kind, "exact_domain");
    assert_eq!(created.pattern, pattern);
    assert_eq!(created.note, "a note");
    assert!(created.enabled, "created enabled");
    assert!(
        created.action.is_empty(),
        "global rule action must be empty"
    );
    let id = created.id;

    // List contains it (built-ins first, then newest first).
    let all = d.list_asset_intercept_rules().await.expect("list");
    assert!(
        all.iter().any(|r| r.id == id),
        "created rule present in list"
    );
    // Newest-first among user rules: our freshly created row should sort by id DESC.
    let user_ids: Vec<i64> = all.iter().filter(|r| !r.builtin).map(|r| r.id).collect();
    if let Some(pos) = user_ids.iter().position(|&x| x == id) {
        // All user ids after `id` in the DESC list must be smaller (none created later here).
        assert!(
            user_ids[pos + 1..].iter().all(|&x| x < id),
            "newest-first ordering broken"
        );
    }

    // Update editable fields.
    let updated = d
        .update_asset_intercept_rule(id, "fuzzy_domain", "newpat", "updated", false)
        .await
        .expect("update");
    assert_eq!(updated.kind, "fuzzy_domain");
    assert_eq!(updated.pattern, "newpat");
    assert_eq!(updated.note, "updated");
    assert!(!updated.enabled, "updated disabled");
    assert!(
        updated.builtin == created.builtin,
        "builtin preserved (false)"
    );

    // Update of a missing id errors (Go QueryRow → ErrNoRows).
    let missing = d
        .update_asset_intercept_rule(i64::MAX, "exact_domain", "x", "", true)
        .await;
    assert!(missing.is_err(), "updating a missing rule must error");

    // Toggle.
    d.toggle_asset_intercept_rule(id, true)
        .await
        .expect("toggle");
    let after = d.list_asset_intercept_rules().await.expect("list2");
    let toggled = after.iter().find(|r| r.id == id).expect("still present");
    assert!(toggled.enabled, "toggled enabled");

    // Delete.
    d.delete_asset_intercept_rule(id).await.expect("delete");
    let after_del = d.list_asset_intercept_rules().await.expect("list3");
    assert!(!after_del.iter().any(|r| r.id == id), "deleted rule gone");

    // Cleanup defensively.
    d.execute(
        "DELETE FROM asset_intercept_rules WHERE pattern=$1",
        &[&pattern],
    )
    .await
    .expect("cleanup");
}
