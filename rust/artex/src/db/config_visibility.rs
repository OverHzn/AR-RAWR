//! Skill + resource visibility — slice of `db/config.go`.
//!
//! Skill rows are DISABLED rather than deleted on "off", so an insert-if-absent
//! seed cannot resurrect an explicitly hidden builtin on the next start.

use crate::db::error::Result;
use crate::db::DB;

impl DB {
    /// `AgentSkillNames`: enabled skills for one agent, by name.
    pub async fn agent_skill_names(&self, agent_id: i64) -> Result<Vec<String>> {
        let rows = self
            .query(
                "SELECT skill_name FROM agent_skill_visibility WHERE agent_id=$1 AND enabled ORDER BY skill_name",
                &[&agent_id],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// `SkillAgents`: agent ids that can see one skill.
    pub async fn skill_agents(&self, skill_name: &str) -> Result<Vec<i64>> {
        let rows = self
            .query(
                "SELECT agent_id FROM agent_skill_visibility WHERE skill_name=$1 AND enabled",
                &[&skill_name],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// `SetAgentSkillVisibility`: replace an agent's visible skills, disabling old
    /// rows rather than deleting them.
    pub async fn set_agent_skill_visibility(&self, agent_id: i64, names: &[String]) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "UPDATE agent_skill_visibility SET enabled=false WHERE agent_id=$1",
                &[&agent_id],
            )
            .await?;
            for name in names {
                tx.execute(
                    "INSERT INTO agent_skill_visibility(agent_id,skill_name,enabled) VALUES ($1,$2,true) \
                     ON CONFLICT (agent_id,skill_name) DO UPDATE SET enabled=true",
                    &[&agent_id, &name],
                )
                .await?;
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

    /// `ToggleSkillVisibility`: set one (agent, skill) on/off. Off writes an
    /// `enabled=false` row even when none existed (never "unseeded").
    pub async fn toggle_skill_visibility(
        &self,
        agent_id: i64,
        skill_name: &str,
        on: bool,
    ) -> Result<()> {
        self.execute(
            "INSERT INTO agent_skill_visibility(agent_id,skill_name,enabled) VALUES ($1,$2,$3) \
             ON CONFLICT (agent_id,skill_name) DO UPDATE SET enabled=$3",
            &[&agent_id, &skill_name, &on],
        )
        .await?;
        Ok(())
    }

    /// `DeleteSkillVisibility`: remove all rows for a skill (skill deleted on disk).
    pub async fn delete_skill_visibility(&self, skill_name: &str) -> Result<()> {
        self.execute(
            "DELETE FROM agent_skill_visibility WHERE skill_name=$1",
            &[&skill_name],
        )
        .await?;
        Ok(())
    }

    /// `AgentVisible`: resource ids of a kind visible to one agent.
    pub async fn agent_visible(&self, agent_id: i64, kind: &str) -> Result<Vec<i64>> {
        let rows = self
            .query(
                "SELECT resource_id FROM agent_visibility WHERE agent_id=$1 AND resource_kind=$2 AND enabled",
                &[&agent_id, &kind],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// `ResourceAgents`: agent ids that can see one resource.
    pub async fn resource_agents(&self, kind: &str, resource_id: i64) -> Result<Vec<i64>> {
        let rows = self
            .query(
                "SELECT agent_id FROM agent_visibility WHERE resource_kind=$1 AND resource_id=$2 AND enabled",
                &[&kind, &resource_id],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// `SetAgentVisibilityKind`: replace the full visible resource set of a kind.
    pub async fn set_agent_visibility_kind(
        &self,
        agent_id: i64,
        kind: &str,
        resource_ids: &[i64],
    ) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "DELETE FROM agent_visibility WHERE agent_id=$1 AND resource_kind=$2",
                &[&agent_id, &kind],
            )
            .await?;
            for rid in resource_ids {
                tx.execute(
                    "INSERT INTO agent_visibility(agent_id,resource_kind,resource_id,enabled) \
                     VALUES ($1,$2,$3,true) \
                     ON CONFLICT (agent_id,resource_kind,resource_id,mcp_tool_name) DO UPDATE SET enabled=true",
                    &[&agent_id, &kind, rid],
                )
                .await?;
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

    /// `ToggleVisibility`: set one (agent, kind, resource) on/off (idempotent).
    pub async fn toggle_visibility(
        &self,
        agent_id: i64,
        kind: &str,
        resource_id: i64,
        on: bool,
    ) -> Result<()> {
        if on {
            self.execute(
                "INSERT INTO agent_visibility(agent_id,resource_kind,resource_id,enabled) \
                 VALUES ($1,$2,$3,true) \
                 ON CONFLICT (agent_id,resource_kind,resource_id,mcp_tool_name) DO UPDATE SET enabled=true",
                &[&agent_id, &kind, &resource_id],
            )
            .await?;
        } else {
            self.execute(
                "DELETE FROM agent_visibility WHERE agent_id=$1 AND resource_kind=$2 AND resource_id=$3",
                &[&agent_id, &kind, &resource_id],
            )
            .await?;
        }
        Ok(())
    }
}
