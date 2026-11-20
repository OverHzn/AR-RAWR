//! Differential tests for `db/side_questions.go` — ports of the Go store tests:
//! idempotency/paging/recovery, memory paging/clear/restart, legacy archive rows
//! and generation-cleared late writers. Runs against `ARTEX_PG_DSN`.

use artex::db::error::Error as DbError;
use artex::db::side_questions::{ERR_SIDE_BUSY, ERR_SIDE_PARENT_GONE};
use artex::db::DB;
use artex::sidequestion::{GoTime, Model, Parent, Snapshot};

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
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

fn snapshot(conv_id: i64) -> Snapshot {
    Snapshot {
        parent: Parent {
            conversation_id: conv_id,
            ..Parent::default()
        },
        run_id: 1,
        version: 1,
        captured_at: GoTime::now(),
        model: Model {
            model: "fixture".to_string(),
            ..Model::default()
        },
        request: artex::llm::CompletionRequest {
            messages: vec![artex::llm::user_text("main-only")],
            ..artex::llm::CompletionRequest::default()
        },
    }
}

/// Each test creates its own conversation so session keys and generation state
/// are isolated; cleanup is by delete at the end.
async fn fixture(d: &DB, tag: &str) -> i64 {
    let c = d
        .create_conversation("mainagent", &format!("side persistence {tag}"), None)
        .await
        .unwrap();
    let s = snapshot(c.id);
    d.save_side_snapshot(&s).await.unwrap();
    c.id
}

async fn cleanup(d: &DB, conv: i64) {
    let _ = d
        .execute(
            "DELETE FROM side_question_sessions WHERE conversation_id=$1",
            &[&conv],
        )
        .await;
    let _ = d.delete_conversation(conv).await;
}

#[tokio::test]
async fn side_history_idempotency_paging_and_recovery() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let conv = fixture(&d, &tag).await;
    let s = snapshot(conv);

    let (first, created) = d
        .start_side_request(&s, "request-0", "question-0")
        .await
        .unwrap();
    assert!(created);
    let (again, created2) = d
        .start_side_request(&s, "request-0", "question-0")
        .await
        .unwrap();
    assert!(!created2 && again.id == first.id);
    assert!(d
        .start_side_request(&s, "request-0", "different")
        .await
        .is_err());
    let busy = d
        .start_side_request(&s, "request-1", "question-1")
        .await
        .unwrap_err();
    assert!(busy.is(ERR_SIDE_BUSY), "got {busy}");

    for i in 0..24 {
        let mut e = first.clone();
        if i > 0 {
            e = d
                .start_side_request(&s, &format!("request-{i}"), &format!("question-{i}"))
                .await
                .unwrap()
                .0;
        }
        e.answer = format!("answer-{i}");
        e.sequence = 1;
        e.status = "completed".to_string();
        assert!(d.update_side_request(&e).await.unwrap());
    }

    let page = d.side_history(&s.parent.key(), 0, 20).await.unwrap();
    assert_eq!(page.len(), 20);
    let tail = d
        .side_history(&s.parent.key(), page[19].ordinal, 20)
        .await
        .unwrap();
    assert_eq!(tail.len(), 4);
    assert!(tail[0].ordinal < page[19].ordinal);

    let replay = d.side_replay(&s.parent.key()).await.unwrap();
    assert_eq!(replay.len(), 20);
    assert_eq!(replay[0].question, "question-4");
    assert_eq!(replay[19].question, "question-23");

    let mut partial = d
        .start_side_request(&s, "unfinished", "partial question")
        .await
        .unwrap()
        .0;
    partial.answer = "saved partial".to_string();
    partial.sequence = 1;
    partial.usage.input_tokens = 17;
    d.update_side_request(&partial).await.unwrap();
    d.interrupt_side_requests().await.unwrap();
    let got = d.side_request(&partial.id).await.unwrap().unwrap();
    assert_eq!(got.status, "interrupted");
    assert_eq!(got.answer, "saved partial");
    assert_eq!(got.usage.input_tokens, 17);

    let saved = d.side_snapshot(&s.parent.key()).await.unwrap().unwrap();
    assert!(!saved.request.messages.is_empty());
    d.start_side_request(&saved, "after-restart", "continue")
        .await
        .unwrap();

    cleanup(&d, conv).await;
}

#[tokio::test]
async fn side_memory_clear_and_generation() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let conv = fixture(&d, &tag).await;
    let s = snapshot(conv);

    let mut e = d.start_side_request(&s, "m0", "q0").await.unwrap().0;
    e.answer = "a0".to_string();
    e.sequence = 1;
    e.status = "completed".to_string();
    d.update_side_request(&e).await.unwrap();

    // Memory is read/written only while the admitted request is still running.
    let running = d.start_side_request(&s, "m1", "q1").await.unwrap().0;
    let mem = artex::sidequestion::Memory {
        history: "summary".to_string(),
        through: 1,
        ..Default::default()
    };
    d.save_side_memory(&running, &mem).await.unwrap();
    let got = d.side_memory(&running).await.unwrap();
    assert_eq!(got.history, "summary");
    assert_eq!(got.through, 1);

    d.clear_side_history(&s.parent.key()).await.unwrap();
    // Late writer against the cleared generation is rejected.
    let stale = d.save_side_memory(&running, &mem).await.unwrap_err();
    assert!(stale.is(ERR_SIDE_PARENT_GONE), "got {stale}");
    // A new request can be started after the bump.
    let (fresh, created) = d.start_side_request(&s, "m2", "q2").await.unwrap();
    assert!(created && fresh.generation > running.generation);
    // Old exchanges are gone from history.
    assert!(d.side_request(&e.id).await.unwrap().is_none());

    cleanup(&d, conv).await;
}

#[tokio::test]
async fn side_parent_gone_and_legacy_rows() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let conv = fixture(&d, &tag).await;
    let s = snapshot(conv);

    // A snapshot whose parent was deleted cannot create new requests.
    let missing = snapshot(conv + 9_000_000);
    let err = d.save_side_snapshot(&missing).await.unwrap_err();
    assert!(err.is(ERR_SIDE_PARENT_GONE), "got {err}");

    // Legacy archive rows: context_info / usage absent entirely -> defaults.
    let legacy_id = format!("legacy-{tag}");
    let key = s.parent.key();
    let gen: i64 = d
        .query_one(
            "SELECT generation FROM side_question_sessions WHERE session_key=$1",
            &[&key],
        )
        .await
        .unwrap()
        .get(0);
    d.execute(
        "INSERT INTO side_question_requests(id,session_key,generation,client_id,question,status,model,snapshot_at)
         VALUES ($1,$2,$3,'legacy','legacy-q','completed','{}',now())",
        &[&legacy_id, &key, &gen],
    ).await.unwrap();
    let got = d.side_request(&legacy_id).await.unwrap().unwrap();
    assert_eq!(got.status, "completed");
    assert_eq!(got.usage.input_tokens, 0);
    assert_eq!(got.context.phase, "");
    let _: Option<DbError> = None;

    cleanup(&d, conv).await;
}

#[tokio::test]
async fn side_replay_page_and_clear_race() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let conv = fixture(&d, &tag).await;
    let s = snapshot(conv);

    let mut e = d.start_side_request(&s, "p0", "q0").await.unwrap().0;
    e.answer = "a0".to_string();
    e.sequence = 1;
    e.status = "completed".to_string();
    d.update_side_request(&e).await.unwrap();
    let admitted = d.start_side_request(&s, "p1", "q1").await.unwrap().0;
    let page = d.side_replay_page(&admitted, 0).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].question, "q0");

    // Two concurrent clearers are serialized; both bump generation once each.
    let key = s.parent.key();
    let d1 = d.clone();
    let k1 = key.clone();
    let t1 = tokio::spawn(async move { d1.clear_side_history(&k1).await });
    let t2 = d.clear_side_history(&key).await;
    t1.await.unwrap().unwrap();
    t2.unwrap();
    let gen: i64 = d
        .query_one(
            "SELECT generation FROM side_question_sessions WHERE session_key=$1",
            &[&key],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(gen, 2);

    cleanup(&d, conv).await;
}
