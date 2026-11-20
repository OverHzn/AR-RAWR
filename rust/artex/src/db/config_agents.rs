//! Agent CRUD + setters — slice of `db/config.go` (agents).
//!
//! Built-in agents are protected: meta updates and deletes only touch
//! `builtin=false` rows. LLM-profile binds follow the shared child/profile lock
//! order to avoid deadlocks with `DeleteProfile`.

use crate::db::config_profiles::ERR_LLM_PROFILE_NOT_FOUND;
use crate::db::error::{Error, Result};
use crate::db::DB;

/// `Agent`: one recorded agent configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Agent {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub description: String,
    pub role: String,
    pub builtin: bool,
    pub enabled: bool,
    pub llm_profile_id: Option<i64>,
    pub max_turns: i32,
    pub run_secs: i32,
    pub web_search: bool,
    pub interactive_shell: bool,
    pub wrapup_prompt: String,
    pub wrapup_max_turns: i32,
    pub task_timeout_wrapup_prompt: String,
    pub task_timeout_wrapup_max_turns: i32,
    pub trigger_run_mode: String,
    pub trigger_merge_mode: String,
    pub trigger_max_parallel: i32,
}

const AGENT_COLS: &str = "id,key,name,COALESCE(description,''),role,builtin,enabled,COALESCE(max_turns,0),COALESCE(run_seconds,600),COALESCE(web_search,false),COALESCE(interactive_shell,false),COALESCE(wrapup_prompt,''),COALESCE(wrapup_max_turns,0),COALESCE(task_timeout_wrapup_prompt,''),COALESCE(task_timeout_wrapup_max_turns,0),COALESCE(trigger_run_mode,'serial'),COALESCE(trigger_merge_mode,'all'),COALESCE(trigger_max_parallel,5),llm_profile_id";

fn scan_agent(row: &tokio_postgres::Row) -> Agent {
    Agent {
        id: row.get(0),
        key: row.get(1),
        name: row.get(2),
        description: row.get(3),
        role: row.get(4),
        builtin: row.get(5),
        enabled: row.get(6),
        max_turns: row.get(7),
        run_secs: row.get(8),
        web_search: row.get(9),
        interactive_shell: row.get(10),
        wrapup_prompt: row.get(11),
        wrapup_max_turns: row.get(12),
        task_timeout_wrapup_prompt: row.get(13),
        task_timeout_wrapup_max_turns: row.get(14),
        trigger_run_mode: row.get(15),
        trigger_merge_mode: row.get(16),
        trigger_max_parallel: row.get(17),
        llm_profile_id: row.get(18),
    }
}

/// `lockLLMProfileForReference`: verify a referenced profile under FOR KEY SHARE.
async fn lock_llm_profile_for_reference(
    tx: &deadpool_postgres::Transaction<'_>,
    profile_id: Option<i64>,
) -> Result<()> {
    let Some(pid) = profile_id else { return Ok(()) };
    let row = tx
        .query_opt(
            "SELECT id FROM llm_profiles WHERE id=$1 FOR KEY SHARE",
            &[&pid],
        )
        .await?;
    if row.is_none() {
        return Err(Error::sentinel(
            ERR_LLM_PROFILE_NOT_FOUND,
            ERR_LLM_PROFILE_NOT_FOUND,
        ));
    }
    Ok(())
}

impl DB {
    pub async fn list_agents(&self) -> Result<Vec<Agent>> {
        let rows = self
            .query(&format!("SELECT {AGENT_COLS} FROM agents ORDER BY id"), &[])
            .await?;
        Ok(rows.iter().map(scan_agent).collect())
    }

    /// `GetAgentByKey`: one agent (None when absent).
    pub async fn get_agent_by_key(&self, key: &str) -> Result<Option<Agent>> {
        let row = self
            .query_opt(
                &format!("SELECT {AGENT_COLS} FROM agents WHERE key=$1"),
                &[&key],
            )
            .await?;
        Ok(row.as_ref().map(scan_agent))
    }

    /// `AgentBindingCounts`: per-agent visible MCP/skill counts by agent id and
    /// bound tool counts by agent key — a few grouped queries, no N+1.
    pub async fn agent_binding_counts(
        &self,
    ) -> Result<(
        std::collections::HashMap<i64, i64>,
        std::collections::HashMap<i64, i64>,
        std::collections::HashMap<String, i64>,
    )> {
        let mut mcp = std::collections::HashMap::new();
        let mut skill = std::collections::HashMap::new();
        let mut tools = std::collections::HashMap::new();
        for (q, into) in [
            ("SELECT agent_id, count(DISTINCT resource_id) FROM agent_visibility \
             WHERE resource_kind='mcp' AND enabled GROUP BY agent_id", &mut mcp),
            ("SELECT agent_id, count(*) FROM agent_skill_visibility WHERE enabled GROUP BY agent_id", &mut skill),
        ] {
            let rows = self.query(q, &[]).await?;
            for r in &rows {
                into.insert(r.get(0), r.get(1));
            }
        }
        let rows = self
            .query(
                "SELECT elem, count(*) FROM tools, jsonb_array_elements_text(agents) AS elem GROUP BY elem",
                &[],
            )
            .await?;
        for r in &rows {
            tools.insert(r.get(0), r.get(1));
        }
        Ok((mcp, skill, tools))
    }

    /// `CreateAgent`: insert a custom (builtin=false) agent with role 'assistant'.
    pub async fn create_agent(&self, key: &str, name: &str, description: &str) -> Result<Agent> {
        let desc: Option<&str> = if description.is_empty() {
            None
        } else {
            Some(description)
        };
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&key, &name, &desc];
        let row = self
            .query_one(
                &format!(
                    "INSERT INTO agents(key, name, description, role, builtin, enabled) \
                     VALUES ($1, $2, NULLIF($3,''), 'assistant', false, true) RETURNING {AGENT_COLS}"
                ),
                &params,
            )
            .await?;
        Ok(scan_agent(&row))
    }

    /// `UpdateAgentMeta`: update a custom agent's name + description (builtins untouched).
    pub async fn update_agent_meta(&self, key: &str, name: &str, description: &str) -> Result<()> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&key, &name, &description];
        self.execute(
            "UPDATE agents SET name=$2, description=NULLIF($3,'') WHERE key=$1 AND builtin=false",
            &params,
        )
        .await?;
        Ok(())
    }

    /// `DeleteAgent`: remove a custom agent (builtins protected).
    pub async fn delete_agent(&self, key: &str) -> Result<()> {
        self.execute("DELETE FROM agents WHERE key=$1 AND builtin=false", &[&key])
            .await?;
        Ok(())
    }

    pub async fn set_agent_max_turns(&self, key: &str, mut max_turns: i32) -> Result<()> {
        if max_turns < 0 {
            max_turns = 0;
        }
        self.execute(
            "UPDATE agents SET max_turns=$1 WHERE key=$2",
            &[&max_turns, &key],
        )
        .await?;
        Ok(())
    }

    pub async fn set_agent_web_search(&self, key: &str, on: bool) -> Result<()> {
        self.execute("UPDATE agents SET web_search=$1 WHERE key=$2", &[&on, &key])
            .await?;
        Ok(())
    }

    pub async fn set_agent_interactive_shell(&self, key: &str, on: bool) -> Result<()> {
        self.execute(
            "UPDATE agents SET interactive_shell=$1 WHERE key=$2",
            &[&on, &key],
        )
        .await?;
        Ok(())
    }

    pub async fn set_agent_wrapup_prompt(&self, key: &str, prompt: &str) -> Result<()> {
        self.execute(
            "UPDATE agents SET wrapup_prompt=$1 WHERE key=$2",
            &[&prompt, &key],
        )
        .await?;
        Ok(())
    }

    pub async fn set_agent_wrapup_max_turns(&self, key: &str, n: i32) -> Result<()> {
        self.execute(
            "UPDATE agents SET wrapup_max_turns=$1 WHERE key=$2",
            &[&n, &key],
        )
        .await?;
        Ok(())
    }

    pub async fn set_agent_task_timeout_wrapup(
        &self,
        key: &str,
        prompt: &str,
        max_turns: i32,
    ) -> Result<()> {
        self.execute(
            "UPDATE agents SET task_timeout_wrapup_prompt=$1, task_timeout_wrapup_max_turns=$2 WHERE key=$3",
            &[&prompt, &max_turns, &key],
        )
        .await?;
        Ok(())
    }

    pub async fn set_agent_run_seconds(&self, key: &str, mut run_secs: i32) -> Result<()> {
        if run_secs < 0 {
            run_secs = 0;
        }
        self.execute(
            "UPDATE agents SET run_seconds=$1 WHERE key=$2",
            &[&run_secs, &key],
        )
        .await?;
        Ok(())
    }

    /// `SetAgentTriggerBehavior`: validate enums with fallback defaults.
    pub async fn set_agent_trigger_behavior(
        &self,
        key: &str,
        run_mode: &str,
        merge_mode: &str,
        mut max_parallel: i32,
    ) -> Result<()> {
        let run_mode = match run_mode {
            "serial" | "parallel" => run_mode,
            _ => "serial",
        };
        let merge_mode = match merge_mode {
            "by_task" | "all" | "none" => merge_mode,
            _ => "by_task",
        };
        if max_parallel < 0 {
            max_parallel = 0;
        }
        self.execute(
            "UPDATE agents SET trigger_run_mode=$1, trigger_merge_mode=$2, trigger_max_parallel=$3 WHERE key=$4",
            &[&run_mode, &merge_mode, &max_parallel, &key],
        )
        .await?;
        Ok(())
    }

    /// `SetAgentLLMProfile`: bind (or clear) an agent's LLM profile. Uses the
    /// child-row -> profile-row lock order to match DeleteProfile.
    pub async fn set_agent_llm_profile(&self, key: &str, id: Option<i64>) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let row = tx
                .query_opt("SELECT id FROM agents WHERE key=$1 FOR UPDATE", &[&key])
                .await?;
            let Some(row) = row else { return Ok(()) }; // unknown key: no-op
            let agent_id: i64 = row.get(0);
            lock_llm_profile_for_reference(&tx, id).await?;
            tx.execute(
                "UPDATE agents SET llm_profile_id=$1 WHERE id=$2",
                &[&id, &agent_id],
            )
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
