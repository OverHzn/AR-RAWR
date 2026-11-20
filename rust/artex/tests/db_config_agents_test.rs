//! Differential tests for the agent slice of `db/config.go` — CRUD, setters,
//! builtin protection, binding counts, profile bind. Runs against `ARTEX_PG_DSN`.

use artex::db::config_profiles::ERR_LLM_PROFILE_NOT_FOUND;
use artex::db::secrets::{set_active_secret_box, SecretBox};

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
async fn agent_crud_setters_and_protection() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let key = format!(
        "t_agent_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;

    // Create: custom, role assistant, enabled, default run_secs 600.
    let a = d.create_agent(&key, "My Agent", "").await.unwrap();
    assert!(!a.builtin && a.enabled && a.role == "assistant");
    assert!(a.run_secs > 0, "default run_seconds");
    assert_eq!(a.trigger_run_mode, "serial");
    assert_eq!(a.trigger_merge_mode, "all");

    // Get + list.
    let got = d.get_agent_by_key(&key).await.unwrap().unwrap();
    assert_eq!(got.name, "My Agent");
    assert!(d.list_agents().await.unwrap().iter().any(|x| x.key == key));
    assert!(d.get_agent_by_key("no-such-agent").await.unwrap().is_none());

    // Meta update.
    d.update_agent_meta(&key, "Renamed", "desc").await.unwrap();
    let got = d.get_agent_by_key(&key).await.unwrap().unwrap();
    assert_eq!(got.name, "Renamed");
    assert_eq!(got.description, "desc");

    // Setters (incl clamping + enum fallbacks).
    d.set_agent_max_turns(&key, -5).await.unwrap();
    d.set_agent_run_seconds(&key, -1).await.unwrap();
    d.set_agent_web_search(&key, true).await.unwrap();
    d.set_agent_interactive_shell(&key, true).await.unwrap();
    d.set_agent_wrapup_prompt(&key, "wrap").await.unwrap();
    d.set_agent_wrapup_max_turns(&key, 4).await.unwrap();
    d.set_agent_task_timeout_wrapup(&key, "timeout-wrap", 7)
        .await
        .unwrap();
    d.set_agent_trigger_behavior(&key, "bogus", "bogus", -3)
        .await
        .unwrap();
    let got = d.get_agent_by_key(&key).await.unwrap().unwrap();
    assert_eq!(got.max_turns, 0, "negative clamped");
    assert_eq!(got.run_secs, 0, "negative clamped");
    assert!(got.web_search && got.interactive_shell);
    assert_eq!(got.wrapup_prompt, "wrap");
    assert_eq!(got.wrapup_max_turns, 4);
    assert_eq!(got.task_timeout_wrapup_prompt, "timeout-wrap");
    assert_eq!(got.task_timeout_wrapup_max_turns, 7);
    assert_eq!(got.trigger_run_mode, "serial", "enum fallback");
    assert_eq!(got.trigger_merge_mode, "by_task", "enum fallback");
    assert_eq!(got.trigger_max_parallel, 0);

    // Binding counts (no N+1) — should be empty maps for this agent.
    let (mcp, skill, tools) = d.agent_binding_counts().await.unwrap();
    assert!(!mcp.contains_key(&got.id));
    assert!(!skill.contains_key(&got.id));
    assert!(!tools.contains_key(&key));

    // Delete custom agent.
    d.delete_agent(&key).await.unwrap();
    assert!(d.get_agent_by_key(&key).await.unwrap().is_none());
}

#[tokio::test]
async fn agent_profile_bind_and_builtin_protection() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let b = SecretBox::new(b"0123456789abcdef0123456789abcdef").unwrap();
    set_active_secret_box(b);
    let key = format!(
        "t_agentp_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;

    d.create_agent(&key, "Agent", "d").await.unwrap();
    // Built-in agents cannot be renamed/deleted via these methods.
    d.update_agent_meta("mainagent", "HACKED", "x")
        .await
        .unwrap();
    let main = d.get_agent_by_key("mainagent").await.unwrap();
    if let Some(m) = main {
        assert_ne!(m.name, "HACKED", "built-in meta protected");
    }
    d.delete_agent("mainagent").await.unwrap();
    assert!(
        d.get_agent_by_key("mainagent").await.unwrap().is_some(),
        "built-in delete protected"
    );

    // Bind to a profile, then clear.
    let mut prof = artex::db::config_profiles::LLMProfile {
        name: format!("t-agent-prof-{key}"),
        format: "openai".to_string(),
        model: "m".to_string(),
        api_key: "k".to_string(),
        ..artex::db::config_profiles::LLMProfile::default()
    };
    let pid = d.save_profile(&mut prof).await.unwrap();
    d.set_agent_llm_profile(&key, Some(pid)).await.unwrap();
    assert_eq!(
        d.get_agent_by_key(&key)
            .await
            .unwrap()
            .unwrap()
            .llm_profile_id,
        Some(pid)
    );
    d.set_agent_llm_profile(&key, None).await.unwrap();
    assert_eq!(
        d.get_agent_by_key(&key)
            .await
            .unwrap()
            .unwrap()
            .llm_profile_id,
        None
    );
    // Missing profile -> sentinel; binding unchanged.
    let err = d
        .set_agent_llm_profile(&key, Some(99_999_999))
        .await
        .expect_err("missing profile");
    assert!(err.is(ERR_LLM_PROFILE_NOT_FOUND), "got {err}");
    // Unknown agent is a no-op.
    d.set_agent_llm_profile("no-such-agent", Some(pid))
        .await
        .unwrap();

    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;
    let _ = d
        .execute("DELETE FROM llm_profiles WHERE id=$1", &[&pid])
        .await;
}
