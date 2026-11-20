//! Notification-event persistence — the writer slice of `db/notification.go`.
//!
//! A `notification_events` row is an immutable "a finding reached this state"
//! record that the delivery engine fans out to channels. This module ports the
//! two writers that have no dependency on the channel/fan-out machinery:
//!   - `RecordNotificationEventTx` — best-effort insert on a SAVEPOINT inside a
//!     caller's transaction (a failure never aborts the surrounding finding write),
//!   - `AddNotificationEvent` — the standalone-transaction variant.
//!
//! `FanOutPendingEvents` + the channel/filter/delivery machinery land with the
//! notify delivery cluster.

use crate::db::error::Result;
use crate::db::DB;
use crate::notify::Snapshot;

/// `recordNotificationEventTx`: best-effort insert of one event on a SAVEPOINT so a
/// failure is isolated and the caller's transaction stays usable. Returns whether
/// the event was written; failures are logged, not propagated (the finding record
/// is unaffected).
pub async fn record_notification_event_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    kind: &str,
    finding_id: i64,
    snap: &Snapshot,
) -> bool {
    let value = snapshot_json(snap);
    if let Err(e) = tx.batch_execute("SAVEPOINT notify_event").await {
        eprintln!("[notify] failed to create savepoint finding={finding_id}: {e}");
        return false;
    }
    let inserted = tx
        .execute(
            "INSERT INTO notification_events(kind,finding_id,snapshot) VALUES($1,$2,$3)",
            &[&kind, &finding_id, &value],
        )
        .await;
    match inserted {
        Ok(_) => {
            let _ = tx.batch_execute("RELEASE SAVEPOINT notify_event").await;
            true
        }
        Err(e) => {
            eprintln!(
                "[notify] failed to write delivery event finding={finding_id} (finding record unaffected): {e}"
            );
            if let Err(rb) = tx.batch_execute("ROLLBACK TO SAVEPOINT notify_event").await {
                eprintln!("[notify] failed to roll back to savepoint finding={finding_id}: {rb}");
            }
            false
        }
    }
}

impl DB {
    /// `AddNotificationEvent`: standalone-transaction event insert (e.g. a channel's
    /// "send test message", which has no real finding). Returns the new event id.
    pub async fn add_notification_event(
        &self,
        kind: &str,
        finding_id: i64,
        snap: &Snapshot,
    ) -> Result<i64> {
        let value = snapshot_json(snap);
        let row = self
            .query_one(
                "INSERT INTO notification_events(kind,finding_id,snapshot) VALUES($1,$2,$3) RETURNING id",
                &[&kind, &finding_id, &value],
            )
            .await?;
        Ok(row.get::<_, i64>(0))
    }
}

/// `snapshot_json`: the on-the-wire form of a `notify.Snapshot`, matching the Go
/// struct's json tags (`from_status` / `to_status` are omitempty).
fn snapshot_json(s: &Snapshot) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("kind".into(), serde_json::json!(s.kind));
    map.insert("finding_id".into(), serde_json::json!(s.finding_id));
    map.insert("task_id".into(), serde_json::json!(s.task_id));
    map.insert("vulnclass".into(), serde_json::json!(s.vulnclass));
    map.insert("name".into(), serde_json::json!(s.name));
    map.insert("severity".into(), serde_json::json!(s.severity));
    map.insert("summary".into(), serde_json::json!(s.summary));
    map.insert("asset_ids".into(), serde_json::json!(s.asset_ids));
    if !s.from_status.is_empty() {
        map.insert("from_status".into(), serde_json::json!(s.from_status));
    }
    if !s.to_status.is_empty() {
        map.insert("to_status".into(), serde_json::json!(s.to_status));
    }
    serde_json::Value::Object(map)
}

/// `SetFindingStatusTx`: change one finding's triage status (+ node payload mirror)
/// and register a status-change notification event, all in the caller's tx.
/// Returns `(from, found, changed, notified)`. No event is written when the status
/// did not actually change (idempotent replays must not produce push noise).
#[allow(dead_code)]
pub(crate) async fn set_finding_status_tx(
    ctx: &crate::llm::Context,
    tx: &deadpool_postgres::Transaction<'_>,
    id: i64,
    status: &str,
) -> Result<(String, bool, bool, bool)> {
    let row = tx
        .query_opt(
            "SELECT vulnclass, name, severity, summary, task_id, asset_ids, status, node_id \
             FROM findings WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    let Some(row) = row else {
        return Ok((String::new(), false, false, false));
    };
    let vulnclass: String = row.get(0);
    let name: String = row.get(1);
    let severity: String = row.get(2);
    let summary: String = row.get(3);
    let task_id: Option<i64> = row.get(4);
    let assets: serde_json::Value = row.get(5);
    let from: String = row.get(6);
    let node_id: Option<i64> = row.get(7);
    if from == status {
        return Ok((from, true, false, false));
    }
    tx.execute("UPDATE findings SET status=$2 WHERE id=$1", &[&id, &status])
        .await?;
    if let Some(nid) = node_id {
        tx.execute(
            "UPDATE exploration_nodes SET payload = jsonb_set(payload, '{status}', to_jsonb($1::text)) WHERE id = $2",
            &[&status, &nid],
        )
        .await?;
    }
    let asset_ids: Vec<i64> = serde_json::from_value(assets).unwrap_or_default();
    let snap = Snapshot {
        kind: crate::notify::EVENT_FINDING_STATUS_CHANGED.to_string(),
        finding_id: id,
        task_id: task_id.unwrap_or(0),
        vulnclass,
        name,
        severity,
        summary,
        asset_ids,
        from_status: from.clone(),
        to_status: status.to_string(),
    };
    let notified =
        record_notification_event_tx(tx, crate::notify::EVENT_FINDING_STATUS_CHANGED, id, &snap)
            .await;
    let _ = ctx; // ctx-free path in Go; kept for signature symmetry with the callers.
    Ok((from, true, true, notified))
}
