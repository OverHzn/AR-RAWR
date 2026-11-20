//! Notification channel config + status-change events — slice of `db/notification.go`.
//!
//! A channel's `config` is sealed at rest. Status changes register a delivery
//! event in the same tx; disabling a channel marks its pending deliveries skipped.

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::secrets::{open_secret, seal_secret, COL_NOTIF_CONFIG};
use crate::db::DB;

pub const ERR_NOTIFICATION_CHANNEL_NOT_FOUND: &str = "notification channel does not exist";

pub const NOTIFY_STATE_PENDING: &str = "pending";
pub const NOTIFY_STATE_SENDING: &str = "sending";
pub const NOTIFY_STATE_SENT: &str = "sent";
pub const NOTIFY_STATE_FAILED: &str = "failed";
pub const NOTIFY_STATE_SKIPPED: &str = "skipped";
pub const NOTIFY_MODE_REALTIME: &str = "realtime";
pub const NOTIFY_MODE_DIGEST: &str = "digest";

pub fn valid_notify_mode(m: &str) -> bool {
    m == NOTIFY_MODE_REALTIME || m == NOTIFY_MODE_DIGEST
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationChannel {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub mode: String,
    /// Clear config JSON (sealed at rest).
    pub config: serde_json::Value,
    pub filter: serde_json::Value,
    pub enabled: Option<bool>,
    pub rate_per_min: i32,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl NotificationChannel {
    /// `IsEnabled`: a nil Enabled (not loaded) is treated as enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotificationStats {
    pub channels: i64,
    pub channels_on: i64,
    pub pending: i64,
    pub failed: i64,
    pub sent_today: i64,
    pub backlog_age_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindingStatusBatchResult {
    pub id: i64,
    pub from: String,
    pub found: bool,
    pub changed: bool,
    pub notified: bool,
}

const CHANNEL_COLS: &str =
    "id, name, kind, enabled, config, mode, filter, rate_per_min, created_at, updated_at";

fn scan_channel(row: &tokio_postgres::Row) -> Result<NotificationChannel> {
    let cfg_val: serde_json::Value = row.get(4);
    let cfg = open_secret(&COL_NOTIF_CONFIG, &cfg_val.to_string()).map_err(Error::msg)?;
    Ok(NotificationChannel {
        id: row.get(0),
        name: row.get(1),
        kind: row.get(2),
        enabled: Some(row.get(3)),
        config: serde_json::from_str(&cfg).unwrap_or(serde_json::json!({})),
        mode: row.get(5),
        filter: row.get(6),
        rate_per_min: row.get(7),
        created_at: row.get(8),
        updated_at: row.get(9),
    })
}

/// `assetDisplayName`: the most recognizable identifier for an asset type; empty
/// when nothing recognizable (never invents `asset#42`).
pub fn asset_display_name(
    typ: &str,
    domain: &str,
    ip: &str,
    url: &str,
    app_name: &str,
    bundle_id: &str,
) -> String {
    let pick = |vals: &[&str]| -> String {
        vals.iter()
            .find(|v| !v.trim().is_empty())
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    match typ {
        "root_domain" | "subdomain" => domain.to_string(),
        "ip" => ip.to_string(),
        "app" => pick(&[app_name, bundle_id]),
        "service" | "endpoint" => pick(&[url, domain, ip]),
        _ => pick(&[domain, ip, url, app_name]),
    }
}

impl DB {
    /// `ListNotificationChannels`: all channels, enabled first then id.
    pub async fn list_notification_channels(&self) -> Result<Vec<NotificationChannel>> {
        let rows = self
            .query(
                &format!(
                    "SELECT {CHANNEL_COLS} FROM notification_channels ORDER BY enabled DESC, id"
                ),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_channel(r)?);
        }
        Ok(out)
    }

    /// `NotificationChannelByID`: one channel; no-rows -> sentinel error.
    pub async fn notification_channel_by_id(&self, id: i64) -> Result<NotificationChannel> {
        let row = self
            .query_opt(
                &format!("SELECT {CHANNEL_COLS} FROM notification_channels WHERE id=$1"),
                &[&id],
            )
            .await?;
        match row {
            Some(r) => scan_channel(&r),
            None => Err(Error::msg(ERR_NOTIFICATION_CHANNEL_NOT_FOUND)),
        }
    }

    /// `SaveNotificationChannel`: insert (id==0) or update a channel. A negative
    /// rate is refused; explicit 0 is preserved (means no limit).
    pub async fn save_notification_channel(&self, c: &mut NotificationChannel) -> Result<i64> {
        let mode = if c.mode.is_empty() {
            NOTIFY_MODE_REALTIME.to_string()
        } else {
            c.mode.clone()
        };
        if c.rate_per_min < 0 {
            return Err(Error::msg("rate limit must not be negative"));
        }
        let config = if c.config.is_null() {
            serde_json::json!({})
        } else {
            c.config.clone()
        };
        let filter = if c.filter.is_null() {
            serde_json::json!({})
        } else {
            c.filter.clone()
        };
        let cfg = seal_secret(&COL_NOTIF_CONFIG, &config.to_string()).map_err(Error::msg)?;
        let cfg_val: serde_json::Value =
            serde_json::from_str(&cfg).map_err(|e| Error::msg(e.to_string()))?;
        let enabled = c.is_enabled();
        if c.id == 0 {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
                &c.name,
                &c.kind,
                &enabled,
                &cfg_val,
                &mode,
                &filter,
                &c.rate_per_min,
            ];
            let row = self.query_one(
                "INSERT INTO notification_channels(name,kind,enabled,config,mode,filter,rate_per_min) \
                 VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",
                &params,
            ).await?;
            Ok(row.get(0))
        } else {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 8] = [
                &c.id,
                &c.name,
                &c.kind,
                &enabled,
                &cfg_val,
                &mode,
                &filter,
                &c.rate_per_min,
            ];
            let n = self.execute(
                "UPDATE notification_channels SET name=$2, kind=$3, enabled=$4, config=$5, mode=$6, filter=$7, rate_per_min=$8 WHERE id=$1",
                &params,
            ).await?;
            if n == 0 {
                return Err(Error::msg(ERR_NOTIFICATION_CHANNEL_NOT_FOUND));
            }
            Ok(c.id)
        }
    }

    /// `SetNotificationChannelEnabled`: toggle; disabling marks pending/sending
    /// deliveries skipped so re-enabling doesn't emit a stale backlog.
    pub async fn set_notification_channel_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let n = tx
                .execute(
                    "UPDATE notification_channels SET enabled=$2 WHERE id=$1",
                    &[&id, &enabled],
                )
                .await?;
            if n == 0 {
                return Err(Error::msg(ERR_NOTIFICATION_CHANNEL_NOT_FOUND));
            }
            if !enabled {
                tx.execute(
                    "UPDATE notification_deliveries SET state=$2, last_error=$3 \
                     WHERE channel_id=$1 AND state IN ($4,$5)",
                    &[
                        &id,
                        &NOTIFY_STATE_SKIPPED,
                        &"channel disabled",
                        &NOTIFY_STATE_PENDING,
                        &NOTIFY_STATE_SENDING,
                    ],
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

    /// `DeleteNotificationChannel`: delete (deliveries cascade by FK).
    pub async fn delete_notification_channel(&self, id: i64) -> Result<()> {
        let n = self
            .execute("DELETE FROM notification_channels WHERE id=$1", &[&id])
            .await?;
        if n == 0 {
            return Err(Error::msg(ERR_NOTIFICATION_CHANNEL_NOT_FOUND));
        }
        Ok(())
    }

    /// `NotificationAssetNames`: resolved labels in the requested order (deduped,
    /// unresolved ids dropped).
    pub async fn notification_asset_names(&self, ids: &[i64]) -> Result<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut labels = std::collections::HashMap::new();
        for &id in ids {
            let row = self
                .query_opt(
                    "SELECT type, domain, ip, url, app_name, bundle_id FROM assets WHERE id=$1",
                    &[&id],
                )
                .await?;
            if let Some(r) = row {
                let get =
                    |i: usize| -> String { r.get::<_, Option<String>>(i).unwrap_or_default() };
                let (typ, domain, ip, url, app_name, bundle_id) =
                    (get(0), get(1), get(2), get(3), get(4), get(5));
                let label = asset_display_name(&typ, &domain, &ip, &url, &app_name, &bundle_id);
                labels.insert(id, label);
            }
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for &id in ids {
            if seen.insert(id) {
                if let Some(label) = labels.get(&id) {
                    if !label.is_empty() {
                        out.push(label.clone());
                    }
                }
            }
        }
        Ok(out)
    }

    /// `SetFindingStatusWithNotify`: status change + one event in one tx.
    pub async fn set_finding_status_with_notify(
        &self,
        id: i64,
        status: &str,
    ) -> Result<(String, bool, bool)> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            crate::db::notification::set_finding_status_tx(
                &crate::llm::Context::background(),
                &tx,
                id,
                status,
            )
            .await
        }
        .await;
        match run {
            Ok((from, found, _changed, notified)) => {
                tx.commit().await?;
                Ok((from, found, notified))
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `SetFindingStatusesWithNotify`: batch status change, one result per id.
    pub async fn set_finding_statuses_with_notify(
        &self,
        ids: &[i64],
        status: &str,
    ) -> Result<Vec<FindingStatusBatchResult>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let mut out = Vec::with_capacity(ids.len());
            for &id in ids {
                let (from, found, changed, notified) =
                    crate::db::notification::set_finding_status_tx(
                        &crate::llm::Context::background(),
                        &tx,
                        id,
                        status,
                    )
                    .await?;
                out.push(FindingStatusBatchResult {
                    id,
                    from,
                    found,
                    changed,
                    notified,
                });
            }
            Ok::<_, Error>(out)
        }
        .await;
        match run {
            Ok(out) => {
                tx.commit().await?;
                Ok(out)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `NotificationStatsSnapshot`: dashboard counts over channels + deliveries.
    pub async fn notification_stats_snapshot(&self) -> Result<NotificationStats> {
        let row = self.query_one(
            "SELECT \
             (SELECT count(*) FROM notification_channels), \
             (SELECT count(*) FROM notification_channels WHERE enabled), \
             (SELECT count(*) FROM notification_deliveries WHERE state IN ($1,$2)), \
             (SELECT count(*) FROM notification_deliveries WHERE state=$3), \
             (SELECT count(*) FROM notification_deliveries WHERE state=$4 AND sent_at >= date_trunc('day', now())), \
             COALESCE((SELECT EXTRACT(EPOCH FROM (now() - min(created_at))) * 1000 FROM notification_deliveries WHERE state=$1), 0)::bigint",
            &[&NOTIFY_STATE_PENDING, &NOTIFY_STATE_SENDING, &NOTIFY_STATE_FAILED, &NOTIFY_STATE_SENT],
        ).await?;
        Ok(NotificationStats {
            channels: row.get(0),
            channels_on: row.get(1),
            pending: row.get(2),
            failed: row.get(3),
            sent_today: row.get(4),
            backlog_age_ms: row.get(5),
        })
    }
}
