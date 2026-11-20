//! Differential tests for the visibility slice of `db/config.go` — port of
//! `TestSkillVisibilityOffIsNotResurrectedBySeed` + agent/resource visibility.
//! Runs against `ARTEX_PG_DSN`; skips when unreachable.

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

async fn row_state(d: &artex::db::DB, agent_id: i64, skill: &str) -> (bool, bool) {
    let row = d
        .query_opt(
            "SELECT enabled FROM agent_skill_visibility WHERE agent_id=$1 AND skill_name=$2",
            &[&agent_id, &skill],
        )
        .await
        .unwrap();
    match row {
        Some(r) => (true, r.get(0)),
        None => (false, false),
    }
}

#[tokio::test]
async fn skill_visibility_off_is_not_resurrected() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let key = format!(
        "t_vis_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;
    let a = d.create_agent(&key, "Vis Agent", "").await.unwrap();
    let aid = a.id;
    let skill = format!("t-skill-{key}");

    // Never seeded.
    assert_eq!(row_state(&d, aid, &skill).await, (false, false));
    // Toggle ON -> row exists + enabled.
    d.toggle_skill_visibility(aid, &skill, true).await.unwrap();
    assert_eq!(row_state(&d, aid, &skill).await, (true, true));
    assert!(d.agent_skill_names(aid).await.unwrap().contains(&skill));
    assert!(d.skill_agents(&skill).await.unwrap().contains(&aid));
    // Toggle OFF -> row still exists but disabled (NOT deleted).
    d.toggle_skill_visibility(aid, &skill, false).await.unwrap();
    assert_eq!(
        row_state(&d, aid, &skill).await,
        (true, false),
        "row kept disabled"
    );
    assert!(!d.agent_skill_names(aid).await.unwrap().contains(&skill));
    assert!(!d.skill_agents(&skill).await.unwrap().contains(&aid));

    // SetAgentSkillVisibility replaces via disable + upsert.
    let other = format!("t-skill-other-{key}");
    d.set_agent_skill_visibility(aid, std::slice::from_ref(&other))
        .await
        .unwrap();
    assert_eq!(
        row_state(&d, aid, &skill).await,
        (true, false),
        "old row disabled not deleted"
    );
    assert_eq!(row_state(&d, aid, &other).await, (true, true));
    let names = d.agent_skill_names(aid).await.unwrap();
    assert_eq!(names, vec![other.clone()], "only new skill visible");

    // DeleteSkillVisibility removes all rows for a skill.
    d.delete_skill_visibility(&skill).await.unwrap();
    assert_eq!(row_state(&d, aid, &skill).await, (false, false));
    d.delete_skill_visibility(&other).await.unwrap();

    // Agent/resource visibility (mcp kind only per schema).
    d.set_agent_visibility_kind(aid, "mcp", &[11, 22])
        .await
        .unwrap();
    let visible = d.agent_visible(aid, "mcp").await.unwrap();
    assert!(visible.contains(&11) && visible.contains(&22));
    assert!(d.resource_agents("mcp", 11).await.unwrap().contains(&aid));
    // Toggle off deletes the row (as Go does).
    d.toggle_visibility(aid, "mcp", 11, false).await.unwrap();
    assert!(!d.agent_visible(aid, "mcp").await.unwrap().contains(&11));
    // Toggle on again.
    d.toggle_visibility(aid, "mcp", 33, true).await.unwrap();
    assert!(d.agent_visible(aid, "mcp").await.unwrap().contains(&33));

    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;
}
