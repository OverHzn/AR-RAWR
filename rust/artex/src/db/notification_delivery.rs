//! Notification delivery claiming + state transitions — port of
//! `db/notification_delivery.go`.
//!
//! Claiming uses a lease (state=sending, next_attempt_at in the future) so no DB
//! lock is held during the network send; a crash mid-delivery self-heals when the
//! lease expires. The retry counter is incremented at claim time.

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::notification_channels::NotificationChannel;
use crate::db::secrets::{open_secret, COL_NOTIF_CONFIG};
use crate::db::DB;

pub const MAX_NOTIFY_ATTEMPTS: i64 = 3;
pub const MAX_DIGEST_BATCH_SIZE: i64 = 500;

const STATE_PENDING: &str = "pending";
const STATE_FAILED: &str = "failed";
const STATE_SKIPPED: &str = "skipped";
const STATE_SENT: &str = "sent";

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationDelivery {
    pub id: i64,
    pub event_id: i64,
    pub channel_id: i64,
    pub state: String,
    pub attempts: i32,
    pub next_attempt_at: SystemTime,
    pub last_error: String,
    pub batch_id: Option<i64>,
    pub created_at: SystemTime,
    pub sent_at: Option<SystemTime>,
    pub snapshot: serde_json::Value,
    pub channel: Option<NotificationChannel>,
    pub finding_id: i64,
    pub event_kind: String,
    pub channel_name: String,
    pub channel_kind: String,
}

impl Default for NotificationDelivery {
    fn default() -> Self {
        NotificationDelivery {
            id: 0,
            event_id: 0,
            channel_id: 0,
            state: String::new(),
            attempts: 0,
            next_attempt_at: SystemTime::UNIX_EPOCH,
            last_error: String::new(),
            batch_id: None,
            created_at: SystemTime::UNIX_EPOCH,
            sent_at: None,
            snapshot: serde_json::Value::Null,
            channel: None,
            finding_id: 0,
            event_kind: String::new(),
            channel_name: String::new(),
            channel_kind: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotificationDeliveryFilter {
    pub channel_id: i64,
    pub state: String,
    pub event_kind: String,
}

impl NotificationDeliveryFilter {
    fn where_clause(&self) -> (String, Vec<FilterArg>) {
        let mut conds = Vec::new();
        let mut args = Vec::new();
        if self.channel_id > 0 {
            args.push(FilterArg::Bigint(self.channel_id));
            conds.push(format!("d.channel_id=${}", args.len()));
        }
        if !self.state.is_empty() {
            args.push(FilterArg::Text(self.state.clone()));
            conds.push(format!("d.state=${}", args.len()));
        }
        if !self.event_kind.is_empty() {
            args.push(FilterArg::Text(self.event_kind.clone()));
            conds.push(format!("e.kind=${}", args.len()));
        }
        if conds.is_empty() {
            (String::new(), args)
        } else {
            (format!(" WHERE {}", conds.join(" AND ")), args)
        }
    }
}

enum FilterArg {
    Bigint(i64),
    Text(String),
}

fn arg_refs(args: &[FilterArg]) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    args.iter()
        .map(|a| match a {
            FilterArg::Bigint(n) => n as &(dyn tokio_postgres::types::ToSql + Sync),
            FilterArg::Text(s) => s as &(dyn tokio_postgres::types::ToSql + Sync),
        })
        .collect()
}

const JOINED_QUERY: &str = "SELECT d.id, d.event_id, d.channel_id, d.state, d.attempts, d.next_attempt_at, d.last_error, d.batch_id, d.created_at, d.sent_at, e.snapshot, e.kind, e.finding_id, c.id, c.name, c.kind, c.enabled, c.config, c.mode, c.filter, c.rate_per_min FROM notification_deliveries d JOIN notification_events e ON e.id = d.event_id JOIN notification_channels c ON c.id = d.channel_id";

fn scan_delivery(row: &tokio_postgres::Row) -> Result<NotificationDelivery> {
    let cfg_val: serde_json::Value = row.get(17);
    let cfg = open_secret(&COL_NOTIF_CONFIG, &cfg_val.to_string()).map_err(Error::msg)?;
    let channel = NotificationChannel {
        id: row.get(13),
        name: row.get(14),
        kind: row.get(15),
        enabled: Some(row.get(16)),
        config: serde_json::from_str(&cfg).unwrap_or(serde_json::json!({})),
        mode: row.get(18),
        filter: row.get(19),
        rate_per_min: row.get(20),
        created_at: SystemTime::UNIX_EPOCH,
        updated_at: SystemTime::UNIX_EPOCH,
    };
    Ok(NotificationDelivery {
        id: row.get(0),
        event_id: row.get(1),
        channel_id: row.get(2),
        state: row.get(3),
        attempts: row.get(4),
        next_attempt_at: row.get(5),
        last_error: row.get::<_, Option<String>>(6).unwrap_or_default(),
        batch_id: row.get(7),
        created_at: row.get(8),
        sent_at: row.get(9),
        snapshot: row.get(10),
        event_kind: row.get(11),
        finding_id: row.get(12),
        channel_name: channel.name.clone(),
        channel_kind: channel.kind.clone(),
        channel: Some(channel),
    })
}

/// `truncateNotifyError`: cap an error to 500 bytes on a UTF-8 boundary.
pub fn truncate_notify_error(msg: &str) -> String {
    const MAX: usize = 500;
    if msg.len() <= MAX {
        return msg.to_string();
    }
    let mut cut = MAX;
    while cut > 0 && !msg.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &msg[..cut])
}

/// `placeholders`: `$n` strings starting at `start` for an IN (...) list.
pub fn placeholders(start: usize, ids: &[i64]) -> (String, Vec<i64>) {
    let ph: Vec<String> = (0..ids.len()).map(|i| format!("${}", start + i)).collect();
    (ph.join(","), ids.to_vec())
}

impl DB {
    /// `MarkDeliveriesSent`: mark a batch sent (no-op on empty).
    pub async fn mark_deliveries_sent(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let (ph, args) = placeholders(2, ids);
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&STATE_SENT];
        refs.extend(
            args.iter()
                .map(|id| id as &(dyn tokio_postgres::types::ToSql + Sync)),
        );
        self.execute(
            &format!("UPDATE notification_deliveries SET state=$1, sent_at=now(), last_error='' WHERE id IN ({ph})"),
            &refs,
        ).await?;
        Ok(())
    }

    /// `RescheduleDeliveries`: return to pending with a delayed retry time.
    pub async fn reschedule_deliveries(
        &self,
        ids: &[i64],
        delay_secs: f64,
        err_msg: &str,
    ) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let (ph, args) = placeholders(4, ids);
        let msg = truncate_notify_error(err_msg);
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&STATE_PENDING, &delay_secs, &msg];
        refs.extend(
            args.iter()
                .map(|id| id as &(dyn tokio_postgres::types::ToSql + Sync)),
        );
        self.execute(
            &format!("UPDATE notification_deliveries SET state=$1, next_attempt_at=now()+make_interval(secs => $2), last_error=$3 WHERE id IN ({ph})"),
            &refs,
        ).await?;
        Ok(())
    }

    /// `DeferDeliveries`: return to pending, immediately claimable, and revoke the
    /// attempt counted at claim time (GREATEST(attempts-1, 0)).
    pub async fn defer_deliveries(&self, ids: &[i64], reason: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let (ph, args) = placeholders(3, ids);
        let reason = truncate_notify_error(reason);
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&STATE_PENDING, &reason];
        refs.extend(
            args.iter()
                .map(|id| id as &(dyn tokio_postgres::types::ToSql + Sync)),
        );
        self.execute(
            &format!("UPDATE notification_deliveries SET state=$1, attempts=GREATEST(attempts-1, 0), next_attempt_at=now(), last_error=$2 WHERE id IN ({ph})"),
            &refs,
        ).await?;
        Ok(())
    }

    /// `FailDeliveries`: mark a batch permanently failed.
    pub async fn fail_deliveries(&self, ids: &[i64], err_msg: &str) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let (ph, args) = placeholders(3, ids);
        let msg = truncate_notify_error(err_msg);
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&STATE_FAILED, &msg];
        refs.extend(
            args.iter()
                .map(|id| id as &(dyn tokio_postgres::types::ToSql + Sync)),
        );
        self.execute(
            &format!(
                "UPDATE notification_deliveries SET state=$1, last_error=$2 WHERE id IN ({ph})"
            ),
            &refs,
        )
        .await?;
        Ok(())
    }

    /// `RetryNotificationDelivery`: manual resend — reset to pending, zero attempts.
    pub async fn retry_notification_delivery(&self, id: i64) -> Result<()> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] =
            [&id, &STATE_PENDING, &STATE_FAILED, &STATE_SKIPPED];
        let n = self.execute(
            "UPDATE notification_deliveries SET state=$2, attempts=0, next_attempt_at=now(), last_error='' \
             WHERE id=$1 AND state IN ($3,$4)",
            &params,
        ).await?;
        if n == 0 {
            return Err(Error::msg(format!(
                "delivery {id} does not exist or its current state does not allow resending"
            )));
        }
        Ok(())
    }

    /// `ListNotificationDeliveries`: delivery history paginated, newest first.
    pub async fn list_notification_deliveries(
        &self,
        f: &NotificationDeliveryFilter,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<NotificationDelivery>, i64)> {
        let page = if page < 1 { 1 } else { page };
        let page_size = if page_size <= 0 || page_size > 200 {
            50
        } else {
            page_size
        };
        let (where_sql, args) = f.where_clause();
        let refs = arg_refs(&args);
        let total: i64 = self.query_one(
            &format!("SELECT count(*) FROM notification_deliveries d JOIN notification_events e ON e.id=d.event_id{where_sql}"),
            &refs,
        ).await?.get(0);
        let limit = page_size as i64;
        let offset = (page - 1) as i64 * page_size as i64;
        let n = args.len();
        let mut all_refs = refs;
        all_refs.push(&limit);
        all_refs.push(&offset);
        let q = format!(
            "{JOINED_QUERY}{where_sql} ORDER BY d.id DESC LIMIT ${} OFFSET ${}",
            n + 1,
            n + 2
        );
        let rows = self.query(&q, &all_refs).await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_delivery(r)?);
        }
        Ok((out, total))
    }
}
