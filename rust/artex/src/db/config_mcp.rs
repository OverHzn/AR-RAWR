//! MCP server store — slice of `db/config.go` (MCP servers + tool cache).
//!
//! `env` is sealed at rest; `args` is plain JSON. Deleting a server also clears
//! its agent-visibility rows in the same tx.

use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::secrets::{open_secret, seal_secret, COL_MCP_ENV};
use crate::db::DB;

#[derive(Debug, Clone, PartialEq)]
pub struct MCPServer {
    pub id: i64,
    pub name: String,
    pub transport: String,
    pub command: String,
    pub args: serde_json::Value,
    /// Clear env spec (sealed at rest).
    pub env: serde_json::Value,
    pub url: String,
    pub enabled: bool,
    pub insecure: bool,
    pub timeout_seconds: i32,
    pub last_connect_at: Option<SystemTime>,
    pub last_connect_ok: Option<bool>,
    pub last_connect_err: Option<String>,
    pub tools: Vec<String>,
}

impl Default for MCPServer {
    fn default() -> Self {
        MCPServer {
            id: 0,
            name: String::new(),
            transport: String::new(),
            command: String::new(),
            args: serde_json::json!([]),
            env: serde_json::json!({}),
            url: String::new(),
            enabled: false,
            insecure: false,
            timeout_seconds: 0,
            last_connect_at: None,
            last_connect_ok: None,
            last_connect_err: None,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MCPTool {
    pub name: String,
    pub description: String,
}

const MCP_COLS: &str = "id,name,transport,COALESCE(command,''),args,env,COALESCE(url,''),enabled,insecure,COALESCE(timeout_seconds,120),last_connect_at,last_connect_ok,last_connect_err";

impl DB {
    /// `ListMCP`: all servers by id, env opened, cached tool names attached.
    pub async fn list_mcp(&self) -> Result<Vec<MCPServer>> {
        let rows = self
            .query(
                &format!("SELECT {MCP_COLS} FROM mcp_servers ORDER BY id"),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let env_raw: serde_json::Value = r.get(5);
            let env_json = env_raw.to_string();
            let plain = open_secret(&COL_MCP_ENV, &env_json).map_err(crate::db::Error::msg)?;
            let mut m = MCPServer {
                id: r.get(0),
                name: r.get(1),
                transport: r.get(2),
                command: r.get(3),
                args: r.get(4),
                env: serde_json::from_str(&plain).unwrap_or(serde_json::json!({})),
                url: r.get(6),
                enabled: r.get(7),
                insecure: r.get(8),
                timeout_seconds: r.get(9),
                last_connect_at: r.get(10),
                last_connect_ok: r.get(11),
                last_connect_err: r.get(12),
                tools: Vec::new(),
            };
            m.tools = self.mcp_tool_names(m.id).await?;
            out.push(m);
        }
        Ok(out)
    }

    /// `MCPToolNames`: cached tool names for one server.
    pub async fn mcp_tool_names(&self, server_id: i64) -> Result<Vec<String>> {
        let rows = self
            .query(
                "SELECT tool_name FROM mcp_tools_cache WHERE server_id=$1 ORDER BY tool_name",
                &[&server_id],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// `MCPToolsDetailed`: cached tools with descriptions.
    pub async fn mcp_tools_detailed(&self, server_id: i64) -> Result<Vec<MCPTool>> {
        let rows = self
            .query(
                "SELECT tool_name, COALESCE(description,'') FROM mcp_tools_cache WHERE server_id=$1 ORDER BY tool_name",
                &[&server_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| MCPTool {
                name: r.get(0),
                description: r.get(1),
            })
            .collect())
    }

    /// `SaveMCPTools`: replace the cached tool list for a server.
    pub async fn save_mcp_tools(&self, server_id: i64, tools: &[MCPTool]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute("DELETE FROM mcp_tools_cache WHERE server_id=$1", &[&server_id]).await?;
            for t in tools {
                let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] =
                    [&server_id, &t.name, &t.description];
                tx.execute(
                    "INSERT INTO mcp_tools_cache(server_id, tool_name, description) VALUES ($1,$2,$3) \
                     ON CONFLICT (server_id, tool_name) DO UPDATE SET description=EXCLUDED.description",
                    &params,
                ).await?;
            }
            Ok(())
        }
        .await;
        match run {
            Ok(()) => {
                tx.commit().await?;
                Ok(())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `UpdateMCPHealth`: record one connect attempt (skip no-op writes).
    pub async fn update_mcp_health(&self, id: i64, ok: bool, err_msg: &str) -> Result<()> {
        let err_opt: Option<&str> = if err_msg.is_empty() {
            None
        } else {
            Some(err_msg)
        };
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&id, &ok, &err_opt];
        self.execute(
            "UPDATE mcp_servers SET last_connect_at = now(), last_connect_ok = $2, \
             last_connect_err = NULLIF($3, '') WHERE id = $1 \
             AND (last_connect_ok IS DISTINCT FROM $2 OR last_connect_err IS DISTINCT FROM NULLIF($3, '') OR last_connect_at IS NULL)",
            &params,
        ).await?;
        Ok(())
    }

    /// `SaveMCP`: insert (id==0) or update a server; env sealed at rest.
    pub async fn save_mcp(&self, m: &MCPServer) -> Result<i64> {
        let args: serde_json::Value = if m.args.is_null() {
            serde_json::json!([])
        } else {
            m.args.clone()
        };
        let env = if m.env.is_null() {
            "{}".to_string()
        } else {
            m.env.to_string()
        };
        let env_sealed = seal_secret(&COL_MCP_ENV, &env).map_err(crate::db::Error::msg)?;
        let env_val: serde_json::Value =
            serde_json::from_str(&env_sealed).map_err(|e| crate::db::Error::msg(e.to_string()))?;
        let timeout = if m.timeout_seconds <= 0 {
            120
        } else {
            m.timeout_seconds
        };
        let cmd_opt: Option<&str> = if m.command.is_empty() {
            None
        } else {
            Some(&m.command)
        };
        let url_opt: Option<&str> = if m.url.is_empty() { None } else { Some(&m.url) };
        if m.id == 0 {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 9] = [
                &m.name,
                &m.transport,
                &cmd_opt,
                &args,
                &env_val,
                &url_opt,
                &m.enabled,
                &m.insecure,
                &timeout,
            ];
            let row = self.query_one(
                "INSERT INTO mcp_servers(name,transport,command,args,env,url,enabled,insecure,timeout_seconds) \
                 VALUES ($1,$2,NULLIF($3,''),$4,$5,NULLIF($6,''),$7,$8,$9) RETURNING id",
                &params,
            ).await?;
            Ok(row.get(0))
        } else {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 10] = [
                &m.name,
                &m.transport,
                &cmd_opt,
                &args,
                &env_val,
                &url_opt,
                &m.enabled,
                &m.insecure,
                &timeout,
                &m.id,
            ];
            self.execute(
                "UPDATE mcp_servers SET name=$1,transport=$2,command=NULLIF($3,''),args=$4,env=$5,url=NULLIF($6,''),enabled=$7,insecure=$8,timeout_seconds=$9 WHERE id=$10",
                &params,
            ).await?;
            Ok(m.id)
        }
    }

    /// `DeleteMCP`: remove a server + its agent visibility in one tx.
    pub async fn delete_mcp(&self, id: i64) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "DELETE FROM agent_visibility WHERE resource_kind='mcp' AND resource_id=$1",
                &[&id],
            )
            .await?;
            tx.execute("DELETE FROM mcp_servers WHERE id=$1", &[&id])
                .await?;
            Ok(())
        }
        .await;
        match run {
            Ok(()) => {
                tx.commit().await?;
                Ok(())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}
