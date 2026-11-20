//! Tools catalog — port of `db/tools.go`.
//!
//! `tools` is the page-editable surface of the built-in + custom tool catalog.
//! Custom tools (`system=false`) store their `exec` spec sealed at rest (JSON
//! envelope) and transparently open it on read via [`scan_tool`].

use serde_json::Value;

use crate::db::error::{Error, Result};
use crate::db::secrets::{open_secret, seal_secret, COL_TOOL_EXEC};
use crate::db::DB;

const TOOL_COLS: &str = "key, system, description, schema, agents, enabled, kind, exec, deferred";

/// `Tool`: one row of the tool catalog. `calls` is a runtime ledger aggregate,
/// not stored in the table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tool {
    pub key: String,
    pub system: bool,
    pub description: String,
    pub schema: Value,
    pub agents: Vec<String>,
    pub enabled: bool,
    pub kind: String,
    /// The execution spec — sealed at rest for `system=false` rows, transparently
    /// opened by every getter (never the raw ciphertext on this struct).
    pub exec: Value,
    pub deferred: bool,
    pub calls: i64,
}

fn scan_tool(row: &tokio_postgres::Row) -> Result<Tool> {
    let system: bool = row.get(1);
    let schema: Value = row.get(3);
    let agents_val: Value = row.get(4);
    let exec_val: Value = row.get(7);
    let exec = if system {
        exec_val
    } else {
        let sealed = exec_val.to_string();
        let plain = open_secret(&COL_TOOL_EXEC, &sealed).map_err(Error::msg)?;
        serde_json::from_str(&plain).unwrap_or(Value::Null)
    };
    let agents: Vec<String> = serde_json::from_value(agents_val).unwrap_or_default();
    Ok(Tool {
        key: row.get(0),
        system,
        description: row.get(2),
        schema,
        agents,
        enabled: row.get(5),
        kind: row.get(6),
        exec,
        deferred: row.get(8),
        calls: 0,
    })
}

/// `sealToolExec`: seal a custom tool's exec spec (empty/null -> `{}`). Returns
/// the JSONB value to bind (the sealed envelope is itself valid JSON).
fn seal_tool_exec(exec: &Value) -> Result<Value> {
    let exec = if exec.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        exec.clone()
    };
    let raw = serde_json::to_string(&exec).map_err(|e| Error::msg(format!("invalid exec: {e}")))?;
    let sealed = seal_secret(&COL_TOOL_EXEC, &raw).map_err(Error::msg)?;
    serde_json::from_str(&sealed).map_err(|e| Error::msg(format!("sealed exec: {e}")))
}

fn schema_or_empty(schema: &Value) -> Value {
    if schema.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        schema.clone()
    }
}

fn agents_or_empty(agents: &Value) -> Value {
    if agents.is_null() {
        Value::Array(Vec::new())
    } else {
        agents.clone()
    }
}

impl DB {
    /// `SeedTool`: insert a built-in tool's code-defined defaults ONCE (first-insert
    /// only; UI edits survive restarts).
    pub async fn seed_tool(
        &self,
        key: &str,
        desc: &str,
        schema: &Value,
        agents: &Value,
    ) -> Result<()> {
        self.execute(
            "INSERT INTO tools(key, system, description, schema, agents, enabled) \
             VALUES ($1, true, $2, $3, $4, true) ON CONFLICT (key) DO NOTHING",
            &[
                &key,
                &desc,
                &schema_or_empty(schema),
                &agents_or_empty(agents),
            ],
        )
        .await?;
        Ok(())
    }

    /// `AddAgentToToolBinding`: add an agent key to the given tools' agents arrays
    /// (idempotent).
    pub async fn add_agent_to_tool_binding(&self, agent_key: &str, keys: &[String]) -> Result<()> {
        for k in keys {
            self.execute(
                "UPDATE tools SET agents = agents || to_jsonb($1::text) \
                 WHERE key=$2 AND NOT (agents ? $1)",
                &[&agent_key, &k],
            )
            .await?;
        }
        Ok(())
    }

    /// `RemoveAgentFromToolBindings`: strip an agent key from every tool's agents.
    pub async fn remove_agent_from_tool_bindings(&self, agent_key: &str) -> Result<()> {
        self.execute(
            "UPDATE tools SET agents = agents - $1 WHERE agents ? $1",
            &[&agent_key],
        )
        .await?;
        Ok(())
    }

    /// `RemoveAgentFromTool`: strip an agent key from a single tool's agents.
    pub async fn remove_agent_from_tool(&self, agent_key: &str, tool_key: &str) -> Result<()> {
        self.execute(
            "UPDATE tools SET agents = agents - $1 WHERE key=$2 AND agents ? $1",
            &[&agent_key, &tool_key],
        )
        .await?;
        Ok(())
    }

    /// `UpsertToolForce`: overwrite a tool row with the given code-default values
    /// (explicit "reset to code default"), keeping system=true and re-enabling it.
    pub async fn upsert_tool_force(
        &self,
        key: &str,
        desc: &str,
        schema: &Value,
        agents: &Value,
    ) -> Result<()> {
        self.execute(
            "INSERT INTO tools(key, system, description, schema, agents, enabled) \
             VALUES ($1, true, $2, $3, $4, true) \
             ON CONFLICT (key) DO UPDATE SET description = EXCLUDED.description, \
                 schema = EXCLUDED.schema, agents = EXCLUDED.agents, enabled = true",
            &[
                &key,
                &desc,
                &schema_or_empty(schema),
                &agents_or_empty(agents),
            ],
        )
        .await?;
        Ok(())
    }

    /// `RefreshToolDefaults`: update a system tool's model-facing description +
    /// schema to code defaults, preserving agent binding + enabled flag.
    pub async fn refresh_tool_defaults(&self, key: &str, desc: &str, schema: &Value) -> Result<()> {
        self.execute(
            "UPDATE tools SET description=$2, schema=$3, updated_at=now() \
             WHERE key=$1 AND system",
            &[&key, &desc, &schema_or_empty(schema)],
        )
        .await?;
        Ok(())
    }

    /// `ListTools`: the whole catalog, ordered by key (custom execs opened).
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        let rows = self
            .query(&format!("SELECT {TOOL_COLS} FROM tools ORDER BY key"), &[])
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_tool(r)?);
        }
        Ok(out)
    }

    /// `GetTool`: one tool (None if absent).
    pub async fn get_tool(&self, key: &str) -> Result<Option<Tool>> {
        let row = self
            .query_opt(
                &format!("SELECT {TOOL_COLS} FROM tools WHERE key=$1"),
                &[&key],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(scan_tool(&r)?)),
            None => Ok(None),
        }
    }

    /// `CreateCustomTool`: insert a user-defined tool (system=false) with a sealed
    /// exec spec. Fails if the key already exists.
    pub async fn create_custom_tool(&self, t: &Tool) -> Result<()> {
        let sealed = seal_tool_exec(&t.exec)?;
        let agents = serde_json::to_value(&t.agents).map_err(|e| Error::msg(e.to_string()))?;
        self.execute(
            "INSERT INTO tools(key, system, description, schema, agents, enabled, kind, exec, deferred) \
             VALUES ($1, false, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &t.key,
                &t.description,
                &schema_or_empty(&t.schema),
                &agents,
                &t.enabled,
                &t.kind,
                &sealed,
                &t.deferred,
            ],
        )
        .await?;
        Ok(())
    }

    /// `UpdateCustomTool`: update a custom tool's editable fields (system=false only).
    pub async fn update_custom_tool(&self, t: &Tool) -> Result<()> {
        let sealed = seal_tool_exec(&t.exec)?;
        let agents = serde_json::to_value(&t.agents).map_err(|e| Error::msg(e.to_string()))?;
        self.execute(
            "UPDATE tools SET description=$2, schema=$3, agents=$4, enabled=$5, kind=$6, exec=$7, deferred=$8 \
             WHERE key=$1 AND system=false",
            &[
                &t.key,
                &t.description,
                &schema_or_empty(&t.schema),
                &agents,
                &t.enabled,
                &t.kind,
                &sealed,
                &t.deferred,
            ],
        )
        .await?;
        Ok(())
    }

    /// `DeleteCustomTool`: remove a custom tool (built-ins protected).
    pub async fn delete_custom_tool(&self, key: &str) -> Result<()> {
        self.execute("DELETE FROM tools WHERE key=$1 AND system=false", &[&key])
            .await?;
        Ok(())
    }

    /// `ListCustomTools`: only user-defined (system=false) tools.
    pub async fn list_custom_tools(&self) -> Result<Vec<Tool>> {
        let rows = self
            .query(
                &format!("SELECT {TOOL_COLS} FROM tools WHERE system=false ORDER BY key"),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_tool(r)?);
        }
        Ok(out)
    }

    /// `UpdateTool`: save the page-editable fields (key is never changed).
    pub async fn update_tool(
        &self,
        key: &str,
        desc: &str,
        schema: &Value,
        agents: &Value,
        enabled: bool,
    ) -> Result<()> {
        self.execute(
            "UPDATE tools SET description=$2, schema=$3, agents=$4, enabled=$5 WHERE key=$1",
            &[
                &key,
                &desc,
                &schema_or_empty(schema),
                &agents_or_empty(agents),
                &enabled,
            ],
        )
        .await?;
        Ok(())
    }
}
