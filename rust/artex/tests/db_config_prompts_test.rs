//! Differential tests for the prompt slice of `db/config.go` — version append,
//! seed-if-empty idempotence, reset, current pointer, ordering. Runs against
//! `ARTEX_PG_DSN`; skips when unreachable.

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
async fn prompt_versions_seed_and_current() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let key = format!(
        "t_prompt_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;
    let agent = d.create_agent(&key, "Prompt Agent", "").await.unwrap();
    let aid = agent.id;

    // No prompt yet.
    assert_eq!(d.current_prompt(aid).await.unwrap(), "");
    assert!(d.list_prompt_versions(aid).await.unwrap().is_empty());

    // Seed default -> version 1; second seed is a no-op.
    d.seed_prompt_if_empty(aid, "default template")
        .await
        .unwrap();
    assert_eq!(d.current_prompt(aid).await.unwrap(), "default template");
    let versions = d.list_prompt_versions(aid).await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, 1);
    assert_eq!(versions[0].note, "built-in default");
    d.seed_prompt_if_empty(aid, "SHOULD NOT WIN").await.unwrap();
    assert_eq!(
        d.current_prompt(aid).await.unwrap(),
        "default template",
        "seed is first-insert-only"
    );

    // Save a user edit -> version 2, current moves.
    let ver = d
        .save_prompt(aid, "user edit", "my note", "tester")
        .await
        .unwrap();
    assert_eq!(ver, 2);
    assert_eq!(d.current_prompt(aid).await.unwrap(), "user edit");
    let versions = d.list_prompt_versions(aid).await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version, 2, "newest first");
    assert_eq!(versions[0].note, "my note");
    assert_eq!(versions[1].version, 1);

    // Reset to built-in -> version 3.
    let ver = d
        .reset_prompt_to_default(aid, "default template")
        .await
        .unwrap();
    assert_eq!(ver, 3);
    assert_eq!(d.current_prompt(aid).await.unwrap(), "default template");
    assert_eq!(d.list_prompt_versions(aid).await.unwrap().len(), 3);

    // Prompt vars (seeded raw).
    let _ = d
        .execute("DELETE FROM agent_prompt_vars WHERE agent_id=$1", &[&aid])
        .await;
    d.execute(
        "INSERT INTO agent_prompt_vars(agent_id,var_name,description,example,source) VALUES ($1,'Z','d','e','runtime')",
        &[&aid],
    ).await.unwrap();
    d.execute(
        "INSERT INTO agent_prompt_vars(agent_id,var_name,description,example,source) VALUES ($1,'A','','','exploration')",
        &[&aid],
    ).await.unwrap();
    let vars = d.prompt_vars(aid).await.unwrap();
    assert_eq!(vars.len(), 2);
    assert_eq!(vars[0].name, "A", "ordered by name");
    assert_eq!(vars[1].name, "Z");

    let _ = d.execute("DELETE FROM agents WHERE key=$1", &[&key]).await;
}
