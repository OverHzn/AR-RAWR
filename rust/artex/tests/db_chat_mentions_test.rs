//! Differential tests for chat-mention search — fresh spec for `db/chat_mentions.go`
//! (the Go package ships no `_test.go` for it).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable. Seeds findings/companies/assets directly (all three FK
//! chains are nullable/defaulted, so no task/exploration scaffolding is needed) and
//! cleans up by a process-unique query prefix.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::chat_mentions::{valid_chat_mention_kind, ERR_INVALID_CHAT_MENTION_CURSOR};
use artex::db::DB;
use artex::llm::Context;

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

/// `ValidChatMentionKind`: known kinds pass, others (and "") fail.
#[test]
fn valid_chat_mention_kind_cases() {
    for k in [
        "finding",
        "company",
        "asset",
        "endpoint",
        "ip",
        "app",
        "root_domain",
        "subdomain",
        "service",
    ] {
        assert!(valid_chat_mention_kind(k), "{k} should be valid");
    }
    assert!(!valid_chat_mention_kind(""), "empty is not valid");
    assert!(!valid_chat_mention_kind("bogus"), "unknown is not valid");
}

#[tokio::test]
async fn search_by_kind_and_query() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let ctx = Context::background();
    let stamp = uniq();
    let tag = format!("tchat-{stamp}");

    // Seed one finding, one company, one asset sharing the searchable token `tag`.
    d.query_one(
        "INSERT INTO findings(name, vulnclass, severity, status, summary) VALUES ($1,'xss','low','open',$1) RETURNING id",
        &[&tag],
    )
    .await
    .expect("seed finding");
    d.query_one(
        "INSERT INTO companies(name, nkey) VALUES ($1,$1) RETURNING id",
        &[&tag],
    )
    .await
    .expect("seed company");
    d.query_one(
        "INSERT INTO assets(type, domain) VALUES ('subdomain',$1) RETURNING id",
        &[&tag],
    )
    .await
    .expect("seed asset");

    // kind=finding finds the finding (ILIKE over name/vulnclass/summary).
    let f = d
        .search_chat_mentions(&ctx, "finding", &tag)
        .await
        .expect("search finding");
    assert!(
        f.iter().any(|m| m.kind == "finding"),
        "finding present: {f:?}"
    );

    // kind=company finds the company.
    let c = d
        .search_chat_mentions(&ctx, "company", &tag)
        .await
        .expect("search company");
    assert!(
        c.iter().any(|m| m.kind == "company" && m.label == tag),
        "company present: {c:?}"
    );

    // kind=asset finds the asset. The assets branch returns the asset's `type`
    // (e.g. "subdomain") as the result kind, matching Go's `SELECT type, id ...`;
    // kind="asset" means "any asset type".
    let a = d
        .search_chat_mentions(&ctx, "asset", &tag)
        .await
        .expect("search asset");
    assert!(
        a.iter()
            .any(|m| m.label == tag && valid_chat_mention_kind(&m.kind)),
        "asset present with its type as kind: {a:?}"
    );

    // An unsupported kind errors.
    let bad = d.search_chat_mentions(&ctx, "nonsense", &tag).await;
    assert!(bad.is_err(), "unsupported kind must error");

    // Cleanup.
    d.execute("DELETE FROM findings WHERE name=$1", &[&tag])
        .await
        .expect("clean finding");
    d.execute("DELETE FROM companies WHERE nkey=$1", &[&tag])
        .await
        .expect("clean company");
    d.execute("DELETE FROM assets WHERE domain=$1", &[&tag])
        .await
        .expect("clean asset");
}

#[tokio::test]
async fn pagination_cursor_round_trip() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let ctx = Context::background();
    let stamp = uniq();
    let tag = format!("tpage-{stamp}");

    // Seed 25 findings sharing the token `tag`, so a search has >20 results.
    for _ in 0..25 {
        d.execute(
            "INSERT INTO findings(name, vulnclass, severity, status, summary) VALUES ($1,'xss','low','open',$1)",
            &[&tag],
        )
        .await
        .expect("seed finding");
    }

    // Page 1: 20 items + a next cursor.
    let p1 = d
        .search_chat_mentions_page(&ctx, "finding", &tag, "")
        .await
        .expect("page 1");
    assert_eq!(p1.items.len(), 20, "first page caps at 20");
    assert!(!p1.next_cursor.is_empty(), "more results -> next cursor");

    // Page 2 with the cursor: the remaining 5, no further cursor.
    let p2 = d
        .search_chat_mentions_page(&ctx, "finding", &tag, &p1.next_cursor)
        .await
        .expect("page 2");
    assert_eq!(p2.items.len(), 5, "second page has the remainder");
    assert!(p2.next_cursor.is_empty(), "no cursor after the last page");

    // No overlap between pages (stable sort key, not offset).
    let ids1: std::collections::HashSet<i64> = p1.items.iter().map(|m| m.id).collect();
    assert!(
        p2.items.iter().all(|m| !ids1.contains(&m.id)),
        "pages must not overlap"
    );

    // A cursor whose scope/query doesn't match the request is rejected.
    let bad = d
        .search_chat_mentions_page(&ctx, "finding", &tag, &p1.next_cursor)
        .await;
    assert!(bad.is_ok(), "matching cursor accepted");
    let mismatched = d
        .search_chat_mentions_page(&ctx, "finding", "different-query", &p1.next_cursor)
        .await;
    assert!(
        mismatched.is_err(),
        "cursor bound to a different query must be rejected"
    );
    let err = mismatched.unwrap_err();
    assert!(
        err.is(ERR_INVALID_CHAT_MENTION_CURSOR),
        "error carries the invalid-cursor sentinel: {err:?}"
    );

    // Garbage cursor is rejected.
    let garbage = d
        .search_chat_mentions_page(&ctx, "finding", &tag, "!!!not-base64!!!")
        .await;
    assert!(garbage.is_err(), "garbage cursor must error");

    // Cleanup.
    d.execute("DELETE FROM findings WHERE name=$1", &[&tag])
        .await
        .expect("cleanup");
}
