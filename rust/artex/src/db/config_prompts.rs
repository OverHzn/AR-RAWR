//! Prompt versions — slice of `db/config.go` (agent prompts).
//!
//! `SavePrompt` appends a new version and points `current_prompt_id` at it in one
//! tx; `SeedPromptIfEmpty` is first-insert-only so a user's edits survive restarts.

use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::DB;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptVar {
    pub name: String,
    pub description: String,
    pub example: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptVersion {
    pub version: i32,
    pub template: String,
    pub note: String,
    pub created_at: SystemTime,
}

impl DB {
    /// `PromptVars`: one agent's prompt variables by name.
    pub async fn prompt_vars(&self, agent_id: i64) -> Result<Vec<PromptVar>> {
        let rows = self
            .query(
                "SELECT var_name,COALESCE(description,''),COALESCE(example,''),source \
                 FROM agent_prompt_vars WHERE agent_id=$1 ORDER BY var_name",
                &[&agent_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| PromptVar {
                name: r.get(0),
                description: r.get(1),
                example: r.get(2),
                source: r.get(3),
            })
            .collect())
    }

    /// `CurrentPrompt`: active template text (empty if none set).
    pub async fn current_prompt(&self, agent_id: i64) -> Result<String> {
        let row = self
            .query_opt(
                "SELECT p.template_text FROM agents a JOIN agent_prompts p \
                 ON p.id=a.current_prompt_id WHERE a.id=$1",
                &[&agent_id],
            )
            .await?;
        Ok(row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .unwrap_or_default())
    }

    /// `SeedPromptIfEmpty`: write the code default as first version only when the
    /// agent has no current prompt. Idempotent.
    pub async fn seed_prompt_if_empty(&self, agent_id: i64, tmpl: &str) -> Result<()> {
        let row = self
            .query_one(
                "SELECT current_prompt_id FROM agents WHERE id=$1",
                &[&agent_id],
            )
            .await?;
        if row.get::<_, Option<i64>>(0).is_some() {
            return Ok(());
        }
        self.save_prompt(agent_id, tmpl, "built-in default", "system")
            .await?;
        Ok(())
    }

    /// `ResetPromptToDefault`: append the code default as a new version.
    pub async fn reset_prompt_to_default(&self, agent_id: i64, tmpl: &str) -> Result<i32> {
        self.save_prompt(agent_id, tmpl, "restore to built-in default", "system")
            .await
    }

    /// `SavePrompt`: append a new version and point `current_prompt_id` at it.
    pub async fn save_prompt(
        &self,
        agent_id: i64,
        template: &str,
        note: &str,
        by: &str,
    ) -> Result<i32> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let row = tx
                .query_one(
                    "SELECT COALESCE(max(version),0)+1 FROM agent_prompts WHERE agent_id=$1",
                    &[&agent_id],
                )
                .await?;
            let ver: i32 = row.get(0);
            let note_opt: Option<&str> = if note.is_empty() { None } else { Some(note) };
            let by_opt: Option<&str> = if by.is_empty() { None } else { Some(by) };
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 5] =
                [&agent_id, &ver, &template, &note_opt, &by_opt];
            let row = tx
                .query_one(
                    "INSERT INTO agent_prompts(agent_id,version,template_text,note,updated_by) \
                     VALUES ($1,$2,$3,NULLIF($4,''),NULLIF($5,'')) RETURNING id",
                    &params,
                )
                .await?;
            let pid: i64 = row.get(0);
            tx.execute(
                "UPDATE agents SET current_prompt_id=$1 WHERE id=$2",
                &[&pid, &agent_id],
            )
            .await?;
            Ok::<_, crate::db::Error>(ver)
        }
        .await;
        match run {
            Ok(ver) => {
                tx.commit().await?;
                Ok(ver)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `ListPromptVersions`: all versions, newest first.
    pub async fn list_prompt_versions(&self, agent_id: i64) -> Result<Vec<PromptVersion>> {
        let rows = self
            .query(
                "SELECT version,template_text,COALESCE(note,''),created_at \
                 FROM agent_prompts WHERE agent_id=$1 ORDER BY version DESC",
                &[&agent_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| PromptVersion {
                version: r.get(0),
                template: r.get(1),
                note: r.get(2),
                created_at: r.get(3),
            })
            .collect())
    }
}
