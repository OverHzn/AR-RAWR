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

// ---------------------------------------------------------------------------
// Target approval requests / decisions (db/target_pins.go后半)
// ---------------------------------------------------------------------------

use serde_json::Value as Json;

pub const ERR_TARGET_PIN_EMPTY: &str =
    "the target request lists no resolved address; an approval never pins an empty address list";

/// `TargetRequest`: the pending approval row a gated call opens for one target.
#[derive(Debug, Clone, Default)]
pub struct TargetRequest {
    pub scope: TargetScopeRef,
    pub host: String,
    pub port: i32,
    /// Owning rule of a merged ask; 0 otherwise.
    pub rule_id: i64,
    pub agent_name: String,
    pub tool_name: String,
    pub tool_input: Vec<u8>,
    pub reason: String,
    /// Serialized `InterceptAudit` (targets/merged_ask/correlation are read on approval).
    pub audit: Option<Json>,
}

/// `TargetDecision`: an outcome of the gate no human took (block / exemption /
/// pin version). It opens no request and never pins.
#[derive(Debug, Clone, Default)]
pub struct TargetDecision {
    pub scope: TargetScopeRef,
    pub agent_name: String,
    pub tool_name: String,
    pub tool_input: Vec<u8>,
    /// allowed | denied
    pub status: String,
    pub reason: String,
    pub audit: Option<Json>,
}

fn audit_json(audit: &Option<Json>) -> Json {
    audit.clone().unwrap_or_else(|| serde_json::json!({}))
}

fn tool_input_json(raw: &[u8]) -> Result<Json> {
    if raw.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_slice(raw).map_err(|e| Error::msg(e.to_string()))
}

impl DB {
    /// `FindOrCreateTargetRequest`: return the open request for (scope, host,
    /// port), creating it once. Concurrent callers share one pending row.
    pub async fn find_or_create_target_request(&self, r: &TargetRequest) -> Result<(i64, bool)> {
        r.scope.check()?;
        let raw = tool_input_json(&r.tool_input)?;
        let rule_id = if r.rule_id != 0 {
            Some(r.rule_id)
        } else {
            None
        };
        let (task_id, conv_id) = r.scope.pending_scope();
        let key = target_key(&r.host, r.port);
        let audit = audit_json(&r.audit);
        let row = self.query_opt(
            "INSERT INTO intercept_pending(rule_id, conversation_id, task_id, agent_name, tool_name, tool_input, reason, decision_source, audit, target_key)
VALUES ($1, $2, $3, $4, $5, $6, $7, 'target', $8, $9)
ON CONFLICT (COALESCE(task_id,''), COALESCE(conversation_id,0), target_key)
    WHERE status = 'pending' AND target_key IS NOT NULL
DO NOTHING RETURNING id",
            &[&rule_id, &conv_id, &task_id, &r.agent_name, &r.tool_name, &raw, &r.reason, &audit, &key],
        ).await?;
        if let Some(row) = row {
            return Ok((row.get(0), true));
        }
        // The open row was created (or decided) in between — look it up.
        let task_key = task_id.unwrap_or_default();
        let row = self.query_opt(
            "SELECT id FROM intercept_pending
WHERE COALESCE(task_id,'')=$1 AND COALESCE(conversation_id,0)=$2 AND target_key=$3 AND status='pending'",
            &[&task_key, &r.scope.conversation_id, &key],
        ).await?;
        Ok(row
            .map(|r| (r.get::<_, i64>(0), false))
            .unwrap_or((0, false)))
    }

    /// `TargetRequestKey`: the target key of an approval row ("" for classic asks).
    pub async fn target_request_key(&self, id: i64) -> Result<String> {
        let row = self
            .query_opt(
                "SELECT target_key FROM intercept_pending WHERE id=$1",
                &[&id],
            )
            .await?;
        Ok(row
            .map(|r| r.get::<_, Option<String>>(0).unwrap_or_default())
            .unwrap_or_default())
    }

    /// `LogTargetDecision`: record a non-human gate outcome as already decided.
    pub async fn log_target_decision(&self, d: &TargetDecision) -> Result<i64> {
        d.scope.check()?;
        let raw = tool_input_json(&d.tool_input)?;
        let (task_id, conv_id) = d.scope.pending_scope();
        let audit = audit_json(&d.audit);
        let row = self.query_one(
            "INSERT INTO intercept_pending(conversation_id, task_id, agent_name, tool_name, tool_input, status, reason, decided_at, decision_source, audit, decided_by)
VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), 'target', $8, 'system') RETURNING id",
            &[&conv_id, &task_id, &d.agent_name, &d.tool_name, &raw, &d.status, &d.reason, &audit],
        ).await?;
        Ok(row.get(0))
    }

    /// `ResolveTargetRequest`: settle an open request. An approval pins the
    /// displayed addresses (superseding the active pin); a denial pins nothing.
    pub async fn resolve_target_request(
        &self,
        id: i64,
        allowed: bool,
        approver: &str,
        remote_addr: &str,
    ) -> Result<(Option<TargetPin>, bool)> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let row = tx
            .query_opt(
                "SELECT task_id, conversation_id, audit FROM intercept_pending
WHERE id=$1 AND status='pending' AND target_key IS NOT NULL FOR UPDATE",
                &[&id],
            )
            .await?;
        let Some(row) = row else {
            return Ok((None, false));
        };
        let task_str: Option<String> = row.get(0);
        let conv_id: Option<i64> = row.get(1);
        let audit: Json = row
            .get::<_, Option<Json>>(2)
            .unwrap_or_else(|| serde_json::json!({}));

        let mut status = "denied";
        let mut action = "deny";
        let mut reason = "user rejected the target; nothing was pinned";
        let mut execution = "not_executed";
        let mut pin = None;

        if allowed {
            let targets = audit
                .get("targets")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if targets.len() != 1 {
                return Err(Error::msg(format!(
                    "target request {id} records {} targets, want 1",
                    targets.len()
                )));
            }
            let mut scope = TargetScopeRef {
                conversation_id: conv_id.unwrap_or(0),
                ..TargetScopeRef::default()
            };
            if let Some(t) = task_str {
                scope.task_id = t
                    .parse::<i64>()
                    .map_err(|e| Error::msg(format!("target request {id}: task id {t:?}: {e}")))?;
            }
            pin =
                Some(write_target_pin(&tx, &scope, id, &targets[0], approver, remote_addr).await?);
            status = "allowed";
            action = "allow";
            reason = "user approved the target; the displayed addresses were pinned";
            execution = "unknown";
            if audit
                .get("merged_ask")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && audit.get("correlation").and_then(|v| v.as_str()) == Some("exact")
            {
                execution = "awaiting_result";
            }
        } else if let Some(targets) = audit.get("targets").and_then(|v| v.as_array()) {
            // Only the row's audit is patched; no pin is written.
            let _ = targets;
        }

        let mut patch = serde_json::json!({
            "effective_action": action,
            "decision_reason": reason,
            "execution_status": execution,
            "targets": audit.get("targets").cloned().unwrap_or_else(|| serde_json::json!([])),
        });
        if !approver.is_empty() {
            patch["approver"] = serde_json::json!(approver);
        }
        if !remote_addr.is_empty() {
            patch["approver_addr"] = serde_json::json!(remote_addr);
        }
        tx.execute(
            "UPDATE intercept_pending SET status=$2, decided_at=NOW(), decided_by='human', audit=COALESCE(audit,'{}'::jsonb) || $3::jsonb WHERE id=$1",
            &[&id, &status, &patch],
        ).await?;
        tx.commit().await?;
        Ok((pin, true))
    }
}

/// `writeTargetPin`: the only writer of target_pins rows.
async fn write_target_pin(
    tx: &deadpool_postgres::Transaction<'_>,
    scope: &TargetScopeRef,
    intercept_id: i64,
    target: &Json,
    approver: &str,
    remote_addr: &str,
) -> Result<TargetPin> {
    scope.check()?;
    let host = target.get("host").and_then(|v| v.as_str()).unwrap_or("");
    let port = target.get("port").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let version_row = tx.query_one(
        &format!("SELECT COALESCE(max(version),0)+1 FROM target_pins WHERE {PIN_SCOPE} AND host=$3 AND port=$4"),
        &[&scope.task_id, &scope.conversation_id, &host, &port],
    ).await?;
    let version: i32 = version_row.get::<_, i64>(0) as i32;

    let mut ips: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(list) = target.get("resolved_ips").and_then(|v| v.as_array()) {
        for ip in list {
            if let Some(s) = ip.as_str() {
                if !s.is_empty() {
                    ips.insert(s.to_string());
                }
            }
        }
    }
    if ips.is_empty() {
        return Err(Error::sentinel(ERR_TARGET_PIN_EMPTY, ERR_TARGET_PIN_EMPTY));
    }
    let prev = tx.query_opt(
        &format!("SELECT {PIN_COLS} FROM target_pins WHERE {PIN_SCOPE} AND host=$3 AND port=$4 AND status='active' FOR UPDATE"),
        &[&scope.task_id, &scope.conversation_id, &host, &port],
    ).await?;
    if let Some(prev) = prev {
        for ip in scan_pin(&prev)?.pinned_ips {
            ips.insert(ip);
        }
        let prev_id: i64 = prev.get(0);
        tx.execute(
            "UPDATE target_pins SET status='superseded' WHERE id=$1",
            &[&prev_id],
        )
        .await?;
    }
    let pinned: Vec<String> = ips.into_iter().collect();
    let (task_id, conv_id) = if scope.task_id > 0 {
        (Some(scope.task_id), None)
    } else {
        (None, Some(scope.conversation_id))
    };
    let resolver = target
        .get("resolver")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let resolved_at = target
        .get("resolved_at")
        .and_then(|v| v.as_str())
        .and_then(crate::db::gotime::parse_rfc3339)
        .unwrap_or_else(std::time::SystemTime::now);
    let name_only = target
        .get("name_only_effective")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let row = tx.query_one(
        &format!("INSERT INTO target_pins(task_id, conversation_id, host, port, version, pinned_ips, resolver, resolved_at, intercept_id, decided_by, decided_from, name_only_effective)
VALUES ($1, $2, $3, $4, $5, $6::text[], $7, $8, $9, $10, $11, $12)
RETURNING {PIN_COLS}"),
        &[&task_id, &conv_id, &host, &port, &version, &pinned, &resolver, &resolved_at, &intercept_id, &approver, &remote_addr, &name_only],
    ).await?;
    scan_pin(&row)
}
