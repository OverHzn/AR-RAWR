//! Differential tests for the exploration activity stream — fresh spec for the
//! `*ExplorationStore` activity methods in `db/exploration.go` (AppendActivity /
//! ActivityList / ActivityListForTerminalIntent / ActivityPage /
//! ActivityPageForTerminalIntent / ActivityMaxID / ActivityDetail / ActivityByIDs /
//! ActivityByIDsForTerminalIntents).
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the
//! DSN is unset/unreachable. Cleans up via cascade on the seeded exploration.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::exploration::Activity;
use artex::db::DB;
use serde_json::json;

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
async fn exploration_activity_stream() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    let stamp = uniq();
    let exp_id = d
        .create_exploration("desc", &format!("t-activity-{stamp}"))
        .await
        .expect("create");
    let store = d.exploration(exp_id);

    // A done intent (terminal) and a live intent, to exercise the terminal-only
    // read variants vs the whole-task ones.
    let done_intent = store
        .add_intent(&json!({"summary": "done"}), 1, &[], "planner")
        .await
        .expect("done intent");
    let live_intent = store
        .add_intent(&json!({"summary": "live"}), 1, &[], "planner")
        .await
        .expect("live intent");
    store
        .set_intent_state(done_intent, "done")
        .await
        .expect("done state");

    // ---- AppendActivity: a full worker step with metadata + tokens + main_seg.
    let a1 = store
        .append_activity(Activity::new(
            Some(done_intent),
            "w1",
            "result",
            "curl",
            "tu_1",
            false,
            "step one",
            "full detail one",
            json!({"key": "v1"}),
            Some(100),
            Some(50),
            Some(10),
            Some(5),
            None,
        ))
        .await
        .expect("append a1");

    // A thinking row (excluded from trace/terminal reads).
    let _a_think = store
        .append_activity(Activity::new(
            Some(done_intent),
            "w1",
            "thinking",
            "",
            "",
            false,
            "internal reasoning",
            "",
            json!(null),
            None,
            None,
            None,
            None,
            None,
        ))
        .await
        .expect("append thinking");

    // A usage row (excluded from trace/terminal reads).
    let _a_usage = store
        .append_activity(Activity::new(
            Some(done_intent),
            "w1",
            "usage",
            "",
            "",
            false,
            "token frame",
            "",
            json!(null),
            Some(7),
            Some(7),
            Some(7),
            Some(7),
            None,
        ))
        .await
        .expect("append usage");

    // A node-less planner row (mainagent segment 0).
    let a_plan = store
        .append_activity(Activity::new(
            None,
            "planner",
            "result",
            "",
            "",
            false,
            "plan",
            "",
            json!({}),
            Some(1),
            Some(1),
            Some(1),
            Some(1),
            None,
        ))
        .await
        .expect("append plan");

    // A node-less mainagent row with segment 2.
    let a_main = store
        .append_activity(Activity::new(
            None,
            "mainagent",
            "result",
            "",
            "",
            false,
            "main step",
            "",
            json!({}),
            Some(2),
            Some(2),
            Some(2),
            Some(2),
            Some(2),
        ))
        .await
        .expect("append main");

    // A live-intent worker step (not terminal).
    let a_live = store
        .append_activity(Activity::new(
            Some(live_intent),
            "w2",
            "result",
            "",
            "",
            false,
            "live step",
            "",
            json!({}),
            Some(3),
            Some(3),
            Some(3),
            Some(3),
            None,
        ))
        .await
        .expect("append live");

    // Empty text fields become NULL; empty metadata becomes {}.
    let a_nulls = store
        .append_activity(Activity::new(
            Some(done_intent),
            "",
            "",
            "",
            "",
            false,
            "",
            "",
            json!(null),
            None,
            None,
            None,
            None,
            None,
        ))
        .await
        .expect("append nulls");
    let nulls_row = d
        .query_one(
            "SELECT worker IS NULL, kind IS NULL, metadata = '{}'::jsonb FROM activity WHERE id=$1",
            &[&a_nulls],
        )
        .await
        .expect("nulls query");
    assert!(nulls_row.get::<_, bool>(0), "empty worker -> NULL");
    assert!(nulls_row.get::<_, bool>(1), "empty kind -> NULL");
    assert!(nulls_row.get::<_, bool>(2), "null metadata -> empty object");

    // ---- ActivityMaxID: highest activity id so far.
    let max_id = store.activity_max_id().await.expect("max id");
    assert_eq!(max_id, a_live.max(a_main).max(a_plan).max(a_nulls));

    // ---- ActivityList (whole task): after 0, ascending id.
    let (list, cursor) = store.activity_list(None, 0, 0).await.expect("list all");
    // 7 rows appended (a1, think, usage, plan, main, live, nulls).
    assert_eq!(list.len(), 7);
    assert_eq!(cursor, max_id);
    assert!(list.windows(2).all(|w| w[0].id < w[1].id), "ascending id");
    // Full rows carry metadata + main_seg.
    let a1_row = list.iter().find(|a| a.id == a1).expect("a1 in list");
    assert_eq!(a1_row.metadata["key"], "v1");
    assert_eq!(a1_row.summary, "step one");
    assert_eq!(a1_row.tool, "curl");
    assert_eq!(a1_row.tool_use_id, "tu_1");
    assert_eq!(a1_row.input_tokens, Some(100));
    assert_eq!(a1_row.node_id, Some(done_intent));
    let main_row = list.iter().find(|a| a.id == a_main).expect("main in list");
    assert_eq!(main_row.main_seg, Some(2));

    // ---- ActivityList incremental: after a1 -> only later rows, cursor advances.
    let (after, cursor2) = store
        .activity_list(None, a1, 0)
        .await
        .expect("list after a1");
    assert!(after.iter().all(|a| a.id > a1));
    assert_eq!(cursor2, max_id);

    // ---- ActivityList filtered by node (done_intent).
    let (node_list, _) = store
        .activity_list(Some(done_intent), 0, 0)
        .await
        .expect("list node");
    assert!(
        node_list.iter().all(|a| a.node_id == Some(done_intent)),
        "only done_intent rows"
    );
    assert!(node_list.iter().any(|a| a.id == a1));

    // ---- ActivityListForTerminalIntent: only terminal intent rows, no thinking/usage.
    let (term, term_cursor) = store
        .activity_list_for_terminal_intent(done_intent, 0, 0)
        .await
        .expect("list terminal");
    assert!(
        term.iter()
            .all(|a| a.kind != "thinking" && a.kind != "usage"),
        "no thinking/usage"
    );
    assert!(term.iter().any(|a| a.id == a1), "a1 (result) included");
    assert!(term.iter().all(|a| a.id != a_live), "live intent excluded");
    assert_eq!(
        term_cursor, a1,
        "cursor is the max id among terminal rows (a_nulls has NULL kind -> excluded)"
    );

    // ---- ActivityDetail: full blob, "" when missing.
    let detail = store.activity_detail(a1).await.expect("detail a1");
    assert_eq!(detail, "full detail one");
    let missing = store
        .activity_detail(99_999_999)
        .await
        .expect("detail missing");
    assert_eq!(missing, "");

    // ---- ActivityByIDs: full detail for specific ids, ascending, no thinking.
    let by_ids = store
        .activity_by_ids(&[a1, _a_think, a_plan])
        .await
        .expect("by ids");
    // thinking is skipped; a1 + a_plan remain, in ascending id order.
    let bids: Vec<i64> = by_ids.iter().map(|a| a.id).collect();
    assert_eq!(
        bids,
        vec![a1.min(a_plan), a1.max(a_plan)],
        "ascending, no thinking"
    );
    let a1_d = by_ids.iter().find(|a| a.id == a1).expect("a1 detail");
    assert_eq!(a1_d.detail, "full detail one");
    assert_eq!(a1_d.kind, "result");
    assert_eq!(a1_d.tool, "curl");
    assert!(!a1_d.is_error);
    // Empty -> empty.
    assert!(store
        .activity_by_ids(&[])
        .await
        .expect("by ids empty")
        .is_empty());

    // ---- ActivityByIDsForTerminalIntents: only terminal-intent rows with detail.
    let term_ids = store
        .activity_by_ids_for_terminal_intents(&[a1, a_live, a_plan])
        .await
        .expect("by ids terminal");
    // a_live belongs to a live intent -> excluded; a_plan is node-less -> excluded.
    let tids: Vec<i64> = term_ids.iter().map(|a| a.id).collect();
    assert_eq!(tids, vec![a1], "only terminal-intent result rows");

    // ---- ActivityPage (reverse pagination, ascending output).
    let (page, has_more) = store
        .activity_page(None, false, None, "", 0, 3)
        .await
        .expect("page latest 3");
    assert_eq!(page.len(), 3, "3 newest steps");
    assert!(has_more, "older steps exist");
    assert!(
        page.windows(2).all(|w| w[0].id < w[1].id),
        "ascending for display"
    );
    // The newest page ends at max_id.
    assert_eq!(page.last().unwrap().id, max_id);

    // Paging backwards from the oldest id on the first page.
    let oldest_on_page = page[0].id;
    let (page2, has_more2) = store
        .activity_page(None, false, None, "", oldest_on_page, 3)
        .await
        .expect("page before oldest");
    assert!(
        page2.iter().all(|a| a.id < oldest_on_page),
        "strictly older"
    );
    // 7 total, first page had 3 newest; older = 4, so page2 has 3 + has_more true.
    assert_eq!(page2.len(), 3);
    assert!(has_more2, "one more older step remains");

    // ---- ActivityPage filtered by worker (planner).
    let (plan_page, _) = store
        .activity_page(None, false, None, "planner", 0, 10)
        .await
        .expect("page planner");
    assert!(plan_page.iter().all(|a| a.worker == "planner"));
    assert_eq!(plan_page.len(), 1);

    // ---- ActivityPage filtered by main session, segment 2.
    let (main_page, _) = store
        .activity_page(None, true, Some(2), "", 0, 10)
        .await
        .expect("page main seg 2");
    assert!(main_page.iter().all(|a| a.worker == "mainagent"));
    assert_eq!(main_page.len(), 1);
    assert_eq!(main_page[0].id, a_main);

    // ---- ActivityPageForTerminalIntent: terminal-only, no thinking/usage.
    let (tpage, tmore) = store
        .activity_page_for_terminal_intent(done_intent, 0, 0)
        .await
        .expect("term page");
    assert!(
        tpage
            .iter()
            .all(|a| a.kind != "thinking" && a.kind != "usage"),
        "no thinking/usage in terminal page"
    );
    assert!(tpage.iter().any(|a| a.id == a1));
    assert!(tpage.iter().all(|a| a.id != a_live), "live intent excluded");
    assert!(!tmore, "fewer than limit terminal steps");

    // Cleanup.
    d.execute("DELETE FROM explorations WHERE id=$1", &[&exp_id])
        .await
        .expect("clean exp");
}
