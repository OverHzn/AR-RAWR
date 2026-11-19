//! Global asset intercept rules — port of `db/asset_intercept.go`.
//!
//! `AssetInterceptRule` is one row of `asset_intercept_rules` — a global asset
//! blocklist entry. Unlike `intercept_rules` (which match tool name/input), these
//! match the *target asset* (exact/fuzzy domain·ip·url, or a CIDR range). This
//! layer only stores rules; the matching/enforcement logic lives elsewhere.
//!
//! Global rules have no `action` column — the field is always empty and treated as
//! intercept (only task-level rules use block/allow).

use crate::db::error::Result;
use crate::db::DB;
use std::time::SystemTime;

/// `AssetInterceptRule` — one global asset blocklist entry.
#[derive(Debug, Clone)]
pub struct AssetInterceptRule {
    pub id: i64,
    pub enabled: bool,
    /// exact_domain|exact_ip|exact_url|fuzzy_domain|fuzzy_ip|fuzzy_url|cidr
    pub kind: String,
    pub pattern: String,
    pub note: String,
    pub builtin: bool,
    /// Only set by task-level rules; always empty for global rules.
    pub action: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl Default for AssetInterceptRule {
    fn default() -> Self {
        AssetInterceptRule {
            id: 0,
            enabled: false,
            kind: String::new(),
            pattern: String::new(),
            note: String::new(),
            builtin: false,
            action: String::new(),
            created_at: SystemTime::UNIX_EPOCH,
            updated_at: SystemTime::UNIX_EPOCH,
        }
    }
}

/// `assetInterceptRuleCols`: the shared read column list.
const ASSET_INTERCEPT_RULE_COLS: &str =
    "id, enabled, kind, pattern, note, builtin, created_at, updated_at";

/// `scanAssetInterceptRule`: scan a row positioned on [`ASSET_INTERCEPT_RULE_COLS`].
fn scan_asset_intercept_rule(row: &tokio_postgres::Row) -> AssetInterceptRule {
    AssetInterceptRule {
        id: row.get(0),
        enabled: row.get(1),
        kind: row.get(2),
        pattern: row.get(3),
        note: row.get(4),
        builtin: row.get(5),
        action: String::new(),
        created_at: row.get(6),
        updated_at: row.get(7),
    }
}

impl DB {
    /// `ListAssetInterceptRules`: all rules, built-ins first then newest first.
    pub async fn list_asset_intercept_rules(&self) -> Result<Vec<AssetInterceptRule>> {
        let sql = format!(
            "SELECT {ASSET_INTERCEPT_RULE_COLS} FROM asset_intercept_rules ORDER BY builtin DESC, id DESC"
        );
        let rows = self.query(&sql, &[]).await?;
        Ok(rows.iter().map(scan_asset_intercept_rule).collect())
    }

    /// `CreateAssetInterceptRule`: insert a new user rule (builtin=false).
    pub async fn create_asset_intercept_rule(
        &self,
        kind: &str,
        pattern: &str,
        note: &str,
        enabled: bool,
    ) -> Result<AssetInterceptRule> {
        let sql = format!(
            "INSERT INTO asset_intercept_rules(enabled, kind, pattern, note, builtin) \
             VALUES ($1, $2, $3, $4, false) RETURNING {ASSET_INTERCEPT_RULE_COLS}"
        );
        let row = self
            .query_one(&sql, &[&enabled, &kind, &pattern, &note])
            .await?;
        Ok(scan_asset_intercept_rule(&row))
    }

    /// `UpdateAssetInterceptRule`: replace the editable fields. Errors if no row
    /// (Go's `QueryRow` → `ErrNoRows`).
    pub async fn update_asset_intercept_rule(
        &self,
        id: i64,
        kind: &str,
        pattern: &str,
        note: &str,
        enabled: bool,
    ) -> Result<AssetInterceptRule> {
        let sql = format!(
            "UPDATE asset_intercept_rules \
             SET enabled=$2, kind=$3, pattern=$4, note=$5 \
             WHERE id=$1 RETURNING {ASSET_INTERCEPT_RULE_COLS}"
        );
        let row = self
            .query_one(&sql, &[&id, &enabled, &kind, &pattern, &note])
            .await?;
        Ok(scan_asset_intercept_rule(&row))
    }

    /// `DeleteAssetInterceptRule`: remove a rule (built-ins are deletable too).
    pub async fn delete_asset_intercept_rule(&self, id: i64) -> Result<()> {
        self.execute("DELETE FROM asset_intercept_rules WHERE id=$1", &[&id])
            .await?;
        Ok(())
    }

    /// `ToggleAssetInterceptRule`: flip the enabled state.
    pub async fn toggle_asset_intercept_rule(&self, id: i64, enabled: bool) -> Result<()> {
        self.execute(
            "UPDATE asset_intercept_rules SET enabled=$2 WHERE id=$1",
            &[&id, &enabled],
        )
        .await?;
        Ok(())
    }
}
