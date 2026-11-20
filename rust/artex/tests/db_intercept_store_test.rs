//! Differential tests for the `db/intercept.go` store slice — decision_source
//! classification (port of `intercept_source_test.go`) + rule/pending CRUD.
//! Runs against `ARTEX_PG_DSN`; helper test is pure.

use artex::db::intercept_store::{intercept_source, MODEL_REASON_PREFIX};

#[test]
fn intercept_source_classification() {
    // Mirrors Go TestInterceptSourceMatchesJudgeWriter + LegacyModelPrefix.
    for reason in [
        format!("{MODEL_REASON_PREFIX} allow: read file"),
        format!("{MODEL_REASON_PREFIX} deny: destructive"),
        format!("{MODEL_REASON_PREFIX} ask"),
        "[模型] 允许".to_string(),
        "[Model] fixture".to_string(),
    ] {
        assert_eq!(intercept_source(0, &reason), "model", "reason {reason:?}");
    }
    assert_eq!(intercept_source(0, "legacy reason"), "unknown");
    assert_eq!(
        intercept_source(7, "[model] rule wins"),
        "rule",
        "rule id wins"
    );
}

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

#[tokio::test]
async fn rule_and_pending_crud() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let name = format!(
        "t_rule_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let r = d
        .create_intercept_rule(
            &name,
            "tool_name",
            "string",
            "Bash",
            "ask",
            "confirm",
            10,
            true,
            false,
            0,
            "allow",
        )
        .await
        .unwrap();
    assert!(r.id > 0 && r.enabled && r.priority == 10);
    assert!(d
        .list_intercept_rules()
        .await
        .unwrap()
        .iter()
        .any(|x| x.id == r.id));

    let upd = d
        .update_intercept_rule(
            r.id,
            &name,
            "tool_name",
            "string",
            "Bash",
            "deny",
            "blocked",
            20,
            false,
            true,
            30,
            "deny",
        )
        .await
        .unwrap();
    assert_eq!(upd.action, "deny");
    assert_eq!(upd.priority, 20);
    assert!(!upd.enabled);
    d.toggle_intercept_rule(r.id, true).await.unwrap();

    // Pending row with derived source.
    let pid = d
        .create_intercept_pending(
            r.id,
            0,
            "",
            "agent",
            "Bash",
            br#"{"command":"ls"}"#,
            "rule message",
            None,
        )
        .await
        .unwrap();
    let p = d.get_intercept_pending(pid).await.unwrap().unwrap();
    assert_eq!(p.decision_source, "rule");
    assert_eq!(p.status, "pending");
    assert!(d
        .list_pending_intercepts()
        .await
        .unwrap()
        .iter()
        .any(|x| x.id == pid));
    d.decide_intercept_pending(pid, "allowed").await.unwrap();
    assert_eq!(
        d.get_intercept_pending(pid).await.unwrap().unwrap().status,
        "allowed"
    );

    // Model-decided pending from a rule-less judge.
    let pid2 = d
        .create_decided_intercept(
            0,
            0,
            "",
            "agent",
            "Bash",
            b"",
            "allowed",
            &format!("{MODEL_REASON_PREFIX} allow"),
            None,
        )
        .await
        .unwrap();
    let p2 = d.get_intercept_pending(pid2).await.unwrap().unwrap();
    assert_eq!(p2.decision_source, "model");
    assert_eq!(p2.decided_by, "model");

    d.delete_intercept_rule(r.id).await.unwrap();
    let _ = d
        .execute(
            "DELETE FROM intercept_pending WHERE id=ANY($1::bigint[])",
            &[&vec![pid, pid2]],
        )
        .await;
}
