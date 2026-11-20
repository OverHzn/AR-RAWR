//! Notification fan-out — port of `db/notification.go` (FanOutPendingEvents).
//!
//! One tx: lock the un-fanned events FOR UPDATE SKIP LOCKED, match each snapshot
//! against every enabled channel's filter, insert deliveries, then mark all this
//! round's events dispatched (even those that hit no channel).

use crate::db::error::Result;
use crate::db::notification::snapshot_json;
use crate::db::notification_channels::NotificationChannel;
use crate::db::DB;
use crate::notify::Snapshot;

/// `listEnabledNotificationChannelsTx`: enabled channels inside a transaction.
async fn list_enabled_channels_tx(
    tx: &deadpool_postgres::Transaction<'_>,
) -> Result<Vec<NotificationChannel>> {
    let rows = tx
        .query(
            "SELECT id, name, kind, config, mode, filter, rate_per_min \
             FROM notification_channels WHERE enabled ORDER BY id",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let filter: serde_json::Value = r.get(5);
        let config: serde_json::Value = r.get(3);
        out.push(NotificationChannel {
            id: r.get(0),
            name: r.get(1),
            kind: r.get(2),
            mode: r.get(4),
            config,
            filter,
            enabled: Some(true),
            rate_per_min: r.get(6),
            created_at: std::time::SystemTime::UNIX_EPOCH,
            updated_at: std::time::SystemTime::UNIX_EPOCH,
        });
    }
    Ok(out)
}

impl DB {
    /// `FanOutPendingEvents`: dispatch pending events to matching channels.
    /// Returns `(event_count, delivery_count)`.
    pub async fn fan_out_pending_events(&self, limit: i32) -> Result<(i64, i64)> {
        let limit = if limit <= 0 { 200 } else { limit } as i64;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let channels = list_enabled_channels_tx(&tx).await?;
            let rows = tx
                .query(
                    "SELECT id, kind, finding_id, snapshot FROM notification_events \
                 WHERE NOT fanned_out ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $1",
                    &[&limit],
                )
                .await?;
            #[allow(dead_code)]
            struct Ev {
                id: i64,
                kind: String,
                finding_id: i64,
                snap: serde_json::Value,
            }
            let events: Vec<Ev> = rows
                .iter()
                .map(|r| Ev {
                    id: r.get(0),
                    kind: r.get(1),
                    finding_id: r.get(2),
                    snap: r.get(3),
                })
                .collect();
            if events.is_empty() {
                return Ok::<_, crate::db::Error>((0i64, 0i64));
            }
            // For each event, parse the stored snapshot + override kind from the row.
            let mut to_insert: Vec<(i64, i64)> = Vec::new();
            for ev in &events {
                let v = &ev.snap;
                let gs = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let mut snap = Snapshot {
                    kind: gs("kind"),
                    finding_id: v.get("finding_id").and_then(|x| x.as_i64()).unwrap_or(0),
                    task_id: v.get("task_id").and_then(|x| x.as_i64()).unwrap_or(0),
                    vulnclass: gs("vulnclass"),
                    name: gs("name"),
                    severity: gs("severity"),
                    summary: gs("summary"),
                    asset_ids: v
                        .get("asset_ids")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                        .unwrap_or_default(),
                    from_status: gs("from_status"),
                    to_status: gs("to_status"),
                };
                snap.kind = ev.kind.clone();
                let snap_val = snapshot_json(&snap);
                for ch in &channels {
                    let filter_bytes: Vec<i64> =
                        ch.filter.to_string().bytes().map(|b| b as i64).collect();
                    let parsed = crate::notify::parse_filter(filter_bytes).unwrap_or_default();
                    let matched = crate::notify::r#match(parsed, snap_val.clone()).unwrap_or(false);
                    if matched {
                        to_insert.push((ev.id, ch.id));
                    }
                }
            }
            if !to_insert.is_empty() {
                let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                    Vec::with_capacity(to_insert.len() * 2);
                for (eid, cid) in &to_insert {
                    params.push(eid);
                    params.push(cid);
                }
                let vals: Vec<String> = (0..to_insert.len())
                    .map(|i| format!("(${},${})", i * 2 + 1, i * 2 + 2))
                    .collect();
                tx.execute(
                    &format!(
                        "INSERT INTO notification_deliveries(event_id,channel_id) VALUES {}",
                        vals.join(",")
                    ),
                    &params,
                )
                .await?;
            }
            // Mark all this round's events dispatched.
            let mut mark_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = events
                .iter()
                .map(|e| &e.id as &(dyn tokio_postgres::types::ToSql + Sync))
                .collect();
            let ph: Vec<String> = (0..events.len()).map(|i| format!("${}", i + 1)).collect();
            tx.execute(
                &format!(
                    "UPDATE notification_events SET fanned_out=true WHERE id IN ({})",
                    ph.join(",")
                ),
                &mark_params,
            )
            .await?;
            mark_params.clear();
            Ok((events.len() as i64, to_insert.len() as i64))
        }
        .await;
        match run {
            Ok(v) => {
                tx.commit().await?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}
