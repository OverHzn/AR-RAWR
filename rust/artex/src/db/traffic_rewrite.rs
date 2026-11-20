//! Traffic rewrite rule store — port of `db/traffic_rewrite.go`.
//!
//! Matchers/actions are sealed at rest and in clear here; they may hold secrets,
//! so callers must never return this struct as-is to a client.

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::secrets::{open_secret, seal_secret, COL_REWRITE_ACTIONS, COL_REWRITE_MATCHERS};
use crate::db::DB;

pub const MAX_TRAFFIC_REWRITE_RULES: i64 = 100;
pub const MAX_ENABLED_TRAFFIC_REWRITE_RULES: i64 = 50;
pub const ERR_TRAFFIC_REWRITE_RULE_LIMIT: &str = "the limit of 100 rewrite rules is reached";
pub const ERR_TRAFFIC_REWRITE_ENABLED_LIMIT: &str =
    "the limit of 50 enabled rewrite rules is reached";

/// `TrafficRewriteRule`: one row of `traffic_rewrite_rules` (clear matchers/actions).
#[derive(Debug, Clone, PartialEq)]
pub struct TrafficRewriteRule {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    /// `request` | `response`.
    pub phase: String,
    pub priority: i32,
    pub matchers: serde_json::Value,
    pub actions: serde_json::Value,
    pub note: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl Default for TrafficRewriteRule {
    fn default() -> Self {
        TrafficRewriteRule {
            id: 0,
            name: String::new(),
            enabled: false,
            phase: String::new(),
            priority: 0,
            matchers: serde_json::Value::Null,
            actions: serde_json::Value::Null,
            note: String::new(),
            created_at: SystemTime::UNIX_EPOCH,
            updated_at: SystemTime::UNIX_EPOCH,
        }
    }
}

const RULE_COLS: &str = "id, name, enabled, phase, priority, matchers::text, actions::text, note, created_at, updated_at";

fn scan_rule(row: &tokio_postgres::Row) -> Result<TrafficRewriteRule> {
    let matchers_raw: String = row.get(5);
    let actions_raw: String = row.get(6);
    let matchers = open_secret(&COL_REWRITE_MATCHERS, &matchers_raw).map_err(Error::msg)?;
    let actions = open_secret(&COL_REWRITE_ACTIONS, &actions_raw).map_err(Error::msg)?;
    Ok(TrafficRewriteRule {
        id: row.get(0),
        name: row.get(1),
        enabled: row.get(2),
        phase: row.get(3),
        priority: row.get(4),
        matchers: serde_json::from_str(&matchers).unwrap_or(serde_json::Value::Null),
        actions: serde_json::from_str(&actions).unwrap_or(serde_json::Value::Null),
        note: row.get(7),
        created_at: row.get(8),
        updated_at: row.get(9),
    })
}

fn seal_rule(r: &TrafficRewriteRule) -> Result<(serde_json::Value, serde_json::Value)> {
    let m = serde_json::to_string(&r.matchers).map_err(|e| Error::msg(e.to_string()))?;
    let a = serde_json::to_string(&r.actions).map_err(|e| Error::msg(e.to_string()))?;
    let sealed_m = seal_secret(&COL_REWRITE_MATCHERS, &m).map_err(Error::msg)?;
    let sealed_a = seal_secret(&COL_REWRITE_ACTIONS, &a).map_err(Error::msg)?;
    Ok((
        serde_json::from_str(&sealed_m).map_err(|e| Error::msg(e.to_string()))?,
        serde_json::from_str(&sealed_a).map_err(|e| Error::msg(e.to_string()))?,
    ))
}

fn no_rows() -> Error {
    Error::msg("no rows in result set")
}

impl DB {
    /// `ListTrafficRewriteRules`: every rule in execution order (priority, id).
    pub async fn list_rewrite_rules(&self) -> Result<Vec<TrafficRewriteRule>> {
        let rows = self
            .query(
                &format!("SELECT {RULE_COLS} FROM traffic_rewrite_rules ORDER BY priority, id"),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_rule(r)?);
        }
        Ok(out)
    }

    /// `GetTrafficRewriteRule`: one rule; no-rows error when absent.
    pub async fn get_rewrite_rule(&self, id: i64) -> Result<TrafficRewriteRule> {
        let row = self
            .query_opt(
                &format!("SELECT {RULE_COLS} FROM traffic_rewrite_rules WHERE id=$1"),
                &[&id],
            )
            .await?;
        match row {
            Some(r) => scan_rule(&r),
            None => Err(no_rows()),
        }
    }

    /// `CreateTrafficRewriteRule`: insert a rule; fails at the 100-rule or
    /// 50-enabled-rule limit.
    pub async fn create_rewrite_rule(&self, r: &TrafficRewriteRule) -> Result<TrafficRewriteRule> {
        let (matchers, actions) = seal_rule(r)?;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute("LOCK TABLE traffic_rewrite_rules IN SHARE ROW EXCLUSIVE MODE", &[])
                .await?;
            let n: i64 = tx
                .query_one("SELECT count(*) FROM traffic_rewrite_rules", &[])
                .await?
                .get(0);
            if n >= MAX_TRAFFIC_REWRITE_RULES {
                return Err(Error::sentinel(ERR_TRAFFIC_REWRITE_RULE_LIMIT, ERR_TRAFFIC_REWRITE_RULE_LIMIT));
            }
            if r.enabled {
                enabled_room(&tx, 0).await?;
            }
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
                &r.name, &r.enabled, &r.phase, &r.priority, &matchers, &actions, &r.note,
            ];
            let row = tx
                .query_one(
                    &format!(
                        "INSERT INTO traffic_rewrite_rules(name, enabled, phase, priority, matchers, actions, note) \
                         VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb, $7) RETURNING {RULE_COLS}"
                    ),
                    &params,
                )
                .await?;
            scan_rule(&row)
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

    /// `UpdateTrafficRewriteRule`: replace editable fields with `edit(stored)`.
    /// `edit` runs inside the tx and can keep stored values; its error aborts.
    pub async fn update_rewrite_rule<F>(&self, id: i64, edit: F) -> Result<TrafficRewriteRule>
    where
        F: FnOnce(TrafficRewriteRule) -> Result<TrafficRewriteRule>,
    {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute("LOCK TABLE traffic_rewrite_rules IN SHARE ROW EXCLUSIVE MODE", &[])
                .await?;
            let row = tx
                .query_opt(
                    &format!("SELECT {RULE_COLS} FROM traffic_rewrite_rules WHERE id=$1"),
                    &[&id],
                )
                .await?;
            let Some(row) = row else { return Err(no_rows()) };
            let stored = scan_rule(&row)?;
            let next = edit(stored.clone())?;
            if next.enabled && !stored.enabled {
                enabled_room(&tx, id).await?;
            }
            let (matchers, actions) = seal_rule(&next)?;
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 8] = [
                &id, &next.name, &next.enabled, &next.phase, &next.priority, &matchers, &actions,
                &next.note,
            ];
            let row = tx
                .query_one(
                    &format!(
                        "UPDATE traffic_rewrite_rules \
                         SET name=$2, enabled=$3, phase=$4, priority=$5, matchers=$6::jsonb, actions=$7::jsonb, note=$8 \
                         WHERE id=$1 RETURNING {RULE_COLS}"
                    ),
                    &params,
                )
                .await?;
            scan_rule(&row)
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

    /// `ToggleTrafficRewriteRule`: set enabled state; no-rows when absent.
    pub async fn toggle_rewrite_rule(&self, id: i64, enabled: bool) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "LOCK TABLE traffic_rewrite_rules IN SHARE ROW EXCLUSIVE MODE",
                &[],
            )
            .await?;
            if enabled {
                enabled_room(&tx, id).await?;
            }
            let n = tx
                .execute(
                    "UPDATE traffic_rewrite_rules SET enabled=$2 WHERE id=$1",
                    &[&id, &enabled],
                )
                .await?;
            if n == 0 {
                Err(no_rows())
            } else {
                Ok(())
            }
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

    /// `DeleteTrafficRewriteRule`: remove a rule; no-rows when absent.
    pub async fn delete_rewrite_rule(&self, id: i64) -> Result<()> {
        let n = self
            .execute("DELETE FROM traffic_rewrite_rules WHERE id=$1", &[&id])
            .await?;
        if n == 0 {
            Err(no_rows())
        } else {
            Ok(())
        }
    }
}

/// `trafficRewriteEnabledRoom`: fail when enabling `id` would exceed the limit.
async fn enabled_room(tx: &deadpool_postgres::Transaction<'_>, id: i64) -> Result<()> {
    let n: i64 = tx
        .query_one(
            "SELECT count(*) FROM traffic_rewrite_rules WHERE enabled AND id<>$1",
            &[&id],
        )
        .await?
        .get(0);
    if n >= MAX_ENABLED_TRAFFIC_REWRITE_RULES {
        return Err(Error::sentinel(
            ERR_TRAFFIC_REWRITE_ENABLED_LIMIT,
            ERR_TRAFFIC_REWRITE_ENABLED_LIMIT,
        ));
    }
    Ok(())
}
