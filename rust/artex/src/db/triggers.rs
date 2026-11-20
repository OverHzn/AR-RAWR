//! Agent triggers + scheduler watermarks + cross-exploration event queries —
//! port of `db/triggers.go`.

use std::time::SystemTime;

use crate::db::Result;
use crate::db::DB;

const TRIGGER_COLS: &str = "id, agent_key, enabled, interval_sec, on_finding, on_goal_met, \
    on_task_timeout, on_tool_call, on_task_create, interval_message, finding_message, \
    goal_message, task_timeout_message, tool_call_message, task_create_message, \
    tool_names, last_fire";

/// `AgentTrigger`: one P3 trigger attached to a custom agent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTrigger {
    pub id: i64,
    pub agent_key: String,
    pub enabled: bool,
    pub interval_sec: i32,
    pub on_finding: bool,
    pub on_goal_met: bool,
    pub on_task_timeout: bool,
    pub on_tool_call: bool,
    pub on_task_create: bool,
    pub interval_message: String,
    pub finding_message: String,
    pub goal_message: String,
    pub task_timeout_message: String,
    pub tool_call_message: String,
    pub task_create_message: String,
    pub tool_names: Vec<String>,
    pub last_fire: Option<SystemTime>,
}

/// `TaskEvent`: a finding/goal/task/tool event carrying its owning task's info.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskEvent {
    pub node_id: i64,
    pub task_id: i64,
    pub task_desc: String,
    pub task_goal: String,
    pub summary: String,
    pub vuln_class: String,
    pub severity: String,
    pub tool: String,
    pub tool_input: String,
    pub tool_output: String,
    pub tool_is_err: bool,
}

/// `marshalToolNames`: encode the tool-key list as JSON text for the TEXT column.
/// nil/empty stores "" (not "null"/"[]") so the column default stays clean.
fn marshal_tool_names(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    serde_json::to_string(names).unwrap_or_default()
}

fn scan_trigger(row: &tokio_postgres::Row) -> AgentTrigger {
    let tool_names: String = row.get(15);
    let last_fire: Option<SystemTime> = row.get(16);
    AgentTrigger {
        id: row.get(0),
        agent_key: row.get(1),
        enabled: row.get(2),
        interval_sec: row.get(3),
        on_finding: row.get(4),
        on_goal_met: row.get(5),
        on_task_timeout: row.get(6),
        on_tool_call: row.get(7),
        on_task_create: row.get(8),
        interval_message: row.get(9),
        finding_message: row.get(10),
        goal_message: row.get(11),
        task_timeout_message: row.get(12),
        tool_call_message: row.get(13),
        task_create_message: row.get(14),
        tool_names: if tool_names.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&tool_names).unwrap_or_default()
        },
        last_fire,
    }
}

impl DB {
    /// `CreateTrigger`: insert a trigger for `agent_key` and return it.
    pub async fn create_trigger(&self, t: &AgentTrigger) -> Result<AgentTrigger> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 15] = [
            &t.agent_key,
            &t.enabled,
            &t.interval_sec,
            &t.on_finding,
            &t.on_goal_met,
            &t.on_task_timeout,
            &t.on_tool_call,
            &t.on_task_create,
            &t.interval_message,
            &t.finding_message,
            &t.goal_message,
            &t.task_timeout_message,
            &t.tool_call_message,
            &t.task_create_message,
            &marshal_tool_names(&t.tool_names),
        ];
        let row = self
            .query_one(
                &format!(
                    "INSERT INTO agent_triggers(agent_key, enabled, interval_sec, on_finding, \
                     on_goal_met, on_task_timeout, on_tool_call, on_task_create, interval_message, \
                     finding_message, goal_message, task_timeout_message, tool_call_message, \
                     task_create_message, tool_names) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
                     RETURNING {TRIGGER_COLS}"
                ),
                &params,
            )
            .await?;
        Ok(scan_trigger(&row))
    }

    /// `UpdateTrigger`: update a trigger's fields (not last_fire).
    pub async fn update_trigger(&self, t: &AgentTrigger) -> Result<()> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 15] = [
            &t.id,
            &t.enabled,
            &t.interval_sec,
            &t.on_finding,
            &t.on_goal_met,
            &t.on_task_timeout,
            &t.on_tool_call,
            &t.on_task_create,
            &t.interval_message,
            &t.finding_message,
            &t.goal_message,
            &t.task_timeout_message,
            &t.tool_call_message,
            &t.task_create_message,
            &marshal_tool_names(&t.tool_names),
        ];
        self.execute(
            "UPDATE agent_triggers SET enabled=$2, interval_sec=$3, on_finding=$4, \
             on_goal_met=$5, on_task_timeout=$6, on_tool_call=$7, on_task_create=$8, \
             interval_message=$9, finding_message=$10, goal_message=$11, \
             task_timeout_message=$12, tool_call_message=$13, task_create_message=$14, \
             tool_names=$15 WHERE id=$1",
            &params,
        )
        .await?;
        Ok(())
    }

    /// `DeleteTrigger`: remove one trigger.
    pub async fn delete_trigger(&self, id: i64) -> Result<()> {
        self.execute("DELETE FROM agent_triggers WHERE id=$1", &[&id])
            .await?;
        Ok(())
    }

    /// `ListTriggersFor`: an agent's triggers, ordered by id.
    pub async fn list_triggers_for(&self, agent_key: &str) -> Result<Vec<AgentTrigger>> {
        self.query_triggers(
            &format!("SELECT {TRIGGER_COLS} FROM agent_triggers WHERE agent_key=$1 ORDER BY id"),
            &[&agent_key],
        )
        .await
    }

    /// `ListEnabledTriggers`: all enabled triggers (for the scheduler).
    pub async fn list_enabled_triggers(&self) -> Result<Vec<AgentTrigger>> {
        self.query_triggers(
            &format!("SELECT {TRIGGER_COLS} FROM agent_triggers WHERE enabled ORDER BY id"),
            &[],
        )
        .await
    }

    async fn query_triggers(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Vec<AgentTrigger>> {
        let rows = self.query(sql, params).await?;
        Ok(rows.iter().map(scan_trigger).collect())
    }

    /// `TouchTriggerFire`: record an interval trigger's fire time (now).
    pub async fn touch_trigger_fire(&self, id: i64) -> Result<()> {
        self.execute(
            "UPDATE agent_triggers SET last_fire=now() WHERE id=$1",
            &[&id],
        )
        .await?;
        Ok(())
    }

    /// `DeleteTriggersForAgent`: remove all triggers of an agent.
    pub async fn delete_triggers_for_agent(&self, agent_key: &str) -> Result<()> {
        self.execute(
            "DELETE FROM agent_triggers WHERE agent_key=$1",
            &[&agent_key],
        )
        .await?;
        Ok(())
    }

    /// `GetSchedState`: a scheduler watermark ("" when absent).
    pub async fn get_sched_state(&self, key: &str) -> Result<String> {
        let row = self
            .query_opt("SELECT value FROM scheduler_state WHERE key=$1", &[&key])
            .await?;
        Ok(row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .unwrap_or_default())
    }

    /// `SetSchedState`: upsert a scheduler watermark.
    pub async fn set_sched_state(&self, key: &str, value: &str) -> Result<()> {
        self.execute(
            "INSERT INTO scheduler_state(key,value) VALUES ($1,$2) \
             ON CONFLICT (key) DO UPDATE SET value=EXCLUDED.value",
            &[&key, &value],
        )
        .await?;
        Ok(())
    }

    /// `NewFindingsSince`: finding-node events with node id > `lastID` across live
    /// tasks (monotonic watermark → no double-fire).
    pub async fn new_findings_since(&self, last_id: i64) -> Result<Vec<TaskEvent>> {
        let rows = self
            .query(
                "SELECT n.id, t.id, t.description, t.goal, n.payload \
                 FROM exploration_nodes n JOIN tasks t ON t.exploration_id = n.exploration_id \
                 WHERE n.kind='finding' AND n.id > $1 AND t.deleted_at IS NULL \
                 ORDER BY n.id",
                &[&last_id],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let payload: serde_json::Value = r.get(4);
            out.push(TaskEvent {
                node_id: r.get(0),
                task_id: r.get(1),
                task_desc: r.get(2),
                task_goal: r.get(3),
                summary: payload
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                vuln_class: payload
                    .get("vulnclass")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                severity: payload
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ..TaskEvent::default()
            });
        }
        Ok(out)
    }

    /// `TimedOutTasksSince`: tasks in `status='timeout'` with id > `lastID`.
    pub async fn timed_out_tasks_since(&self, last_id: i64) -> Result<Vec<TaskEvent>> {
        let rows = self
            .query(
                "SELECT id, description, goal FROM tasks \
                 WHERE status='timeout' AND deleted_at IS NULL AND id > $1 ORDER BY id",
                &[&last_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let id: i64 = r.get(0);
                TaskEvent {
                    node_id: id,
                    task_id: id,
                    task_desc: r.get(1),
                    task_goal: r.get(2),
                    summary: r.get(2),
                    ..TaskEvent::default()
                }
            })
            .collect())
    }

    /// `NewTasksSince`: tasks created with id > `lastID` (excluding deleted).
    pub async fn new_tasks_since(&self, last_id: i64) -> Result<Vec<TaskEvent>> {
        let rows = self
            .query(
                "SELECT id, description, goal FROM tasks \
                 WHERE deleted_at IS NULL AND id > $1 ORDER BY id",
                &[&last_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let id: i64 = r.get(0);
                TaskEvent {
                    node_id: id,
                    task_id: id,
                    task_desc: r.get(1),
                    task_goal: r.get(2),
                    summary: r.get(2),
                    ..TaskEvent::default()
                }
            })
            .collect())
    }

    /// `NewToolCallsSince`: completed tool calls (tool_result rows) with activity
    /// id > `lastID`, joining the paired tool_use row for the input.
    pub async fn new_tool_calls_since(&self, last_id: i64) -> Result<Vec<TaskEvent>> {
        let rows = self
            .query(
                "SELECT r.id, t.id, t.description, t.goal, r.tool, COALESCE(u.detail,''), \
                 COALESCE(r.detail,''), r.is_error \
                 FROM activity r \
                 JOIN tasks t ON t.exploration_id = r.exploration_id \
                 LEFT JOIN activity u ON u.exploration_id = r.exploration_id \
                     AND u.tool_use_id = r.tool_use_id AND u.kind='tool_use' \
                 WHERE r.kind='tool_result' AND r.id > $1 AND r.tool <> '' \
                     AND t.deleted_at IS NULL \
                 ORDER BY r.id",
                &[&last_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TaskEvent {
                node_id: r.get(0),
                task_id: r.get(1),
                task_desc: r.get(2),
                task_goal: r.get(3),
                tool: r.get(4),
                tool_input: r.get(5),
                tool_output: r.get(6),
                tool_is_err: r.get(7),
                ..TaskEvent::default()
            })
            .collect())
    }

    /// `MetGoals`: all met goals across live tasks.
    pub async fn met_goals(&self) -> Result<Vec<TaskEvent>> {
        let rows = self
            .query(
                "SELECT n.id, t.id, t.description, t.goal, n.payload \
                 FROM exploration_nodes n JOIN tasks t ON t.exploration_id = n.exploration_id \
                 WHERE n.kind='goal' AND n.state='met' AND t.deleted_at IS NULL \
                 ORDER BY n.id",
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let payload: serde_json::Value = r.get(4);
            let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let summary = payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push(TaskEvent {
                node_id: r.get(0),
                task_id: r.get(1),
                task_desc: r.get(2),
                task_goal: r.get(3),
                summary: if !text.is_empty() {
                    text.to_string()
                } else {
                    summary.to_string()
                },
                ..TaskEvent::default()
            });
        }
        Ok(out)
    }
}
