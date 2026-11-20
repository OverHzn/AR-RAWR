//! Differential tests for the task-create slice of `db/tasks.go`
//! (`TaskCreateOptions` + `CreateTask` / `CreateTaskWithOptions` and the
//! `insertTaskRelations` / `insertTaskCompanies` / `insertTaskInterceptRules`
//! helpers), plus the origin fact node and company-scope snapshot. The Go specs
//! (`TestTaskRelationsAndLLMFailoverChain`, `TestTaskContextRejectsDuplicates...`,
//! `TestCreateTaskWithCompanyScopes`) drive Create/Get/Assets helpers that are not
//! ported yet, so the fixtures are seeded / asserted via raw SQL here.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use artex::db::task_templates::TaskInterceptRuleInput;
use artex::db::tasks::{TaskCreateOptions, MIN_PLAN_HEARTBEAT_SECONDS};
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

/// Raw-SQL task+exploration create (DeleteTask is not ported yet); returns (id, exp).
async fn seed_task(d: &DB, stamp: u128) -> (i64, i64) {
    let exp = d
        .create_exploration("d", &format!("seed-{stamp}"))
        .await
        .unwrap();
    let id = d
        .query_one(
            "INSERT INTO tasks(exploration_id, description, goal) VALUES ($1,'','') RETURNING id",
            &[&exp],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    (id, exp)
}

/// Delete a task and its exploration by id (no cascade port yet; task has no
/// dependents here).
async fn drop_task(d: &DB, task_id: i64) {
    let exp = d
        .query_opt("SELECT exploration_id FROM tasks WHERE id=$1", &[&task_id])
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<_, i64>(0));
    let _ = d
        .execute("DELETE FROM tasks WHERE id=$1", &[&task_id])
        .await;
    if let Some(e) = exp {
        let _ = d
            .execute("DELETE FROM explorations WHERE id=$1", &[&e])
            .await;
    }
}

async fn seed_profile(d: &DB, stamp: u128, i: usize) -> i64 {
    d.query_one(
        "INSERT INTO llm_profiles(name, format, model) VALUES ($1,'openai','m') RETURNING id",
        &[&format!("create-prof-{stamp}-{i}")],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

#[tokio::test]
async fn create_task_basic_and_origin_node() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let t = d
        .create_task("the description", "the goal", None, 0, 0)
        .await
        .expect("create");

    assert_eq!(t.status, "created", "default status");
    assert!(!t.paused && !t.queued);
    assert_eq!(t.description, "the description");
    assert_eq!(t.goal, "the goal");
    assert_eq!(t.timeout_seconds, 0);
    assert_eq!(
        t.plan_heartbeat_seconds, MIN_PLAN_HEARTBEAT_SECONDS,
        "sub-floor heartbeat raised to the default"
    );
    assert!(t.plan_quiet_mode && t.coverage_enabled, "defaults on");
    assert_eq!(t.llm_failover_state, "default", "no chain -> default");
    assert!(t.llm_profile_ids.is_empty());
    assert!(t.created_at != SystemTime::UNIX_EPOCH, "created_at stamped");

    // The exploration graph root is an origin fact node holding desc+goal.
    let origin = d
        .query_one(
            "SELECT kind, state, payload->>'description', payload->>'goal' \
             FROM exploration_nodes WHERE exploration_id=$1 AND state='origin'",
            &[&t.exploration_id],
        )
        .await
        .expect("origin node");
    assert_eq!(origin.get::<_, String>(0), "fact");
    assert_eq!(origin.get::<_, String>(1), "origin");
    assert_eq!(origin.get::<_, String>(2), "the description");
    assert_eq!(origin.get::<_, String>(3), "the goal");

    drop_task(&d, t.id).await;
}

#[tokio::test]
async fn create_task_with_chain_and_sources() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let src_a = seed_task(&d, uniq()).await;
    let src_b = seed_task(&d, uniq()).await;
    let p0 = seed_profile(&d, stamp, 0).await;
    let p1 = seed_profile(&d, stamp, 1).await;

    let t = d
        .create_task_with_options(
            "child",
            "new goal",
            TaskCreateOptions {
                source_task_ids: vec![src_a.0, src_b.0],
                llm_profile_ids: vec![p0, p1],
                ..TaskCreateOptions::default()
            },
        )
        .await
        .expect("create child");

    // Chain + active head persisted; failover_state ready.
    assert_eq!(t.llm_profile_ids, vec![p0, p1]);
    assert_eq!(t.active_llm_profile_id, Some(p0));
    assert_eq!(t.llm_failover_state, "ready");

    // Hydrated view matches (chains + sources).
    let got = d.get_task(t.id).await.expect("get").expect("some");
    assert_eq!(got.llm_profile_ids, vec![p0, p1]);
    assert_eq!(got.active_llm_profile_id, Some(p0));
    assert_eq!(got.source_task_ids, vec![src_a.0, src_b.0]);
    assert_eq!(got.llm_failover_state, "ready");

    // task_llm_profiles rows are ordered by position.
    let chain: Vec<i64> = d
        .query(
            "SELECT profile_id FROM task_llm_profiles WHERE task_id=$1 ORDER BY position",
            &[&t.id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<_, i64>(0))
        .collect();
    assert_eq!(chain, vec![p0, p1]);

    drop_task(&d, t.id).await;
    drop_task(&d, src_a.0).await;
    drop_task(&d, src_b.0).await;
    for p in [p0, p1] {
        let _ = d
            .execute("DELETE FROM llm_profiles WHERE id=$1", &[&p])
            .await;
    }
}

#[tokio::test]
async fn create_task_validation_and_rollback() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let src = seed_task(&d, uniq()).await;
    let company = d
        .companies()
        .upsert_company(&format!("Create Co {stamp}"), "")
        .await
        .unwrap()
        .0;

    // Duplicate source ids rejected.
    let dup_src = d
        .create_task_with_options(
            "bad",
            "goal",
            TaskCreateOptions {
                source_task_ids: vec![src.0, src.0],
                ..TaskCreateOptions::default()
            },
        )
        .await;
    assert!(dup_src.is_err(), "duplicate source task id rejected");

    // Self-referential source rejected.
    // (Create the task first to learn its id is impossible pre-insert; the check
    // fires on source==task which cannot happen for a fresh id, so exercise the
    // non-positive / missing cases instead.)
    let neg = d
        .create_task_with_options(
            "bad",
            "goal",
            TaskCreateOptions {
                source_task_ids: vec![-1],
                ..TaskCreateOptions::default()
            },
        )
        .await;
    assert!(neg.is_err(), "non-positive source task id rejected");

    let missing_src = d
        .create_task_with_options(
            "bad",
            "goal",
            TaskCreateOptions {
                source_task_ids: vec![99_999_999],
                ..TaskCreateOptions::default()
            },
        )
        .await;
    assert!(missing_src.is_err(), "missing source task rejected");

    // Too many sources rejected.
    let too_many: Vec<i64> = (1..=9).collect();
    let many = d
        .create_task_with_options(
            "bad",
            "goal",
            TaskCreateOptions {
                source_task_ids: too_many,
                ..TaskCreateOptions::default()
            },
        )
        .await;
    assert!(many.is_err(), "over source cap rejected");

    // Missing company -> ErrTaskCompanyNotFound + no leaked exploration row.
    let bad_desc = format!("invalid-company-{stamp}");
    let bad = d
        .create_task_with_options(
            &bad_desc,
            "rollback",
            TaskCreateOptions {
                company_ids: vec![company, 1_i64 << 62],
                ..TaskCreateOptions::default()
            },
        )
        .await;
    let err = bad.expect_err("missing company rejected");
    assert!(
        err.is(artex::db::tasks::ERR_TASK_COMPANY_NOT_FOUND),
        "missing company sentinel, got {err}"
    );
    let leaked: i64 = d
        .query_one(
            "SELECT count(*) FROM explorations WHERE description=$1",
            &[&bad_desc],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(leaked, 0, "failed association rolled back the exploration");

    drop_task(&d, src.0).await;
    let _ = d
        .execute("DELETE FROM companies WHERE id=$1", &[&company])
        .await;
}

#[tokio::test]
async fn create_task_company_scope_snapshot() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let company_a = d
        .companies()
        .upsert_company(&format!("Task Company A {stamp}"), "")
        .await
        .unwrap()
        .0;
    let company_b = d
        .companies()
        .upsert_company(&format!("Task Company B {stamp}"), "")
        .await
        .unwrap()
        .0;

    // existingTask owns companyAssetA already.
    let existing = seed_task(&d, uniq()).await;
    let domain_a = format!("task-company-a-{stamp}.example.test");
    let asset_a = d
        .query_one(
            "INSERT INTO assets(type, domain, root_domain, company_id, company_source, task_ids) \
             VALUES ('root_domain',$1,$1,$2,'explicit',ARRAY[$3]::bigint[]) RETURNING id",
            &[&domain_a, &company_a, &existing.0],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let domain_b = format!("task-company-b-{stamp}.example.test");
    let asset_b = d
        .query_one(
            "INSERT INTO assets(type, domain, root_domain, company_id, company_source) \
             VALUES ('root_domain',$1,$1,$2,'explicit') RETURNING id",
            &[&domain_b, &company_b],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    // Duplicate company ids normalize to first-seen order.
    let task = d
        .create_task_with_options(
            "company-scoped task",
            "use company scope",
            TaskCreateOptions {
                company_ids: vec![company_a, company_a, company_b, company_a],
                ..TaskCreateOptions::default()
            },
        )
        .await
        .expect("create company task");
    assert_eq!(
        task.company_ids,
        vec![company_a, company_b],
        "company ids normalized"
    );

    // Hydrated company ids match.
    let got = d.get_task(task.id).await.expect("get").expect("some");
    assert_eq!(got.company_ids, vec![company_a, company_b]);

    // Two company task_scope rows.
    let scope_count: i64 = d
        .query_one(
            "SELECT count(*) FROM task_scope WHERE task_id=$1 AND kind='company'",
            &[&task.id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(scope_count, 2);

    // Both company assets are snapshotted into the task.
    let links: Vec<(i64, String, String)> = d
        .query(
            "SELECT asset_id, source, source_summary FROM task_asset_links WHERE task_id=$1",
            &[&task.id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    assert_eq!(links.len(), 2, "two provenance links");
    for (asset_id, source, summary) in &links {
        assert_eq!(source, "company", "link source=company");
        let expected_name = if *asset_id == asset_a {
            format!("Task Company A {stamp}")
        } else {
            format!("Task Company B {stamp}")
        };
        assert_eq!(
            summary,
            &format!("associated with company at task creation: {expected_name}")
        );
    }

    // The existing task's association on asset_a survives the new snapshot.
    let still_assoc: bool = d
        .query_one(
            "SELECT $1=ANY(task_ids) FROM assets WHERE id=$2",
            &[&existing.0, &asset_a],
        )
        .await
        .unwrap()
        .get(0);
    assert!(still_assoc, "existing task association preserved");
    // And asset_a now also carries the new task.
    let has_new: bool = d
        .query_one(
            "SELECT $1=ANY(task_ids) FROM assets WHERE id=$2",
            &[&task.id, &asset_a],
        )
        .await
        .unwrap()
        .get(0);
    assert!(has_new, "new task appended to asset task_ids");

    drop_task(&d, task.id).await;
    drop_task(&d, existing.0).await;
    let _ = d
        .execute(
            "DELETE FROM assets WHERE id=ANY($1::bigint[])",
            &[&vec![asset_a, asset_b]],
        )
        .await;
    let _ = d
        .execute(
            "DELETE FROM companies WHERE id=ANY($1::bigint[])",
            &[&vec![company_a, company_b]],
        )
        .await;
}

#[tokio::test]
async fn create_task_intercept_rules_and_category() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let cat = d
        .query_one(
            "INSERT INTO task_categories(name, nkey) VALUES ($1,$2) RETURNING id",
            &[&format!("Cat {stamp}"), &format!("cat-{stamp}")],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    let t = d
        .create_task_with_options(
            "desc",
            "goal",
            TaskCreateOptions {
                name: "named".to_string(),
                category_id: Some(cat),
                intercept_rules: vec![
                    TaskInterceptRuleInput {
                        enabled: true,
                        action: "allow".to_string(),
                        kind: "exact_domain".to_string(),
                        pattern: "good.example".to_string(),
                        note: String::new(),
                    },
                    TaskInterceptRuleInput {
                        enabled: true,
                        action: "".to_string(), // empty -> block
                        kind: "fuzzy_url".to_string(),
                        pattern: "bad.example".to_string(),
                        note: "n".to_string(),
                    },
                ],
                ..TaskCreateOptions::default()
            },
        )
        .await
        .expect("create with rules");

    assert_eq!(t.name, "named");
    assert_eq!(t.category_id, Some(cat));
    assert_eq!(
        t.category_name,
        format!("Cat {stamp}"),
        "category name resolved"
    );

    let rules: Vec<(String, String, String)> = d
        .query(
            "SELECT action, kind, pattern FROM task_intercept_rules \
             WHERE task_id=$1 ORDER BY pattern",
            &[&t.id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    // Sorted by pattern: bad.example (block, empty->block) then good.example (allow).
    assert_eq!(
        rules,
        vec![
            (
                "block".to_string(),
                "fuzzy_url".to_string(),
                "bad.example".to_string()
            ),
            (
                "allow".to_string(),
                "exact_domain".to_string(),
                "good.example".to_string()
            ),
        ],
        "actions normalized (empty -> block)"
    );

    // Invalid category id rejected.
    let bad = d
        .create_task_with_options(
            "desc",
            "goal",
            TaskCreateOptions {
                category_id: Some(0),
                ..TaskCreateOptions::default()
            },
        )
        .await;
    assert!(bad.is_err(), "non-positive category id rejected");

    drop_task(&d, t.id).await;
    let _ = d
        .execute("DELETE FROM task_categories WHERE id=$1", &[&cat])
        .await;
}
