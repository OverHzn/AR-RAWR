//! Target pin store — slice of `db/target_pins.go`.
//!
//! A pin is a host+port a human approved in one scope (task or conversation),
//! with the addresses shown at approval time. Port 0 covers every port of the host.

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::DB;

pub const TARGET_PIN_ACTIVE: &str = "active";
pub const TARGET_PIN_SUPERSEDED: &str = "superseded";
pub const TARGET_PIN_REVOKED: &str = "revoked";
pub const TARGET_PIN_REVOKED_BY_OPERATOR: &str = "operator";
pub const TARGET_PIN_REVOKED_TASK_DELETED: &str = "task_deleted";
pub const TARGET_PIN_REVOKED_CONVERSATION_DELETE: &str = "conversation_deleted";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TargetScopeRef {
    pub task_id: i64,
    pub conversation_id: i64,
}

impl TargetScopeRef {
    pub fn check(&self) -> Result<()> {
        if (self.task_id > 0) == (self.conversation_id > 0) {
            return Err(Error::msg(format!(
                "target scope must name one task or one conversation, got task {} and conversation {}",
                self.task_id, self.conversation_id
            )));
        }
        Ok(())
    }

    /// Scope as `intercept_pending` stores it (task_id TEXT, other column NULL).
    pub fn pending_scope(&self) -> (Option<String>, Option<i64>) {
        if self.task_id > 0 {
            (Some(self.task_id.to_string()), None)
        } else {
            (None, Some(self.conversation_id))
        }
    }
}

/// `TargetKey`: host + ":" + port (last colon wins for IPv6/CIDR in callers).
pub fn target_key(host: &str, port: i32) -> String {
    format!("{host}:{port}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetPin {
    pub id: i64,
    pub task_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub host: String,
    pub port: i32,
    pub version: i32,
    pub pinned_ips: Vec<String>,
    pub resolver: String,
    pub resolved_at: SystemTime,
    pub status: String,
    pub intercept_id: Option<i64>,
    pub decided_by: String,
    pub decided_from: String,
    pub name_only_effective: bool,
    pub revoked_at: Option<SystemTime>,
    pub revoked_reason: String,
    pub created_at: SystemTime,
}

const PIN_COLS: &str = "id, task_id, conversation_id, host, port, version, array_to_json(pinned_ips)::text, resolver, resolved_at, status, intercept_id, decided_by, decided_from, name_only_effective, revoked_at, revoked_reason, created_at";
const PIN_SCOPE: &str = "COALESCE(task_id,0)=$1 AND COALESCE(conversation_id,0)=$2";

fn scan_pin(row: &tokio_postgres::Row) -> Result<TargetPin> {
    let ips: String = row.get(6);
    Ok(TargetPin {
        id: row.get(0),
        task_id: row.get(1),
        conversation_id: row.get(2),
        host: row.get(3),
        port: row.get(4),
        version: row.get(5),
        pinned_ips: serde_json::from_str(&ips).unwrap_or_default(),
        resolver: row.get(7),
        resolved_at: row.get(8),
        status: row.get(9),
        intercept_id: row.get(10),
        decided_by: row.get(11),
        decided_from: row.get(12),
        name_only_effective: row.get(13),
        revoked_at: row.get(14),
        revoked_reason: row.get(15),
        created_at: row.get(16),
    })
}

async fn query_pins(
    db: &DB,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> Result<Vec<TargetPin>> {
    let rows = db.query(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(scan_pin(r)?);
    }
    Ok(out)
}

impl DB {
    /// `ActiveTargetPins`: pins covering (host, port) — exact port first then port 0.
    pub async fn active_target_pins(
        &self,
        scope: &TargetScopeRef,
        host: &str,
        port: i32,
    ) -> Result<Vec<TargetPin>> {
        scope.check()?;
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] =
            [&scope.task_id, &scope.conversation_id, &host, &port];
        query_pins(self, &format!(
            "SELECT {PIN_COLS} FROM target_pins WHERE {PIN_SCOPE} AND host=$3 AND port IN ($4, 0) AND status='active' ORDER BY port DESC"
        ), &params).await
    }

    /// `ListTargetPins`: every pin of a scope in any state, newest first.
    pub async fn list_target_pins(&self, scope: &TargetScopeRef) -> Result<Vec<TargetPin>> {
        scope.check()?;
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] =
            [&scope.task_id, &scope.conversation_id];
        query_pins(
            self,
            &format!("SELECT {PIN_COLS} FROM target_pins WHERE {PIN_SCOPE} ORDER BY id DESC"),
            &params,
        )
        .await
    }

    /// `RevokeTargetPin`: revoke one active pin on behalf of an operator; returns
    /// the pin afterwards (None when the scope has no such pin).
    pub async fn revoke_target_pin(
        &self,
        scope: &TargetScopeRef,
        pin_id: i64,
    ) -> Result<Option<TargetPin>> {
        scope.check()?;
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] = [
            &scope.task_id,
            &scope.conversation_id,
            &pin_id,
            &TARGET_PIN_REVOKED_BY_OPERATOR,
        ];
        self.execute(
            &format!("UPDATE target_pins SET status='revoked', revoked_at=now(), revoked_reason=$4 WHERE {PIN_SCOPE} AND id=$3 AND status='active'"),
            &params,
        ).await?;
        let row = self
            .query_opt(
                &format!("SELECT {PIN_COLS} FROM target_pins WHERE {PIN_SCOPE} AND id=$3"),
                &[&scope.task_id, &scope.conversation_id, &pin_id],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(scan_pin(&r)?)),
            None => Ok(None),
        }
    }
}
