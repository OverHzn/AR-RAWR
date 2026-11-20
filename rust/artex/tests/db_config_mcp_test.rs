//! Differential tests for the MCP slice of `db/config.go` — CRUD, sealed env,
//! tool cache, health, delete cascade. Runs against `ARTEX_PG_DSN`; skips when unreachable.

use artex::db::config_mcp::{MCPServer, MCPTool};
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
async fn mcp_crud_sealed_env_tools_health_delete() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    set_active_secret_box(SecretBox::new(b"0123456789abcdef0123456789abcdef").unwrap());
    let name = format!(
        "t_mcp_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _ = d
        .execute("DELETE FROM mcp_servers WHERE name=$1", &[&name])
        .await;

    let mut m = MCPServer {
        name: name.clone(),
        transport: "stdio".to_string(),
        command: "npx".to_string(),
        args: serde_json::json!(["-y", "@x/mcp"]),
        env: serde_json::json!({"API_KEY": "secret-token-xyz"}),
        enabled: true,
        timeout_seconds: 0, // -> 120
        ..MCPServer::default()
    };
    let id = d.save_mcp(&m).await.unwrap();
    assert!(id > 0);

    // Env sealed at rest; clear via list.
    let raw_env: serde_json::Value = {
        let row = d
            .query_one("SELECT env FROM mcp_servers WHERE id=$1", &[&id])
            .await
            .unwrap();
        row.get(0)
    };
    let raw = raw_env.to_string();
    assert!(raw.contains("__artex_enc"), "env sealed: {raw}");
    assert!(!raw.contains("secret-token-xyz"));
    let listed = d.list_mcp().await.unwrap();
    let got = listed.iter().find(|x| x.id == id).unwrap();
    assert_eq!(got.env, m.env);
    assert_eq!(got.args, m.args);
    assert_eq!(got.timeout_seconds, 120);
    assert!(got.enabled);

    // Update via id.
    m.id = id;
    m.name = format!("{name}-2");
    m.transport = "http".to_string();
    m.timeout_seconds = 45;
    let id2 = d.save_mcp(&m).await.unwrap();
    assert_eq!(id2, id);

    // Tool cache.
    d.save_mcp_tools(
        id,
        &[
            MCPTool {
                name: "zeta".to_string(),
                description: "Z".to_string(),
            },
            MCPTool {
                name: "alpha".to_string(),
                description: "A".to_string(),
            },
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        d.mcp_tool_names(id).await.unwrap(),
        vec!["alpha".to_string(), "zeta".to_string()]
    );
    let detailed = d.mcp_tools_detailed(id).await.unwrap();
    assert_eq!(detailed[0].name, "alpha");
    assert_eq!(detailed[0].description, "A");
    // Replace cache.
    d.save_mcp_tools(
        id,
        &[MCPTool {
            name: "beta".to_string(),
            description: "B".to_string(),
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        d.mcp_tool_names(id).await.unwrap(),
        vec!["beta".to_string()]
    );

    // Health update.
    d.update_mcp_health(id, true, "").await.unwrap();
    let (ok, err): (Option<bool>, Option<String>) = {
        let row = d
            .query_one(
                "SELECT last_connect_ok, last_connect_err FROM mcp_servers WHERE id=$1",
                &[&id],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(ok, Some(true));
    assert_eq!(err, None);

    // Delete removes server + tools cache rows (FK) + visibility.
    d.delete_mcp(id).await.unwrap();
    assert!(d.list_mcp().await.unwrap().iter().all(|x| x.id != id));
    let tools_left: i64 = d
        .query_one(
            "SELECT count(*) FROM mcp_tools_cache WHERE server_id=$1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(tools_left, 0);
}
