//! Differential tests for `db/conversation.go` — ported from
//! `db/conversation_pin_test.go` plus CRUD/profile/activity/token coverage.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::conversation::{ConversationPatch, ERR_LLM_PROFILE_NOT_FOUND};
use artex::db::exploration::Activity;

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

fn act(summary: &str, kind: &str, tokens: Option<i32>) -> Activity {
    Activity {
        worker: "tester".to_string(),
        kind: kind.to_string(),
        summary: summary.to_string(),
        detail: format!("detail-{summary}"),
        input_tokens: tokens,
        output_tokens: tokens,
        cache_read_tokens: tokens,
        cache_write_tokens: tokens,
        ..Activity::default()
    }
}

#[tokio::test]
async fn conversation_pin_ordering_and_patch() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let first = d
        .create_conversation("mainagent", "pin-first", None)
        .await
        .unwrap();
    let second = d
        .create_conversation("mainagent", "pin-second", None)
        .await
        .unwrap();

    // Pin first, then (after a tick) second.
    let first = d
        .update_conversation(
            first.id,
            ConversationPatch {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(first.pinned && first.pinned_at.is_some());
    let first_pinned_at = first.pinned_at.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let second = d
        .update_conversation(
            second.id,
            ConversationPatch {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(second.pinned);

    // Re-pin with a title change preserves the original pin time.
    let first = d
        .update_conversation(
            first.id,
            ConversationPatch {
                title: Some("renamed while pinned".to_string()),
                pinned: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.title, "renamed while pinned");
    assert_eq!(first.pinned_at, Some(first_pinned_at), "pin time preserved");

    // Newer pin sorts first in the list.
    let list = d.list_conversations().await.unwrap();
    let pos = |id| list.iter().position(|c| c.id == id).unwrap();
    assert!(pos(second.id) < pos(first.id), "newer pin first");

    // Unpin first clears both pinned + pinned_at.
    let first = d
        .update_conversation(
            first.id,
            ConversationPatch {
                pinned: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!first.pinned && first.pinned_at.is_none());

    let _ = d.delete_conversation(first.id).await;
    let _ = d.delete_conversation(second.id).await;
}

#[tokio::test]
async fn conversation_crud_profile_and_gate() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    // Seed a valid LLM profile.
    let pid = d
        .query_one(
            "INSERT INTO llm_profiles(name, format, model) VALUES ('conv-p', 'openai', 'm') RETURNING id",
            &[],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);

    // Gated create with a profile.
    let c = d
        .create_conversation_gated("agent", "gated", Some(pid), true)
        .await
        .unwrap();
    assert_eq!(c.llm_profile_id, Some(pid));
    assert!(c.target_gate);
    assert!(!c.pinned);

    // Get + rename + touch.
    let got = d.get_conversation(c.id).await.unwrap().unwrap();
    assert_eq!(got.title, "gated");
    d.rename_conversation(c.id, "renamed").await.unwrap();
    d.touch_conversation(c.id).await.unwrap();
    assert_eq!(
        d.get_conversation(c.id).await.unwrap().unwrap().title,
        "renamed"
    );
    d.update_conversation(
        c.id,
        ConversationPatch {
            target_gate: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!d.get_conversation(c.id).await.unwrap().unwrap().target_gate);

    // Update profile: clear then set again.
    d.update_conversation_profile(c.id, None).await.unwrap();
    assert_eq!(
        d.get_conversation(c.id)
            .await
            .unwrap()
            .unwrap()
            .llm_profile_id,
        None
    );
    d.update_conversation_profile(c.id, Some(pid))
        .await
        .unwrap();
    assert_eq!(
        d.get_conversation(c.id)
            .await
            .unwrap()
            .unwrap()
            .llm_profile_id,
        Some(pid)
    );

    // Missing profile -> sentinel; the row is unchanged (tx rolled back).
    let err = d
        .update_conversation_profile(c.id, Some(99_999_999))
        .await
        .expect_err("missing profile rejected");
    assert!(err.is(ERR_LLM_PROFILE_NOT_FOUND), "got {err}");
    assert_eq!(
        d.get_conversation(c.id)
            .await
            .unwrap()
            .unwrap()
            .llm_profile_id,
        Some(pid)
    );

    // Unknown conversation id -> no-op (Ok).
    d.update_conversation_profile(99_999_999, None)
        .await
        .unwrap();

    // Delete + batch delete.
    let other = d.create_conversation("agent", "other", None).await.unwrap();
    let deleted = d
        .delete_conversations(&[other.id, 99_999_999])
        .await
        .unwrap();
    assert_eq!(deleted, vec![other.id], "only existing ids returned");
    assert!(d.get_conversation(other.id).await.unwrap().is_none());
    assert!(d.delete_conversations(&[]).await.unwrap().is_empty());

    d.delete_conversation(c.id).await.unwrap();
    assert!(d.get_conversation(c.id).await.unwrap().is_none());
    let _ = d
        .execute("DELETE FROM llm_profiles WHERE id=$1", &[&pid])
        .await;
}

#[tokio::test]
async fn conversation_activities_and_token_summaries() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let c = d.create_conversation("agent", "acts", None).await.unwrap();
    let mut ids = Vec::new();
    for i in 0..5 {
        let id = d
            .append_conv_activity(c.id, act(&format!("step{i}"), "step", None))
            .await
            .unwrap();
        ids.push(id);
    }
    // One kind='result' row with token usage (for token summaries).
    d.append_conv_activity(c.id, act("result", "result", Some(7)))
        .await
        .unwrap();

    // List after the first activity: 5 rows (steps 1..4 + result), cursor advances.
    let (list, cursor) = d.conv_activity_list(c.id, ids[0], 0).await.unwrap();
    assert_eq!(list.len(), 5, "default limit 500");
    assert_eq!(cursor, ids[4] + 1, "cursor = last result id");
    assert_eq!(list[0].summary, "step1");

    // Page (reverse) with size 2: newest-first window reversed to ascending.
    let (page, has_more) = d.conv_activity_page(c.id, 0, 2).await.unwrap();
    assert_eq!(page.len(), 2);
    assert!(has_more, "more history before the window");
    assert!(page[0].id < page[1].id, "ascending id order");
    // Next older page using `before` = first id of previous window.
    let (page2, more2) = d.conv_activity_page(c.id, page[0].id, 2).await.unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page2[1].id < page[0].id, "older window");
    let _ = more2;

    // Detail lazy load.
    let detail = d.conv_activity_detail(c.id, ids[2]).await.unwrap();
    assert_eq!(detail, "detail-step2");
    assert_eq!(d.conv_activity_detail(c.id, 99_999_999).await.unwrap(), "");
    // Detail from another conversation -> "" (scope guard).
    assert_eq!(
        d.conv_activity_detail(99_999_999, ids[2]).await.unwrap(),
        ""
    );

    // Token summaries include this conversation's result-row tokens (7 each).
    let sums = d.conversation_token_summaries().await.unwrap();
    let c_created = d.get_conversation(c.id).await.unwrap().unwrap().created_at;
    // Match by created_at text prefix is brittle; just assert a row with all 7s exists.
    assert!(
        sums.iter().any(|s| s.input_tokens == 7
            && s.output_tokens == 7
            && s.cache_read_tokens == 7
            && s.cache_write_tokens == 7),
        "result-row tokens summed into a summary"
    );
    let _ = c_created;

    let _ = d.delete_conversation(c.id).await;
}
