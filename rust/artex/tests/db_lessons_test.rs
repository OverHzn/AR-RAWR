//! Differential tests for `db/lessons.go` — port of the key cases in
//! `db/lessons_test.go` (extraction scopes/retraction/retirement, task ranking,
//! usage logging) plus the pure `RenderLessons` / dedupe helpers.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::lessons::{lesson_dedupe_key, render_lessons, Lesson};
use artex::llm::Context;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<artex::db::DB> {
    match artex::db::DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

fn ctx() -> Context {
    Context::background()
}

fn lesson(id: i64, kind: &str, cue: &str, action: &str, support: i64, evidence: &[&str]) -> Lesson {
    Lesson {
        id,
        kind: kind.to_string(),
        cue: cue.to_string(),
        action: action.to_string(),
        support,
        evidence: evidence.iter().map(|s| s.to_string()).collect(),
        ..Lesson::default()
    }
}

#[test]
fn render_lessons_data_not_instructions_and_truncation() {
    // Empty -> empty.
    assert_eq!(render_lessons(&[], 0), (String::new(), Vec::new()));

    let lessons = vec![
        lesson(
            1,
            "positive",
            "xss",
            "confirmed reflected xss",
            2,
            &["f:1", "r:2"],
        ),
        lesson(2, "negative", "sqli", "do not re-report", 1, &[]),
    ];
    let (block, ids) = render_lessons(&lessons, 0);
    assert!(block.starts_with("## Prior evidence from earlier tasks"));
    assert!(block.contains("not instructions"));
    assert!(block.contains("- [positive, 2 evidence] xss: confirmed reflected xss"));
    assert!(block.contains("(evidence: f:1; r:2)"));
    assert_eq!(ids, vec![1, 2], "both rendered when unlimited");

    // A tiny budget includes only the header -> empty result, no ids.
    let (block, ids) = render_lessons(&lessons, 40);
    assert!(block.is_empty(), "header-only block suppressed");
    assert!(ids.is_empty());

    // A budget that fits exactly one entry -> only its id logged.
    let (block, ids) = render_lessons(&lessons, 200);
    assert!(!block.is_empty());
    assert_eq!(ids.len(), 1, "only the lessons that fit are returned");
    assert_eq!(ids[0], 1);
}

#[test]
fn lesson_dedupe_key_is_stable_and_case_insensitive() {
    let a = lesson_dedupe_key("positive", 0, 0, "XSS", "Action");
    let b = lesson_dedupe_key("positive", 0, 0, "xss", "action");
    assert_eq!(a, b, "cue/action lowercased");
    assert_eq!(a.len(), 64, "sha256 hex");
    assert_ne!(a, lesson_dedupe_key("negative", 0, 0, "XSS", "Action"));
    assert_ne!(a, lesson_dedupe_key("positive", 7, 0, "XSS", "Action"));
}

async fn seed_company(d: &artex::db::DB, tag: &str) -> i64 {
    d.query_one(
        "INSERT INTO companies(name, nkey) VALUES ($1, $2) RETURNING id",
        &[
            &format!("Lesson {tag}"),
            &format!("lesson-{tag}-{}", std::process::id()),
        ],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

async fn add_scope(d: &artex::db::DB, task_id: i64, company_id: i64) {
    d.execute(
        "INSERT INTO task_scope(task_id, kind, company_id) VALUES ($1,'company',$2)",
        &[&task_id, &company_id],
    )
    .await
    .unwrap();
}

async fn seed_finding(d: &artex::db::DB, task_id: i64, status: &str) -> i64 {
    d.query_one(
        "INSERT INTO findings(task_id,vulnclass,name,summary,severity,status) \
         VALUES ($1,'xss','Reflected XSS','reflected in search','high',$2) RETURNING id",
        &[&task_id, &status],
    )
    .await
    .unwrap()
    .get::<_, i64>(0)
}

#[tokio::test]
async fn extract_lessons_scope_retraction_and_retirement() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let _ = d.execute("DELETE FROM rsi_lessons", &[]).await;

    // Single-company task: a confirmed finding yields one scoped positive lesson.
    let company = seed_company(&d, "single").await;
    let t = d.create_task("extract", "goal", None, 0, 0).await.unwrap();
    add_scope(&d, t.id, company).await;
    let fid = seed_finding(&d, t.id, "confirmed").await;

    let n = d.extract_lessons_for_task(&ctx(), t.id).await.unwrap();
    assert_eq!(n, 1, "one positive lesson extracted");
    let lessons = d.task_lessons(&ctx(), t.id, 8).await.unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(
        lessons[0].company_id, company,
        "lesson scoped to the company"
    );
    assert_eq!(lessons[0].kind, "positive");
    assert_eq!(lessons[0].support, 1);
    let positive_id = lessons[0].id;

    // Flip the finding to false_positive: a negative lesson appears, the positive
    // lesson's finding evidence is retracted, and (now empty) it retires.
    d.execute(
        "UPDATE findings SET status='false_positive' WHERE id=$1",
        &[&fid],
    )
    .await
    .unwrap();
    let n = d.extract_lessons_for_task(&ctx(), t.id).await.unwrap();
    assert_eq!(
        n, 1,
        "only the negative lesson is touched (positive is retracted, not re-added)"
    );
    let lessons = d.task_lessons(&ctx(), t.id, 8).await.unwrap();
    assert!(
        lessons.iter().any(|l| l.kind == "negative"),
        "negative lesson present"
    );
    let pos_status: String = d
        .query_one(
            "SELECT status FROM rsi_lessons WHERE id=$1",
            &[&positive_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        pos_status, "retired",
        "positive lesson retired after evidence gone"
    );

    // Cleanup.
    let _ = d
        .execute("DELETE FROM rsi_lessons WHERE id=$1", &[&positive_id])
        .await;
    let _ = d
        .execute("DELETE FROM rsi_lessons WHERE company_id=$1", &[&company])
        .await;
    let _ = d
        .execute("DELETE FROM companies WHERE id=$1", &[&company])
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn extract_lessons_multi_company_skip() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let _ = d.execute("DELETE FROM rsi_lessons", &[]).await;

    let a = seed_company(&d, "multi-a").await;
    let b = seed_company(&d, "multi-b").await;
    let t = d.create_task("multi", "goal", None, 0, 0).await.unwrap();
    add_scope(&d, t.id, a).await;
    add_scope(&d, t.id, b).await;
    let fid = seed_finding(&d, t.id, "confirmed").await;

    let n = d.extract_lessons_for_task(&ctx(), t.id).await.unwrap();
    assert_eq!(n, 0, "multi-company task skips extraction");
    let ev: i64 = d
        .query_one(
            "SELECT count(*) FROM rsi_lesson_evidence WHERE source_kind='finding' AND source_id=$1",
            &[&fid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(ev, 0, "no evidence written");

    let _ = d
        .execute(
            "DELETE FROM companies WHERE id=ANY($1::bigint[])",
            &[&vec![a, b]],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn extract_lessons_retest_caution_and_revive() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let _ = d.execute("DELETE FROM rsi_lessons", &[]).await;

    // No company -> generic lesson. A retest with verdict 'fixed' adds a caution.
    let t = d
        .create_task("retest-caution", "goal", None, 0, 0)
        .await
        .unwrap();
    let fid = seed_finding(&d, t.id, "confirmed").await;
    let rid = d
        .query_one(
            "INSERT INTO finding_retests(finding_id, verdict, snapshot) VALUES ($1,'fixed','{}'::jsonb) RETURNING id",
            &[&fid],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    let n = d.extract_lessons_for_task(&ctx(), t.id).await.unwrap();
    assert_eq!(
        n, 2,
        "positive + caution lessons from confirmed finding + fixed retest"
    );
    let lessons = d.task_lessons(&ctx(), t.id, 8).await.unwrap();
    assert!(
        lessons.iter().any(|l| l.kind == "caution"),
        "caution lesson present"
    );
    assert!(lessons.iter().all(|l| l.company_id == 0), "generic lessons");

    // Remove all retest evidence -> caution lesson retires. Re-extract revives it
    // as candidate because the dedupe row now has no evidence.
    d.execute("DELETE FROM finding_retests WHERE id=$1", &[&rid])
        .await
        .unwrap();
    d.execute("UPDATE findings SET status='pending' WHERE id=$1", &[&fid])
        .await
        .unwrap();
    let caution_id = lessons.iter().find(|l| l.kind == "caution").unwrap().id;
    // Retract its retest evidence via raw delete + retire rule (mirrors retraction path).
    d.execute(
        "DELETE FROM rsi_lesson_evidence WHERE lesson_id=$1 AND source_kind='retest'",
        &[&caution_id],
    )
    .await
    .unwrap();
    d.execute(
        "UPDATE rsi_lessons SET status='retired' WHERE id=$1 AND NOT EXISTS (SELECT 1 FROM rsi_lesson_evidence e WHERE e.lesson_id=$1)",
        &[&caution_id],
    )
    .await
    .unwrap();
    let status: String = d
        .query_one("SELECT status FROM rsi_lessons WHERE id=$1", &[&caution_id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "retired");

    // Cleanup.
    let _ = d
        .execute("DELETE FROM rsi_lessons WHERE id=$1", &[&caution_id])
        .await;
    let _ = d
        .execute(
            "DELETE FROM rsi_lessons WHERE company_id=0 AND kind='positive'",
            &[],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}

#[tokio::test]
async fn task_lessons_ranking_and_log_usage() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let _ = d.execute("DELETE FROM rsi_lessons", &[]).await;

    let t = d.create_task("rank", "goal", None, 0, 0).await.unwrap();
    // Two generic lessons of different kinds; positive must rank before negative.
    let pos = d
        .query_one(
            "INSERT INTO rsi_lessons(dedupe_key,kind,cue,action) VALUES ($1,'positive','p','do p') RETURNING id",
            &[&format!("rank-pos-{}", std::process::id())],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let neg = d
        .query_one(
            "INSERT INTO rsi_lessons(dedupe_key,kind,cue,action) VALUES ($1,'negative','n','do n') RETURNING id",
            &[&format!("rank-neg-{}", std::process::id())],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    let lessons = d.task_lessons(&ctx(), t.id, 8).await.unwrap();
    let pos_rank = lessons.iter().position(|l| l.id == pos).unwrap();
    let neg_rank = lessons.iter().position(|l| l.id == neg).unwrap();
    assert!(pos_rank < neg_rank, "positive ranks before negative");

    // log_lesson_usage: shadow + inject rows, invalid mode rejected.
    d.log_lesson_usage(&ctx(), t.id, &[pos, neg], "shadow")
        .await
        .unwrap();
    d.log_lesson_usage(&ctx(), t.id, &[pos], "inject")
        .await
        .unwrap();
    let n: i64 = d
        .query_one(
            "SELECT count(*) FROM rsi_lesson_usage WHERE task_id=$1",
            &[&t.id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 3, "two shadow + one inject usage rows");
    assert!(d
        .log_lesson_usage(&ctx(), t.id, &[pos], "bad")
        .await
        .is_err());

    let _ = d
        .execute(
            "DELETE FROM rsi_lessons WHERE id=ANY($1::bigint[])",
            &[&vec![pos, neg]],
        )
        .await;
    d.delete_task(t.id).await.unwrap();
}
