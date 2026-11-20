//! Differential tests for `db/finding_retests.go` + `SetFindingStatusTx` — ports
//! of the key cases in `db/finding_retests_test.go` (atomic dedup, snapshot
//! immutability, staged-result rules, fixed-triage only after success).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::finding_retests::ERR_RETEST_NOT_RUNNING;
use artex::db::findings::FINDING_FIXED;
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

async fn seed_finding(d: &artex::db::DB) -> i64 {
    d.add_finding(
        0,
        0,
        "retest-test",
        "test vulnerability",
        "high",
        "original summary",
        "original evidence",
        "worker",
        &[],
    )
    .await
    .unwrap()
}

async fn cleanup(d: &artex::db::DB, fid: i64) {
    let _ = d
        .execute(
            "DELETE FROM conversations WHERE id IN (SELECT conversation_id FROM finding_retests WHERE finding_id=$1)",
            &[&fid],
        )
        .await;
    let _ = d.delete_finding(fid).await;
}

#[tokio::test]
async fn retest_dedup_snapshot_and_staged_result_lifecycle() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let ctx = Context::background();
    let fid = seed_finding(&d).await;

    // First create makes a conversation; a second returns the same run, created=false.
    let (r, conv, created) = d
        .create_finding_retest(&ctx, fid, "  fixed in v2  ")
        .await
        .unwrap();
    assert!(
        created && conv.is_some(),
        "first retest creates a conversation"
    );
    assert_eq!(r.notes, "fixed in v2", "notes trimmed");
    let conv_id = conv.unwrap().id;
    let (r2, conv2, created2) = d.create_finding_retest(&ctx, fid, "other").await.unwrap();
    assert!(!created2 && conv2.is_none(), "second create deduped");
    assert_eq!(r2.id, r.id, "same run id");
    assert_eq!(d.list_finding_retests(fid).await.unwrap().len(), 1);

    // History omits the snapshot; by-conversation loads it.
    let history = d.list_finding_retests(fid).await.unwrap();
    assert!(
        history[0].snapshot.is_none(),
        "history does not leak snapshot"
    );
    let full = d
        .finding_retest_for_conversation(&ctx, conv_id)
        .await
        .unwrap()
        .unwrap();
    let snap = full.snapshot.unwrap().to_string();
    assert!(
        snap.contains("original evidence"),
        "snapshot holds original evidence"
    );

    // Snapshot is immutable even if the finding changes.
    d.execute(
        "UPDATE findings SET evidence='changed evidence' WHERE id=$1",
        &[&fid],
    )
    .await
    .unwrap();
    let full = d
        .finding_retest_for_conversation(&ctx, conv_id)
        .await
        .unwrap()
        .unwrap();
    let snap = full.snapshot.unwrap().to_string();
    assert!(!snap.contains("changed evidence"), "snapshot unchanged");
    assert!(snap.contains("original evidence"));

    // One user message seeded.
    let messages: i64 = d
        .query_one(
            "SELECT count(*) FROM conversation_activities WHERE conversation_id=$1 AND kind='user'",
            &[&conv_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(messages, 1);

    // Pending rejects a result; unknown conversation rejects; start -> running.
    let err = d
        .record_finding_retest_result(&ctx, conv_id, "fixed", "s", "p")
        .await
        .expect_err("pending rejects result");
    assert!(err.is(ERR_RETEST_NOT_RUNNING), "got {err}");
    let err = d
        .record_finding_retest_result(&ctx, 0, "fixed", "s", "p")
        .await
        .expect_err("unscoped write");
    assert!(err.is(ERR_RETEST_NOT_RUNNING));
    assert!(d.start_finding_retest(&ctx, r.id).await.unwrap());

    // Identical retries safe; a different verdict is refused.
    for _ in 0..2 {
        d.record_finding_retest_result(
            &ctx,
            conv_id,
            "fixed",
            "fix verification passed",
            "control works normally, original trigger condition no longer applies",
        )
        .await
        .unwrap();
    }
    let err = d
        .record_finding_retest_result(&ctx, conv_id, "reproduced", "different", "proof")
        .await
        .expect_err("overwrite staged verdict");
    assert!(err.is(ERR_RETEST_NOT_RUNNING));

    // Finish -> fixed triage; sealed result cannot be rewritten.
    d.finish_finding_retest(r.id, "completed", "")
        .await
        .unwrap();
    let err = d
        .record_finding_retest_result(&ctx, conv_id, "fixed", "s", "p")
        .await
        .expect_err("sealed result");
    assert!(err.is(ERR_RETEST_NOT_RUNNING));
    let f = d.get_finding(fid).await.unwrap().unwrap();
    assert_eq!(f.status, FINDING_FIXED, "fixed retest updates triage");
    assert_eq!(f.evidence, "changed evidence");

    // A completed retest with NO staged verdict becomes failed (not silently fixed).
    let (next, _, created) = d.create_finding_retest(&ctx, fid, "second").await.unwrap();
    assert!(
        created && next.id != r.id,
        "new retest after previous sealed"
    );
    d.finish_finding_retest(next.id, "completed", "")
        .await
        .unwrap();
    let rows = d.list_finding_retests(fid).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|x| x.status == "failed" && !x.error.is_empty()),
        "missing verdict -> failed"
    );

    cleanup(&d, fid).await;
}

#[tokio::test]
async fn retest_validation_and_recovery() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let ctx = Context::background();
    let fid = seed_finding(&d).await;
    let (r, conv, _) = d.create_finding_retest(&ctx, fid, "notes").await.unwrap();
    let conv_id = conv.unwrap().id;

    // Invalid verdict / empty fields / invalid terminal status rejected.
    assert!(d
        .record_finding_retest_result(&ctx, conv_id, "bogus", "s", "p")
        .await
        .is_err());
    assert!(d
        .record_finding_retest_result(&ctx, conv_id, "fixed", "", "p")
        .await
        .is_err());
    assert!(d.finish_finding_retest(r.id, "bogus", "").await.is_err());

    // FailPending seals a pending retest by conversation id.
    d.fail_pending_retest_for_conversation(conv_id, "runner failed")
        .await
        .unwrap();
    let rows = d.list_finding_retests(fid).await.unwrap();
    let pending = rows.iter().find(|x| x.id == r.id).unwrap();
    assert_eq!(pending.status, "failed");
    assert_eq!(pending.error, "runner failed");

    // Active list excludes the failed retest.
    assert!(d
        .list_active_finding_retests(&ctx)
        .await
        .unwrap()
        .iter()
        .all(|x| x.id != r.id));

    // RecoverFindingRetests stops all pending/running retests.
    let (r2, _, _) = d.create_finding_retest(&ctx, fid, "second").await.unwrap();
    d.recover_finding_retests().await.unwrap();
    let rows = d.list_finding_retests(fid).await.unwrap();
    assert_eq!(
        rows.iter().find(|x| x.id == r2.id).unwrap().status,
        "stopped"
    );

    cleanup(&d, fid).await;
}
