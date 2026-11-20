//! Target-gate exemption list + change log — slice of `db/target_pins.go`.

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::DB;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TargetExemption {
    pub host: String,
    pub port: i32,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetExemptionChange {
    pub id: i64,
    pub changed_at: SystemTime,
    pub changed_by: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub digest: String,
}

impl DB {
    /// `TargetExemptions`: stored list + raw settings value (empty if never stored).
    pub async fn target_exemptions(&self) -> Result<(Vec<TargetExemption>, String)> {
        let (raw, ok) = self
            .get_setting(crate::db::seed::SETTING_TARGET_EXEMPT_HOSTS)
            .await?;
        if !ok {
            return Ok((Vec::new(), String::new()));
        }
        let list: Vec<TargetExemption> = serde_json::from_str(&raw)
            .map_err(|e| Error::msg(format!("stored target-gate exemption list: {e}")))?;
        Ok((list, raw))
    }

    /// `ReplaceTargetExemptions`: store the list + append a change-log row in one tx.
    pub async fn replace_target_exemptions(
        &self,
        list: &[TargetExemption],
        changed_by: &str,
        digest: &str,
    ) -> Result<()> {
        let after = serde_json::to_string(list).map_err(|e| Error::msg(e.to_string()))?;
        let after_val: serde_json::Value =
            serde_json::from_str(&after).map_err(|e| Error::msg(e.to_string()))?;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let row = tx
                .query_opt(
                    "SELECT value FROM settings WHERE key=$1 FOR UPDATE",
                    &[&crate::db::seed::SETTING_TARGET_EXEMPT_HOSTS],
                )
                .await?;
            let before: Option<serde_json::Value> = row.and_then(|r| {
                let raw: String = r.get(0);
                serde_json::from_str(&raw).ok()
            });
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] =
                [&crate::db::seed::SETTING_TARGET_EXEMPT_HOSTS, &after];
            tx.execute(
                "INSERT INTO settings(key, value) VALUES ($1, $2) \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
                &params,
            )
            .await?;
            let log_params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] =
                [&changed_by, &before, &after_val, &digest];
            tx.execute(
                "INSERT INTO target_gate_exemption_log(changed_by, before, after, digest) \
                 VALUES ($1, $2::jsonb, $3::jsonb, $4)",
                &log_params,
            )
            .await?;
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

    /// `ListTargetExemptionLog`: the most recent changes, newest first.
    pub async fn list_target_exemption_log(
        &self,
        limit: i64,
    ) -> Result<Vec<TargetExemptionChange>> {
        let rows = self
            .query(
                "SELECT id, changed_at, changed_by, before, after, digest \
             FROM target_gate_exemption_log ORDER BY id DESC LIMIT $1",
                &[&limit],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TargetExemptionChange {
                id: r.get(0),
                changed_at: r.get(1),
                changed_by: r.get(2),
                before: r
                    .get::<_, Option<serde_json::Value>>(3)
                    .unwrap_or(serde_json::Value::Null),
                after: r
                    .get::<_, Option<serde_json::Value>>(4)
                    .unwrap_or(serde_json::Value::Null),
                digest: r.get(5),
            })
            .collect())
    }
}
