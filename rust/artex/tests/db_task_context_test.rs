//! Differential tests for the task-context slice of `db/task_context.go`
//! (`TaskLLMProfile`/`TaskSource`/`TaskLLMTransition` + `task_llm_context`/
//! `task_source_ids`/`task_company_ids`/`task_sources`/`task_llm_profiles` +
//! `hydrate_task_context`/`hydrate_tasks_context` + `replace_task_llm_profiles`/
//! `mark_task_llm_profile_quota_exhausted*` + the `ExplorationStore` blocked-reason
//! writers).
//!
//! `CreateTask`/`GetTask`/`DeleteTask`/`SaveProfile`/`DeleteProfile` are not ported
//! yet (the Create/Get/Delete slices), so the Go specs' task/profile fixtures are
//! seeded via raw SQL here; the failover-chain transitions are exercised directly
//! against the ported writers and asserted against the same invariants the Go
//! `TestTaskRelationsAndLLMFailoverChain` checks.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Each task gets its own exploration (deleted after).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use artex::db::task_context::{TaskLLMTransition, INTENT_BLOCKED_LLM_QUOTA};
use artex::db::tasks::Task;
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

/// Create a task (with its exploration) and return (task_id, exp_id).
async fn make_task(d: &DB, stamp: u128) -> (i64, i64) {
    let exp_id = d
        .create_exploration("desc", &format!("t-ctx-{stamp}"))
        .await
        .expect("exploration");
    let row = d
        .query_one(
            "INSERT INTO tasks(exploration_id, description, goal) VALUES ($1,'','') RETURNING id",
            &[&exp_id],
        )
        .await
        .expect("task");
    (row.get::<_, i64>(0), exp_id)
}

/// Seed an `llm_profiles` row via raw SQL; returns its id.
async fn seed_profile(d: &DB, stamp: u128, i: usize) -> i64 {
    let row = d
        .query_one(
            "INSERT INTO llm_profiles(name, format, model, api_key) \
             VALUES ($1, 'openai', $2, 'test-key') RETURNING id",
            &[&format!("ctx-chain-{stamp}-{i}"), &format!("model-{i}")],
        )
        .await
        .expect("profile");
    row.get::<_, i64>(0)
}

async fn cleanup_task(d: &DB, task_id: i64, exp_id: i64) {
    let _ = d
        .execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await;
    let _ = d
        .execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await;
}

async fn cleanup_profiles(d: &DB, ids: &[i64]) {
    for id in ids {
        let _ = d
            .execute("DELETE FROM llm_profiles WHERE id=$1", &[id])
            .await;
    }
}

async fn active_profile(d: &DB, task_id: i64) -> Option<i64> {
    d.query_one(
        "SELECT active_llm_profile_id FROM tasks WHERE id=$1",
        &[&task_id],
    )
    .await
    .unwrap()
    .get::<_, Option<i64>>(0)
}

#[tokio::test]
async fn task_context_failover_chain() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let profile_ids: Vec<i64> =
        futures::future::join_all((0..3).map(|i| seed_profile(&d, stamp, i))).await;
    // The test is keyed to these three; cleanup unconditionally at the end.
    let (task_id, exp_id) = make_task(&d, stamp).await;

    // Seed the initial chain exactly as CreateTaskWithOptions would: insert the
    // ordered rows and set the active cursor to the first profile.
    for (pos, pid) in profile_ids.iter().enumerate() {
        d.execute(
            "INSERT INTO task_llm_profiles(task_id, profile_id, position) VALUES ($1,$2,$3)",
            &[&task_id, pid, &(pos as i32)],
        )
        .await
        .unwrap();
    }
    d.execute(
        "UPDATE tasks SET active_llm_profile_id=$2, llm_profile_id=$2 WHERE id=$1",
        &[&task_id, &profile_ids[0]],
    )
    .await
    .unwrap();

    // hydrate_task_context reflects the seeded chain + active head.
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate");
    assert_eq!(t.llm_profile_ids, profile_ids, "full chain ordered");
    assert_eq!(t.ready_llm_profile_ids, profile_ids, "all ready");
    assert_eq!(t.active_llm_profile_id, Some(profile_ids[0]));
    assert_eq!(t.llm_failover_state, "ready", "active head ready");
    assert!(
        t.llm_failover_reason.is_empty(),
        "no exhausted entry -> no reason"
    );
    let initial_revision = t.llm_chain_revision;

    // ReplaceTaskLLMProfiles bumps the revision.
    d.replace_task_llm_profiles(task_id, &profile_ids, profile_ids[0], None)
        .await
        .expect("replace");
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate");
    assert!(
        t.llm_chain_revision > initial_revision,
        "replacement bumped revision"
    );
    assert_eq!(t.active_llm_profile_id, Some(profile_ids[0]));

    // Stale: a quota failure tagged with the pre-replacement revision must not
    // mutate the replacement chain.
    let stale = d
        .mark_task_llm_profile_quota_exhausted_at_revision(
            task_id,
            profile_ids[0],
            initial_revision,
            "late quota from prior generation",
        )
        .await
        .expect("stale mark");
    assert_eq!(
        stale,
        TaskLLMTransition {
            previous_profile_id: profile_ids[0],
            stale: true,
            ..TaskLLMTransition::default()
        },
        "old revision -> stale, no advance"
    );
    let replacement_revision = {
        let mut t = Task {
            id: task_id,
            ..Task::default()
        };
        d.hydrate_task_context(&mut t).await.expect("hydrate");
        assert_eq!(t.active_llm_profile_id, Some(profile_ids[0]));
        t.llm_chain_revision
    };

    // Advance from the head to the second profile.
    let t1 = d
        .mark_task_llm_profile_quota_exhausted(task_id, profile_ids[0], "insufficient_quota")
        .await
        .expect("mark head");
    assert!(t1.advanced && !t1.chain_exhausted);
    assert_eq!(t1.next_profile_id, Some(profile_ids[1]));
    assert_eq!(active_profile(&d, task_id).await, Some(profile_ids[1]));
    let after_advance = {
        let mut t = Task {
            id: task_id,
            ..Task::default()
        };
        d.hydrate_task_context(&mut t).await.expect("hydrate");
        t.llm_chain_revision
    };
    assert_eq!(
        after_advance,
        replacement_revision + 1,
        "cursor advance bumped revision by exactly 1"
    );

    // Late duplicate from the old head: records exhaustion but does NOT advance
    // past the current cursor.
    let late = d
        .mark_task_llm_profile_quota_exhausted(task_id, profile_ids[0], "late duplicate")
        .await
        .expect("late mark");
    assert!(!late.advanced, "late old-head failure must not advance");
    assert_eq!(late.next_profile_id, Some(profile_ids[1]));

    // Exhaust profile[1] -> advance to profile[2]; then exhaust profile[2] -> chain
    // exhausted, cursor cleared.
    d.mark_task_llm_profile_quota_exhausted(task_id, profile_ids[1], "quota_exceeded")
        .await
        .expect("mark mid");
    let last = d
        .mark_task_llm_profile_quota_exhausted(task_id, profile_ids[2], "insufficient balance")
        .await
        .expect("mark tail");
    assert!(last.advanced && last.chain_exhausted && last.next_profile_id.is_none());
    assert_eq!(active_profile(&d, task_id).await, None, "cursor cleared");
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t)
        .await
        .expect("hydrate exhausted");
    assert_eq!(t.llm_failover_state, "chain_exhausted");
    assert_eq!(
        t.llm_failover_reason, "insufficient balance",
        "latest exhausted reason"
    );

    // Duplicate final failure is idempotent: chain_exhausted, no advance.
    let dup = d
        .mark_task_llm_profile_quota_exhausted(task_id, profile_ids[2], "late final duplicate")
        .await
        .expect("dup tail");
    assert!(!dup.advanced && dup.chain_exhausted && dup.next_profile_id.is_none());

    cleanup_task(&d, task_id, exp_id).await;
    cleanup_profiles(&d, &profile_ids).await;
}

#[tokio::test]
async fn task_context_reset_and_middle_cursor() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let p: Vec<i64> = futures::future::join_all((0..3).map(|i| seed_profile(&d, stamp, i))).await;
    let (task_id, exp_id) = make_task(&d, stamp).await;

    // Reset the chain to [p2, p0, p1] with the active cursor on p0 (the middle of
    // the new ordering), then only candidates *after* p0 are consumed.
    d.replace_task_llm_profiles(task_id, &[p[2], p[0], p[1]], p[0], None)
        .await
        .expect("replace middle");
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate");
    assert_eq!(
        t.llm_profile_ids,
        vec![p[2], p[0], p[1]],
        "chain order as replaced"
    );
    assert_eq!(t.active_llm_profile_id, Some(p[0]));
    assert_eq!(t.llm_failover_state, "ready");

    let step = d
        .mark_task_llm_profile_quota_exhausted(task_id, p[0], "middle quota")
        .await
        .expect("middle mark");
    assert_eq!(
        step.next_profile_id,
        Some(p[1]),
        "middle cursor advances to its successor only"
    );
    let end = d
        .mark_task_llm_profile_quota_exhausted(task_id, p[1], "tail quota")
        .await
        .expect("tail mark");
    assert!(
        end.chain_exhausted,
        "manual chain tail exhausts without wrapping to p2"
    );
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate");
    assert_eq!(
        t.active_llm_profile_id, None,
        "hydration does not revive p2"
    );
    assert_eq!(t.llm_failover_state, "chain_exhausted");

    // Reset clears the failure state back to ready.
    d.replace_task_llm_profiles(task_id, &[p[2], p[0], p[1]], p[0], None)
        .await
        .expect("reset");
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate reset");
    assert_eq!(t.active_llm_profile_id, Some(p[0]));
    assert_eq!(t.llm_failover_state, "ready");
    assert!(
        t.llm_failover_reason.is_empty(),
        "reset cleared failure reason"
    );

    cleanup_task(&d, task_id, exp_id).await;
    cleanup_profiles(&d, &p).await;
}

#[tokio::test]
async fn task_context_validation_and_terminal_edit() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let profile = seed_profile(&d, stamp, 0).await;
    let (task_id, exp_id) = make_task(&d, stamp).await;

    // replace with an active id that is not in the chain -> error, no mutation.
    let bad = d
        .replace_task_llm_profiles(task_id, &[profile], 999_999, None)
        .await;
    assert!(bad.is_err(), "active id outside chain rejected");

    // replace with a non-positive / duplicate id -> error.
    let dup = d
        .replace_task_llm_profiles(task_id, &[profile, profile], profile, None)
        .await;
    assert!(dup.is_err(), "duplicate profile id rejected");

    // Terminal tasks may still edit the chain (the main agent keeps using it).
    d.execute("UPDATE tasks SET status='done' WHERE id=$1", &[&task_id])
        .await
        .unwrap();
    d.replace_task_llm_profiles(task_id, &[profile], profile, None)
        .await
        .expect("terminal edit allowed");
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate");
    assert_eq!(t.llm_profile_ids, vec![profile]);
    assert_eq!(t.active_llm_profile_id, Some(profile));

    // Clearing the chain (empty list) is allowed and resets active to None.
    d.replace_task_llm_profiles(task_id, &[], 0, None)
        .await
        .expect("clear terminal chain");
    let mut t = Task {
        id: task_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t)
        .await
        .expect("hydrate cleared");
    assert!(t.llm_profile_ids.is_empty());
    assert_eq!(t.active_llm_profile_id, None);

    // replace against a missing task -> "task N not found".
    let missing = d
        .replace_task_llm_profiles(99_999_999, &[profile], profile, None)
        .await;
    assert!(missing.is_err(), "missing task rejected");

    cleanup_task(&d, task_id, exp_id).await;
    cleanup_profiles(&d, &[profile]).await;
}

#[tokio::test]
async fn task_context_batch_and_readers() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let profiles: Vec<i64> =
        futures::future::join_all((0..2).map(|i| seed_profile(&d, stamp, i))).await;
    let (child_id, child_exp) = make_task(&d, stamp).await;
    let (src_a, src_a_exp) = make_task(&d, uniq()).await;
    let (src_b, src_b_exp) = make_task(&d, uniq()).await;

    // Wire the child's chain + source relations + a company scope.
    for (pos, pid) in profiles.iter().enumerate() {
        d.execute(
            "INSERT INTO task_llm_profiles(task_id, profile_id, position) VALUES ($1,$2,$3)",
            &[&child_id, pid, &(pos as i32)],
        )
        .await
        .unwrap();
    }
    d.execute(
        "UPDATE tasks SET active_llm_profile_id=$2, llm_profile_id=$2 WHERE id=$1",
        &[&child_id, &profiles[0]],
    )
    .await
    .unwrap();
    d.execute(
        "INSERT INTO task_relations(task_id, source_task_id) VALUES ($1,$2),($1,$3)",
        &[&child_id, &src_a, &src_b],
    )
    .await
    .unwrap();

    let company = d
        .companies()
        .upsert_company(&format!("Ctx Co {stamp}"), "")
        .await
        .expect("company")
        .0;
    d.execute(
        "INSERT INTO task_scope(task_id, kind, company_id, source) \
         VALUES ($1,'company',$2,'manual')",
        &[&child_id, &company],
    )
    .await
    .unwrap();

    // Single-task hydrate picks up sources + company.
    let mut t = Task {
        id: child_id,
        ..Task::default()
    };
    d.hydrate_task_context(&mut t).await.expect("hydrate");
    assert_eq!(
        t.source_task_ids,
        vec![src_a, src_b],
        "sources by created_at,id"
    );
    assert_eq!(t.company_ids, vec![company], "company scope read");

    // Direct readers match.
    assert_eq!(
        d.task_source_ids(child_id).await.unwrap(),
        vec![src_a, src_b]
    );
    assert_eq!(d.task_company_ids(child_id).await.unwrap(), vec![company]);
    let srcs = d.task_sources(child_id).await.unwrap();
    assert_eq!(srcs.len(), 2);
    assert_eq!(srcs[0].task_id, src_a);
    let chain = d.task_llm_profiles(child_id).await.unwrap();
    assert_eq!(
        chain.iter().map(|e| e.profile_id).collect::<Vec<_>>(),
        profiles
    );
    assert!(chain.iter().all(|e| e.status == "ready"));

    // Bulk hydrate produces the same view for a slice of tasks.
    let mut many = vec![
        Task {
            id: child_id,
            ..Task::default()
        },
        Task {
            id: src_a,
            ..Task::default()
        },
    ];
    d.hydrate_tasks_context(&mut many)
        .await
        .expect("bulk hydrate");
    assert_eq!(many[0].source_task_ids, vec![src_a, src_b]);
    assert_eq!(many[0].company_ids, vec![company]);
    assert_eq!(many[0].llm_profile_ids, profiles);
    assert_eq!(many[0].active_llm_profile_id, Some(profiles[0]));
    // src_a has no chain/sources/companies of its own.
    assert!(many[1].llm_profile_ids.is_empty());
    assert!(many[1].source_task_ids.is_empty());

    cleanup_task(&d, child_id, child_exp).await;
    cleanup_task(&d, src_a, src_a_exp).await;
    cleanup_task(&d, src_b, src_b_exp).await;
    cleanup_profiles(&d, &profiles).await;
    let _ = d
        .execute("DELETE FROM companies WHERE id=$1", &[&company])
        .await;
}

#[tokio::test]
async fn task_context_rotate_and_blocked_reason() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let profiles: Vec<i64> =
        futures::future::join_all((0..3).map(|i| seed_profile(&d, stamp, i))).await;
    let (task_id, exp_id) = make_task(&d, stamp).await;

    // llm_rotate=true makes mark pick a ready entry from the head (position 0),
    // even when the current head is p0 at position 1.
    d.replace_task_llm_profiles(task_id, &profiles, profiles[0], Some(true))
        .await
        .expect("replace rotate on");
    // Exhaust p0 (position 0). rotate=true -> next ready from position 0 onwards
    // excluding already-exhausted entries; p1 (position 1) is next.
    let t1 = d
        .mark_task_llm_profile_quota_exhausted(task_id, profiles[0], "rotate quota")
        .await
        .expect("rotate mark 1");
    assert_eq!(t1.next_profile_id, Some(profiles[1]));
    // A stale revision failure leaves the chain untouched.
    let before = active_profile(&d, task_id).await;
    let stale = d
        .mark_task_llm_profile_quota_exhausted_at_revision(task_id, profiles[1], 999_999, "stale")
        .await
        .expect("stale");
    assert!(stale.stale && !stale.advanced);
    assert_eq!(active_profile(&d, task_id).await, before);

    cleanup_task(&d, task_id, exp_id).await;
    cleanup_profiles(&d, &profiles).await;

    // ---- ExplorationStore blocked-reason writers.
    let (task_id, exp_id) = make_task(&d, uniq()).await;
    let exp = d.exploration(exp_id);
    // Seed two intent nodes, one of which we block.
    let n1 = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, state) \
             VALUES ($1,'intent','{}'::jsonb,'open') RETURNING id",
            &[&exp_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let _n2 = d
        .query_one(
            "INSERT INTO exploration_nodes(exploration_id, kind, payload, state) \
             VALUES ($1,'intent','{}'::jsonb,'open') RETURNING id",
            &[&exp_id],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    exp.set_intent_blocked_reason(n1, INTENT_BLOCKED_LLM_QUOTA)
        .await
        .expect("block intent");
    let (state, reason): (String, Option<String>) = {
        let r = d
            .query_one(
                "SELECT state, blocked_reason FROM exploration_nodes WHERE id=$1",
                &[&n1],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(state, "blocked");
    assert_eq!(reason.as_deref(), Some(INTENT_BLOCKED_LLM_QUOTA));

    // Reopen by reason reopens only the blocked intents.
    let n = exp
        .reopen_intents_by_blocked_reason(INTENT_BLOCKED_LLM_QUOTA)
        .await
        .expect("reopen");
    assert_eq!(n, 1, "one intent reopened");
    let state: String = d
        .query_one("SELECT state FROM exploration_nodes WHERE id=$1", &[&n1])
        .await
        .unwrap()
        .get(0);
    assert_eq!(state, "open");
    let reason: Option<String> = d
        .query_one(
            "SELECT blocked_reason FROM exploration_nodes WHERE id=$1",
            &[&n1],
        )
        .await
        .unwrap()
        .get(0);
    assert!(reason.is_none(), "reopen cleared the reason");

    // Clean up nodes then the task.
    d.execute(
        "DELETE FROM exploration_nodes WHERE exploration_id=$1",
        &[&exp_id],
    )
    .await
    .unwrap();
    cleanup_task(&d, task_id, exp_id).await;
}
