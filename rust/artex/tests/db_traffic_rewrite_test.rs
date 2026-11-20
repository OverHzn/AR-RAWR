//! Differential tests for `db/traffic_rewrite.go` — ports of
//! `TestTrafficRewriteRuleCRUDSealed` + `TestTrafficRewriteRuleLimits`.
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::secrets::{set_active_secret_box, SecretBox};
use artex::db::traffic_rewrite::{
    TrafficRewriteRule, ERR_TRAFFIC_REWRITE_ENABLED_LIMIT, ERR_TRAFFIC_REWRITE_RULE_LIMIT,
    MAX_ENABLED_TRAFFIC_REWRITE_RULES, MAX_TRAFFIC_REWRITE_RULES,
};

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

fn install_box() {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    set_active_secret_box(SecretBox::new(&key).unwrap());
}

const MATCHERS: &str =
    r#"{"host":"api.example","headers":[{"name":"X-Env","value_regex":"rw-matcher-plain"}]}"#;
const ACTIONS: &str = r#"[{"type":"set_header","name":"Authorization","value":"Bearer rw-action-plain","sensitive":true}]"#;

fn rule(name: &str, enabled: bool) -> TrafficRewriteRule {
    TrafficRewriteRule {
        name: name.to_string(),
        enabled,
        phase: "request".to_string(),
        matchers: serde_json::json!({}),
        actions: serde_json::json!([{"type":"delete_header","name":"X-A"}]),
        ..TrafficRewriteRule::default()
    }
}

async fn clean(d: &artex::db::DB) {
    let _ = d.execute("DELETE FROM traffic_rewrite_rules", &[]).await;
}

#[tokio::test]
async fn crud_sealed_and_limits() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_box();
    clean(&d).await;

    // Create: fields preserved, sealed at rest, clear via getters.
    let in_rule = TrafficRewriteRule {
        name: "t-rw-crud".to_string(),
        phase: "response".to_string(),
        priority: 7,
        note: "n".to_string(),
        matchers: serde_json::from_str(MATCHERS).unwrap(),
        actions: serde_json::from_str(ACTIONS).unwrap(),
        ..TrafficRewriteRule::default()
    };
    let r = d.create_rewrite_rule(&in_rule).await.unwrap();
    assert!(r.id > 0 && !r.enabled && r.phase == "response" && r.priority == 7 && r.note == "n");

    let (m_raw, a_raw): (String, String) = {
        let row = d
            .query_one(
                "SELECT matchers::text, actions::text FROM traffic_rewrite_rules WHERE id=$1",
                &[&r.id],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert!(
        m_raw.starts_with("{\"__artex_enc\""),
        "matchers sealed: {m_raw}"
    );
    assert!(
        a_raw.starts_with("{\"__artex_enc\""),
        "actions sealed: {a_raw}"
    );
    assert!(!m_raw.contains("rw-matcher-plain") && !a_raw.contains("rw-action-plain"));
    let got = d.get_rewrite_rule(r.id).await.unwrap();
    assert_eq!(got.matchers, in_rule.matchers);
    assert_eq!(got.actions, in_rule.actions);

    // Empty matcher object stays clear (nothing to hide), actions still sealed.
    let empty = d
        .create_rewrite_rule(&rule("t-rw-empty", false))
        .await
        .unwrap();
    let (m, _): (String, String) = {
        let row = d
            .query_one(
                "SELECT matchers::text, actions::text FROM traffic_rewrite_rules WHERE id=$1",
                &[&empty.id],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(m, "{}", "empty matchers stay clear");

    // Update: edit sees stored in clear; new nonce; failed edit changes nothing.
    let updated = d
        .update_rewrite_rule(r.id, |stored| {
            assert_eq!(stored.actions, in_rule.actions, "edit sees stored clear");
            let mut next = stored;
            next.name = "t-rw-crud2".to_string();
            next.priority = -1;
            next.note.clear();
            next.enabled = true;
            Ok(next)
        })
        .await
        .unwrap();
    assert_eq!(updated.name, "t-rw-crud2");
    assert!(updated.enabled && updated.priority == -1);
    let (m2, _a2): (String, String) = {
        let row = d
            .query_one(
                "SELECT matchers::text, actions::text FROM traffic_rewrite_rules WHERE id=$1",
                &[&r.id],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_ne!(m2, m_raw, "fresh nonce on update");
    let failed = d
        .update_rewrite_rule(r.id, |mut s| {
            s.name = "t-rw-never".to_string();
            Err(artex::db::Error::msg("refused"))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(d.get_rewrite_rule(r.id).await.unwrap().name, "t-rw-crud2");

    // Execution order: priority asc then id asc. r has priority -1, empty 0.
    let list = d.list_rewrite_rules().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, r.id, "lower priority first");

    // Toggle + delete; missing rule -> no-rows error on every path.
    d.toggle_rewrite_rule(r.id, false).await.unwrap();
    assert!(!d.get_rewrite_rule(r.id).await.unwrap().enabled);
    d.delete_rewrite_rule(r.id).await.unwrap();
    d.delete_rewrite_rule(empty.id).await.unwrap();
    assert!(d.get_rewrite_rule(r.id).await.is_err());
    assert!(d.toggle_rewrite_rule(r.id, true).await.is_err());
    assert!(d.delete_rewrite_rule(r.id).await.is_err());

    // Limits: 100 rules, 50 enabled.
    let mut enabled_ids = Vec::new();
    let mut disabled_ids = Vec::new();
    for i in 0..MAX_TRAFFIC_REWRITE_RULES as i32 {
        let on = (i as i64) < MAX_ENABLED_TRAFFIC_REWRITE_RULES;
        let rr = d
            .create_rewrite_rule(&rule(&format!("t-rw-limit-{i:03}"), on))
            .await
            .unwrap();
        if on {
            enabled_ids.push(rr.id);
        } else {
            disabled_ids.push(rr.id);
        }
    }
    let over = d.create_rewrite_rule(&rule("t-rw-limit-over", false)).await;
    assert!(over.is_err() && over.unwrap_err().is(ERR_TRAFFIC_REWRITE_RULE_LIMIT));
    let enable = |mut s: TrafficRewriteRule| -> artex::db::Result<TrafficRewriteRule> {
        s.enabled = true;
        Ok(s)
    };
    let t51 = d.toggle_rewrite_rule(disabled_ids[0], true).await;
    assert!(t51.is_err() && t51.unwrap_err().is(ERR_TRAFFIC_REWRITE_ENABLED_LIMIT));
    let u51 = d.update_rewrite_rule(disabled_ids[0], enable).await;
    assert!(u51.is_err() && u51.unwrap_err().is(ERR_TRAFFIC_REWRITE_ENABLED_LIMIT));
    // At the limit an enabled rule can still be saved/toggled.
    assert!(d.update_rewrite_rule(enabled_ids[0], enable).await.is_ok());
    assert!(d.toggle_rewrite_rule(enabled_ids[0], true).await.is_ok());
    // Free one slot -> enabling works.
    d.delete_rewrite_rule(disabled_ids[1]).await.unwrap();
    let create_on = d.create_rewrite_rule(&rule("t-rw-limit-on", true)).await;
    assert!(create_on.is_err() && create_on.unwrap_err().is(ERR_TRAFFIC_REWRITE_ENABLED_LIMIT));
    d.toggle_rewrite_rule(enabled_ids[1], false).await.unwrap();
    assert!(d.toggle_rewrite_rule(disabled_ids[0], true).await.is_ok());

    let (total, on): (i64, i64) = {
        let row = d
            .query_one(
                "SELECT count(*), count(*) FILTER (WHERE enabled) FROM traffic_rewrite_rules",
                &[],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(total, MAX_TRAFFIC_REWRITE_RULES - 1);
    assert_eq!(on, MAX_ENABLED_TRAFFIC_REWRITE_RULES);

    clean(&d).await;
}
