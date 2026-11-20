//! Differential tests for `db/tools.go` — port of the key cases in
//! `db/tools_secrets_test.go` plus the built-in/custom CRUD surface.
//!
//! Runs against `ARTEX_PG_DSN` behind the shared lock; skips cleanly when the DSN
//! is unset/unreachable.

use artex::db::secrets::{set_active_secret_box, SecretBox};
use artex::db::tools::Tool;

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

fn install_test_secret_box() {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    set_active_secret_box(SecretBox::new(&key).expect("32-byte key"));
}

fn tool(key: &str, kind: &str, exec: serde_json::Value) -> Tool {
    Tool {
        key: key.to_string(),
        kind: kind.to_string(),
        exec,
        enabled: true,
        ..Tool::default()
    }
}

const EXEC_HTTP: &str = r#"{"method":"GET","url":"https://api.example/x","headers":{"Authorization":"Bearer tool-hdr-plain"},"body":"tool-body-plain","proxy":"http://u:tool-proxy-plain@p.example:3128"}"#;

#[tokio::test]
async fn custom_tool_exec_sealed_at_rest() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let key = format!("t_tool_seal_{}", std::process::id());
    let _ = d
        .execute("DELETE FROM tools WHERE key LIKE $1", &[&format!("{key}%")])
        .await;
    let want: serde_json::Value = serde_json::from_str(EXEC_HTTP).unwrap();
    d.create_custom_tool(&tool(&key, "http", want.clone()))
        .await
        .unwrap();

    // Raw JSONB is sealed (no plaintext), all getters read back the clear exec.
    let raw: String = d
        .query_one("SELECT exec::text FROM tools WHERE key=$1", &[&key])
        .await
        .unwrap()
        .get(0);
    assert!(raw.contains("__artex_enc"), "sealed at rest: {raw}");
    assert!(!raw.contains("tool-hdr-plain"), "no plaintext at rest");
    assert!(!raw.contains("tool-body-plain"), "no plaintext at rest");
    assert_eq!(d.get_tool(&key).await.unwrap().unwrap().exec, want);

    let (list, customs) = (
        d.list_tools().await.unwrap(),
        d.list_custom_tools().await.unwrap(),
    );
    assert!(list.iter().any(|t| t.key == key && t.exec == want));
    assert!(customs.iter().any(|t| t.key == key && t.exec == want));

    // Re-seal under a fresh nonce: the same exec must not produce the same bytes.
    let first = raw;
    let cmd: serde_json::Value = serde_json::from_str(
        r#"{"command":"curl -H 'X-Key: tool-cmd-plain' {args}","timeout_ms":5000}"#,
    )
    .unwrap();
    d.update_custom_tool(&tool(&key, "command", cmd.clone()))
        .await
        .unwrap();
    let second: String = d
        .query_one("SELECT exec::text FROM tools WHERE key=$1", &[&key])
        .await
        .unwrap()
        .get(0);
    assert_ne!(second, first, "fresh nonce on re-seal");
    assert_eq!(d.get_tool(&key).await.unwrap().unwrap().exec, cmd);

    // Empty exec stores as `{}` (nothing to hide).
    let shell_key = format!("{key}_shell");
    d.create_custom_tool(&tool(&shell_key, "shell", serde_json::Value::Null))
        .await
        .unwrap();
    let shell_raw: String = d
        .query_one("SELECT exec::text FROM tools WHERE key=$1", &[&shell_key])
        .await
        .unwrap()
        .get(0);
    assert_eq!(shell_raw, "{}", "empty exec stays {{}}");

    // Reserved __artex_enc key refused on write.
    let reserved = serde_json::json!({"__artex_enc": "x"});
    let bad_key = format!("{key}_resv");
    assert!(
        d.create_custom_tool(&tool(&bad_key, "command", reserved))
            .await
            .is_err(),
        "reserved key refused"
    );

    // A system tool's exec stays in clear text.
    let sys_key = format!("{key}_sys");
    d.seed_tool(
        &sys_key,
        "sys",
        &serde_json::json!({}),
        &serde_json::json!([]),
    )
    .await
    .unwrap();
    d.execute(
        "UPDATE tools SET kind='command', exec='{\"command\":\"echo system-plain\"}'::jsonb WHERE key=$1",
        &[&sys_key],
    )
    .await
    .unwrap();
    let sys_raw: String = d
        .query_one("SELECT exec::text FROM tools WHERE key=$1", &[&sys_key])
        .await
        .unwrap()
        .get(0);
    assert!(sys_raw.contains("system-plain"), "system exec stays clear");
    assert_eq!(
        d.get_tool(&sys_key).await.unwrap().unwrap().exec["command"],
        "echo system-plain"
    );

    // Cleanup.
    let _ = d
        .execute("DELETE FROM tools WHERE key LIKE $1", &[&format!("{key}%")])
        .await;
}

#[tokio::test]
async fn builtin_seed_upsert_refresh_and_bindings() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_test_secret_box();

    let key = format!("t_tool_crud_{}", std::process::id());
    let _ = d
        .execute("DELETE FROM tools WHERE key LIKE $1", &[&format!("{key}%")])
        .await;
    let schema = serde_json::json!({"type":"object","properties":{"q":{"description":"d"}}});
    let agents = serde_json::json!([]);

    // Seed is first-insert-only.
    d.seed_tool(&key, "original", &schema, &agents)
        .await
        .unwrap();
    d.seed_tool(&key, "should-not-win", &schema, &agents)
        .await
        .unwrap();
    assert_eq!(
        d.get_tool(&key).await.unwrap().unwrap().description,
        "original"
    );
    assert!(d.get_tool(&key).await.unwrap().unwrap().system);

    // Agent bindings: add (idempotent) / remove from single / remove from all.
    d.add_agent_to_tool_binding("agent-a", &[key.clone(), key.clone()])
        .await
        .unwrap();
    assert_eq!(
        d.get_tool(&key).await.unwrap().unwrap().agents,
        vec!["agent-a".to_string()],
        "idempotent add"
    );
    d.add_agent_to_tool_binding("agent-b", std::slice::from_ref(&key))
        .await
        .unwrap();
    assert_eq!(d.get_tool(&key).await.unwrap().unwrap().agents.len(), 2);
    d.remove_agent_from_tool("agent-a", &key).await.unwrap();
    assert_eq!(
        d.get_tool(&key).await.unwrap().unwrap().agents,
        vec!["agent-b".to_string()]
    );
    d.remove_agent_from_tool_bindings("agent-b").await.unwrap();
    assert!(d.get_tool(&key).await.unwrap().unwrap().agents.is_empty());

    // UpdateTool (page-editable fields).
    d.update_tool(
        &key,
        "updated",
        &schema,
        &serde_json::json!(["agent-c"]),
        false,
    )
    .await
    .unwrap();
    let t = d.get_tool(&key).await.unwrap().unwrap();
    assert_eq!(t.description, "updated");
    assert!(!t.enabled);
    assert_eq!(t.agents, vec!["agent-c".to_string()]);

    // RefreshToolDefaults: desc/schema change, agents + enabled preserved.
    let new_schema = serde_json::json!({"type":"object","properties":{"q":{"description":"d2"}}});
    d.refresh_tool_defaults(&key, "refreshed", &new_schema)
        .await
        .unwrap();
    let t = d.get_tool(&key).await.unwrap().unwrap();
    assert_eq!(t.description, "refreshed");
    assert_eq!(t.schema, new_schema);
    assert_eq!(t.agents, vec!["agent-c".to_string()], "binding preserved");
    assert!(!t.enabled, "enabled preserved");

    // UpsertToolForce overwrites defaults + re-enables.
    d.upsert_tool_force(&key, "forced", &schema, &serde_json::json!(["agent-z"]))
        .await
        .unwrap();
    let t = d.get_tool(&key).await.unwrap().unwrap();
    assert_eq!(t.description, "forced");
    assert!(t.enabled);
    assert_eq!(t.agents, vec!["agent-z".to_string()]);

    // DeleteCustomTool protects system rows.
    d.delete_custom_tool(&key).await.unwrap();
    assert!(
        d.get_tool(&key).await.unwrap().is_some(),
        "system tool protected"
    );

    // A custom tool can be deleted.
    let ck = format!("{key}_custom");
    d.create_custom_tool(&tool(&ck, "http", serde_json::json!({})))
        .await
        .unwrap();
    assert!(d.get_tool(&ck).await.unwrap().is_some());
    d.delete_custom_tool(&ck).await.unwrap();
    assert!(d.get_tool(&ck).await.unwrap().is_none());

    assert!(d.get_tool("no-such-tool").await.unwrap().is_none());

    let _ = d
        .execute("DELETE FROM tools WHERE key LIKE $1", &[&format!("{key}%")])
        .await;
}
