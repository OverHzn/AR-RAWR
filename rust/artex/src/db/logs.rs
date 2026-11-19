//! Persisted server logs — port of `db/logs.go`.
//!
//! `server_logs` is the lightweight backend log sink (separate from the structured
//! logs pipeline). Rows are append-only; the readers page newest-first internally
//! then reverse to oldest-first, matching Go.

use crate::db::error::Result;
use crate::db::DB;
use std::time::SystemTime;

/// `DBLog` — one persisted backend log row.
#[derive(Debug, Clone)]
pub struct DBLog {
    pub id: i64,
    pub created_at: SystemTime,
    pub level: String,
    pub tag: String,
    pub text: String,
}

impl DB {
    /// `InsertLog`: append one log line, returning its auto-assigned id.
    pub async fn insert_log(&self, level: &str, tag: &str, text: &str) -> Result<i64> {
        let row = self
            .query_one(
                "INSERT INTO server_logs(level,tag,text) VALUES($1,$2,$3) RETURNING id",
                &[&level, &tag, &text],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `RecentLogs`: the most recent `limit` rows, oldest-first.
    pub async fn recent_logs(&self, limit: i64) -> Result<Vec<DBLog>> {
        let rows = self
            .query(
                "SELECT id, created_at, level, tag, text \
             FROM server_logs \
             ORDER BY id DESC \
             LIMIT $1",
                &[&limit],
            )
            .await?;
        Ok(reversed(rows))
    }

    /// `ListLogsBefore`: up to `limit` rows with `id < before_id`, oldest-first.
    pub async fn list_logs_before(&self, before_id: i64, limit: i64) -> Result<Vec<DBLog>> {
        let rows = self
            .query(
                "SELECT id, created_at, level, tag, text \
             FROM server_logs \
             WHERE id < $1 \
             ORDER BY id DESC \
             LIMIT $2",
                &[&before_id, &limit],
            )
            .await?;
        Ok(reversed(rows))
    }
}

/// Scan newest-first rows and reverse to oldest-first (Go reverses the slice).
fn reversed(rows: Vec<tokio_postgres::Row>) -> Vec<DBLog> {
    let mut out: Vec<DBLog> = rows
        .iter()
        .map(|r| DBLog {
            id: r.get(0),
            created_at: r.get(1),
            level: r.get(2),
            tag: r.get(3),
            text: r.get(4),
        })
        .collect();
    out.reverse();
    out
}
