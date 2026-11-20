//! Activity outbox — port of `db/activity_outbox.go`.
//!
//! Keeps one activity that `AppendActivity` could not store, so a transient
//! write failure never loses a worker step. Replays in order, atomically.

use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::exploration::Activity;

const ACTIVITY_OUTBOX_MAX_ATTEMPTS: i32 = 5;

/// `Activity` + `Detail` — the outbox payload. `Activity.Detail` is omitted from
/// ordinary JSON; the outbox must keep it so a replayed row is the same step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ActivityPayload {
    #[serde(flatten)]
    activity: ActivitySerde,
    #[serde(default)]
    detail: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ActivitySerde {
    #[serde(default)]
    id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node_id: Option<i64>,
    #[serde(default)]
    worker: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    tool: String,
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    metadata: serde_json::Value,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    input_tokens: Option<i32>,
    #[serde(default)]
    output_tokens: Option<i32>,
    #[serde(default)]
    cache_read_tokens: Option<i32>,
    #[serde(default)]
    cache_write_tokens: Option<i32>,
    #[serde(default)]
    source_task_id: i64,
    #[serde(default)]
    inherited: bool,
    #[serde(default)]
    main_seg: Option<i32>,
}

impl ActivitySerde {
    fn from_activity(a: &Activity) -> Self {
        ActivitySerde {
            id: a.id,
            node_id: a.node_id,
            worker: a.worker.clone(),
            kind: a.kind.clone(),
            tool: a.tool.clone(),
            tool_use_id: a.tool_use_id.clone(),
            is_error: a.is_error,
            summary: a.summary.clone(),
            detail: String::new(), // Go omits Detail from ordinary Activity JSON
            metadata: a.metadata.clone(),
            created_at: crate::db::gotime::format_rfc3339(a.created_at),
            input_tokens: a.input_tokens,
            output_tokens: a.output_tokens,
            cache_read_tokens: a.cache_read_tokens,
            cache_write_tokens: a.cache_write_tokens,
            source_task_id: a.source_task_id,
            inherited: a.inherited,
            main_seg: a.main_seg,
        }
    }

    fn into_activity(mut self, detail: String) -> Activity {
        self.detail = detail;
        Activity {
            id: self.id,
            node_id: self.node_id,
            worker: self.worker,
            kind: self.kind,
            tool: self.tool,
            tool_use_id: self.tool_use_id,
            is_error: self.is_error,
            summary: self.summary,
            detail: self.detail,
            metadata: self.metadata,
            created_at: crate::db::gotime::parse_rfc3339(&self.created_at)
                .unwrap_or(SystemTime::UNIX_EPOCH),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            source_task_id: self.source_task_id,
            inherited: self.inherited,
            main_seg: self.main_seg,
        }
    }
}

fn marshal_activity(a: &Activity) -> Result<Vec<u8>> {
    let payload = ActivityPayload {
        activity: ActivitySerde::from_activity(a),
        detail: a.detail.clone(),
    };
    serde_json::to_vec(&payload)
        .map_err(|e| crate::db::Error::msg(format!("marshal activity: {e}")))
}

fn unmarshal_activity(raw: &[u8]) -> Result<Activity> {
    let payload: ActivityPayload = serde_json::from_slice(raw)
        .map_err(|e| crate::db::Error::msg(format!("unmarshal activity: {e}")))?;
    Ok(payload.activity.into_activity(payload.detail))
}

impl crate::db::exploration::ExplorationStore {
    /// `EnqueueActivity`: keep one activity that `AppendActivity` could not store.
    pub async fn enqueue_activity(&self, a: &Activity, cause: &str) -> Result<()> {
        let raw = marshal_activity(a)?;
        let payload: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|e| crate::db::Error::msg(format!("outbox payload: {e}")))?;
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] =
            [&self.id(), &payload, &cause];
        self.db()
            .execute(
                "INSERT INTO activity_outbox (exploration_id, payload, last_error) VALUES ($1, $2::jsonb, $3)",
                &params,
            )
            .await?;
        Ok(())
    }

    /// `FlushActivityOutbox`: replay queued activities in order, deleting each row
    /// that lands. Insert + delete commit together. A stuck row is retried up to
    /// `ACTIVITY_OUTBOX_MAX_ATTEMPTS`, then dropped; undecodable payloads are dropped.
    pub async fn flush_activity_outbox(&self) -> Result<i64> {
        let exp_id = self.id();
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&exp_id];
        let rows = self
            .db()
            .query(
                "SELECT id, payload, attempts FROM activity_outbox WHERE exploration_id=$1 ORDER BY id",
                &params,
            )
            .await?;
        struct Item {
            id: i64,
            payload: serde_json::Value,
            attempts: i32,
        }
        let items: Vec<Item> = rows
            .iter()
            .map(|r| Item {
                id: r.get(0),
                payload: r.get(1),
                attempts: r.get(2),
            })
            .collect();

        let mut stored = 0i64;
        for it in &items {
            let raw = serde_json::to_vec(&it.payload)
                .map_err(|e| crate::db::Error::msg(format!("outbox payload: {e}")))?;
            let a = match unmarshal_activity(&raw) {
                Ok(a) => a,
                Err(_) => {
                    self.db()
                        .execute("DELETE FROM activity_outbox WHERE id=$1", &[&it.id])
                        .await?;
                    continue;
                }
            };
            let mut client = self.db().pool.get().await?;
            let tx = client.transaction().await?;
            match self.append_activity_tx(&tx, &a).await {
                Ok(_) => {
                    tx.execute("DELETE FROM activity_outbox WHERE id=$1", &[&it.id])
                        .await?;
                    tx.commit().await?;
                    stored += 1;
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    let next = it.attempts + 1;
                    if next >= ACTIVITY_OUTBOX_MAX_ATTEMPTS {
                        eprintln!(
                            "[activity] outbox {} for exploration {} dropped after {} attempts: {}",
                            it.id, exp_id, next, e
                        );
                        self.db()
                            .execute("DELETE FROM activity_outbox WHERE id=$1", &[&it.id])
                            .await?;
                    } else {
                        self.db()
                            .execute(
                                "UPDATE activity_outbox SET attempts=$2, last_error=$3 WHERE id=$1",
                                &[&it.id, &next, &e.to_string()],
                            )
                            .await?;
                    }
                }
            }
        }
        Ok(stored)
    }
}

// Keep private helpers referenced (tests exercise them through the store).
#[allow(dead_code)]
fn _helpers(a: &Activity) -> bool {
    unmarshal_activity(&marshal_activity(a).unwrap()).is_ok()
}
