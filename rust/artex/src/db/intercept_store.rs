//! Intercept rule/pending store — slice of `db/intercept.go`.
//!
//! `decision_source` is derived from rule id + the model reason prefix, so the
//! display source stays consistent with rows written before the column existed.

use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::DB;

pub const MODEL_REASON_PREFIX: &str = "[model]";
const LEGACY_MODEL_REASON_PREFIX: &str = "[模型]";

fn is_model_reason(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    lower.starts_with(MODEL_REASON_PREFIX) || reason.starts_with(LEGACY_MODEL_REASON_PREFIX)
}

/// `InterceptSource`: classify a pending row as rule | model | unknown.
pub fn intercept_source(rule_id: i64, reason: &str) -> &'static str {
    if rule_id != 0 {
        "rule"
    } else if is_model_reason(reason) {
        "model"
    } else {
        "unknown"
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterceptRule {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub match_target: String,
    pub match_type: String,
    pub pattern: String,
    pub action: String,
    pub message: String,
    pub timeout_enabled: bool,
    pub timeout_seconds: i32,
    pub timeout_action: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterceptPending {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub task_id: Option<String>,
    pub agent_name: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub status: String,
    pub decision_source: String,
    pub decided_by: String,
    pub reason: String,
    pub decided_at: Option<SystemTime>,
    pub created_at: SystemTime,
}

const RULE_COLS: &str = "id, name, enabled, priority, match_target, match_type, pattern, action, message, timeout_enabled, timeout_seconds, timeout_action, created_at, updated_at";
const PENDING_COLS: &str = "id, rule_id, conversation_id, task_id, agent_name, tool_name, tool_input, status, reason, decided_at, created_at, decision_source, decided_by";

fn scan_rule(row: &tokio_postgres::Row) -> InterceptRule {
    InterceptRule {
        id: row.get(0),
        name: row.get(1),
        enabled: row.get(2),
        priority: row.get(3),
        match_target: row.get(4),
        match_type: row.get(5),
        pattern: row.get(6),
        action: row.get(7),
        message: row.get(8),
        timeout_enabled: row.get(9),
        timeout_seconds: row.get(10),
        timeout_action: row.get(11),
        created_at: row.get(12),
        updated_at: row.get(13),
    }
}

fn scan_pending(row: &tokio_postgres::Row) -> InterceptPending {
    InterceptPending {
        id: row.get(0),
        rule_id: row.get(1),
        conversation_id: row.get(2),
        task_id: row.get(3),
        agent_name: row.get(4),
        tool_name: row.get(5),
        tool_input: row.get(6),
        status: row.get(7),
        reason: row.get(8),
        decided_at: row.get(9),
        created_at: row.get(10),
        decision_source: row.get(11),
        decided_by: row.get(12),
    }
}

impl DB {
    /// `ListInterceptRules`: all rules by priority desc then id.
    pub async fn list_intercept_rules(&self) -> Result<Vec<InterceptRule>> {
        let rows = self
            .query(
                &format!("SELECT {RULE_COLS} FROM intercept_rules ORDER BY priority DESC, id"),
                &[],
            )
            .await?;
        Ok(rows.iter().map(scan_rule).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_intercept_rule(
        &self,
        name: &str,
        match_target: &str,
        match_type: &str,
        pattern: &str,
        action: &str,
        message: &str,
        priority: i32,
        enabled: bool,
        timeout_enabled: bool,
        timeout_seconds: i32,
        timeout_action: &str,
    ) -> Result<InterceptRule> {
        let p: [&(dyn tokio_postgres::types::ToSql + Sync); 11] = [
            &name,
            &enabled,
            &priority,
            &match_target,
            &match_type,
            &pattern,
            &action,
            &message,
            &timeout_enabled,
            &timeout_seconds,
            &timeout_action,
        ];
        let row = self.query_one(
            &format!("INSERT INTO intercept_rules(name, enabled, priority, match_target, match_type, pattern, action, message, timeout_enabled, timeout_seconds, timeout_action) \
                      VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING {RULE_COLS}"),
            &p,
        ).await?;
        Ok(scan_rule(&row))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_intercept_rule(
        &self,
        id: i64,
        name: &str,
        match_target: &str,
        match_type: &str,
        pattern: &str,
        action: &str,
        message: &str,
        priority: i32,
        enabled: bool,
        timeout_enabled: bool,
        timeout_seconds: i32,
        timeout_action: &str,
    ) -> Result<InterceptRule> {
        let p: [&(dyn tokio_postgres::types::ToSql + Sync); 12] = [
            &id,
            &name,
            &enabled,
            &priority,
            &match_target,
            &match_type,
            &pattern,
            &action,
            &message,
            &timeout_enabled,
            &timeout_seconds,
            &timeout_action,
        ];
        let row = self.query_one(
            &format!("UPDATE intercept_rules SET name=$2, enabled=$3, priority=$4, match_target=$5, match_type=$6, pattern=$7, action=$8, message=$9, timeout_enabled=$10, timeout_seconds=$11, timeout_action=$12 WHERE id=$1 RETURNING {RULE_COLS}"),
            &p,
        ).await?;
        Ok(scan_rule(&row))
    }

    pub async fn delete_intercept_rule(&self, id: i64) -> Result<()> {
        self.execute("DELETE FROM intercept_rules WHERE id=$1", &[&id])
            .await?;
        Ok(())
    }

    pub async fn toggle_intercept_rule(&self, id: i64, enabled: bool) -> Result<()> {
        self.execute(
            "UPDATE intercept_rules SET enabled=$2 WHERE id=$1",
            &[&id, &enabled],
        )
        .await?;
        Ok(())
    }

    /// `CreateInterceptPending`: insert a pending approval. rule_id==0 / conv_id==0 /
    /// task_id=="" become NULL. decision_source is derived.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_intercept_pending(
        &self,
        rule_id: i64,
        conv_id: i64,
        task_id: &str,
        agent_name: &str,
        tool_name: &str,
        input: &[u8],
        reason: &str,
        audit: Option<serde_json::Value>,
    ) -> Result<i64> {
        let raw: serde_json::Value = if input.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(input).unwrap_or(serde_json::json!({}))
        };
        let rule_opt = if rule_id != 0 { Some(rule_id) } else { None };
        let conv_opt = if conv_id != 0 { Some(conv_id) } else { None };
        let task_opt = if task_id.is_empty() {
            None
        } else {
            Some(task_id)
        };
        let source = intercept_source(rule_id, reason);
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 9] = [
            &rule_opt,
            &conv_opt,
            &task_opt,
            &agent_name,
            &tool_name,
            &raw,
            &reason,
            &source,
            &audit,
        ];
        let row = self.query_one(
            "INSERT INTO intercept_pending(rule_id, conversation_id, task_id, agent_name, tool_name, tool_input, reason, decision_source, audit) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
            &params,
        ).await?;
        Ok(row.get(0))
    }

    /// `DecideInterceptPending`: settle a pending row (allowed/denied/timeout).
    pub async fn decide_intercept_pending(&self, id: i64, status: &str) -> Result<()> {
        self.execute(
            "UPDATE intercept_pending SET status=$2, decided_at=NOW() WHERE id=$1",
            &[&id, &status],
        )
        .await?;
        Ok(())
    }

    /// `CreateDecidedIntercept`: insert a row already in a final state, decided by
    /// rule (when rule_id != 0) or model.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_decided_intercept(
        &self,
        rule_id: i64,
        conv_id: i64,
        task_id: &str,
        agent_name: &str,
        tool_name: &str,
        input: &[u8],
        status: &str,
        reason: &str,
        audit: Option<serde_json::Value>,
    ) -> Result<i64> {
        let raw: serde_json::Value = if input.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_slice(input).unwrap_or(serde_json::json!({}))
        };
        let rule_opt = if rule_id != 0 { Some(rule_id) } else { None };
        let conv_opt = if conv_id != 0 { Some(conv_id) } else { None };
        let task_opt = if task_id.is_empty() {
            None
        } else {
            Some(task_id)
        };
        let source = intercept_source(rule_id, reason);
        let decided_by = if rule_id != 0 { "rule" } else { "model" };
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 11] = [
            &rule_opt,
            &conv_opt,
            &task_opt,
            &agent_name,
            &tool_name,
            &raw,
            &status,
            &reason,
            &source,
            &audit,
            &decided_by,
        ];
        let row = self.query_one(
            "INSERT INTO intercept_pending(rule_id, conversation_id, task_id, agent_name, tool_name, tool_input, status, reason, decided_at, decision_source, audit, decided_by) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9,$10,$11) RETURNING id",
            &params,
        ).await?;
        Ok(row.get(0))
    }

    pub async fn list_pending_intercepts(&self) -> Result<Vec<InterceptPending>> {
        let rows = self.query(
            &format!("SELECT {PENDING_COLS} FROM intercept_pending WHERE status='pending' ORDER BY created_at DESC"),
            &[],
        ).await?;
        Ok(rows.iter().map(scan_pending).collect())
    }

    pub async fn get_intercept_pending(&self, id: i64) -> Result<Option<InterceptPending>> {
        let row = self
            .query_opt(
                &format!("SELECT {PENDING_COLS} FROM intercept_pending WHERE id=$1"),
                &[&id],
            )
            .await?;
        Ok(row.as_ref().map(scan_pending))
    }
}
