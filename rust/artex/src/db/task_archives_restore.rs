//! Task archive asset restore — port of `db/task_archives_restore.go` slice.

use serde_json::{Map, Value as Json};

use crate::db::archive_helpers::{decode_archive_rows, json_int64, merge_json_task_id};
use crate::db::asset_helpers::normalize_url;
use crate::db::error::{Error, Result};
use crate::db::DB;

const ALLOWED_EXISTENCE_TABLES: &[&str] = &[
    "task_categories",
    "llm_profiles",
    "companies",
    "tasks",
    "assets",
];

fn str_value(row: &Map<String, Json>, key: &str) -> String {
    row.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

async fn row_exists(tx: &deadpool_postgres::Transaction<'_>, table: &str, id: i64) -> Result<bool> {
    if !ALLOWED_EXISTENCE_TABLES.contains(&table) {
        return Err(Error::msg(format!(
            "archive row existence table {table:?} is not allowed"
        )));
    }
    Ok(tx
        .query_opt(&format!("SELECT id FROM {table} WHERE id=$1"), &[&id])
        .await?
        .is_some())
}

/// `findArchiveAssetNaturalID`: look up an existing asset by its natural key
/// (unique index), using only typed fields.
async fn find_archive_asset_natural_id(
    tx: &deadpool_postgres::Transaction<'_>,
    row: &Map<String, Json>,
) -> Result<Option<i64>> {
    let typ = str_value(row, "type");
    let sql: &str;
    let mut vals: Vec<String> = Vec::new();
    let mut port_val: Option<i64> = None;
    match typ.as_str() {
        "root_domain" => {
            sql = "SELECT id FROM assets WHERE type='root_domain' AND domain=$1";
            vals.push(str_value(row, "domain"));
        }
        "ip" => {
            sql = "SELECT id FROM assets WHERE type='ip' AND ip=$1";
            vals.push(str_value(row, "ip"));
        }
        "subdomain" => {
            sql = "SELECT id FROM assets WHERE type='subdomain' AND domain=$1 AND COALESCE(record_type,'')=$2";
            vals.push(str_value(row, "domain"));
            vals.push(str_value(row, "record_type"));
        }
        "app" if !str_value(row, "bundle_id").is_empty() => {
            sql = "SELECT id FROM assets WHERE type='app' AND bundle_id=$1";
            vals.push(str_value(row, "bundle_id"));
        }
        "app" => {
            sql = "SELECT id FROM assets WHERE type='app' AND bundle_id IS NULL AND app_name=$1";
            vals.push(str_value(row, "app_name"));
        }
        "service" if str_value(row, "service_type") == "http" => {
            sql = "SELECT id FROM assets WHERE type='service' AND service_type='http' AND url=$1";
            vals.push(str_value(row, "url"));
        }
        "service" => {
            sql = "SELECT id FROM assets WHERE type='service' AND service_type='other' AND COALESCE(domain,'')=$1 AND COALESCE(ip,'')=$2 AND port=$3 AND service_name=$4";
            vals.push(str_value(row, "domain"));
            vals.push(str_value(row, "ip"));
            port_val = Some(json_int64(row.get("port").unwrap_or(&Json::Null)).unwrap_or(0));
            vals.push(str_value(row, "service_name"));
        }
        "endpoint" => {
            sql = "SELECT id FROM assets WHERE type='endpoint' AND url=$1 AND method=$2";
            vals.push(str_value(row, "url"));
            vals.push(str_value(row, "method"));
        }
        other => {
            return Err(Error::msg(format!(
                "unsupported archived asset type {other:?}"
            )))
        }
    }
    if let Some(port) = port_val {
        return Ok(tx
            .query_opt(sql, &[&vals[0], &vals[1], &port, &vals[3]])
            .await?
            .map(|r| r.get::<_, i64>(0)));
    }
    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vals
        .iter()
        .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    Ok(tx
        .query_opt(sql, &params)
        .await?
        .map(|r| r.get::<_, i64>(0)))
}

impl DB {
    /// `restoreArchiveAssets`: insert archived asset rows, reusing existing
    /// natural-key rows (and appending the task) instead of duplicating them.
    /// Archived URLs of service/endpoint rows are normalized, dropping any
    /// credential the old archive may have carried.
    pub async fn restore_archive_assets(
        &self,
        task_id: i64,
        raw: &Json,
    ) -> Result<(std::collections::HashMap<i64, i64>, Vec<String>)> {
        let rows = decode_archive_rows(raw);
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut mapping = std::collections::HashMap::new();
        let mut warnings = Vec::new();

        for mut row_value in rows {
            let Some(row) = row_value.as_object_mut() else {
                continue;
            };
            let old_id = json_int64(row.get("id").unwrap_or(&Json::Null))
                .ok_or_else(|| Error::msg("archived asset has invalid id"))?;
            if old_id <= 0 {
                return Err(Error::msg("archived asset has invalid id"));
            }

            if let Some(company_id) = row.get("company_id").and_then(json_int64) {
                if company_id > 0 && !row_exists(&tx, "companies", company_id).await? {
                    row.insert("company_id".to_string(), Json::Null);
                    row.insert(
                        "company_source".to_string(),
                        Json::String("explicit".into()),
                    );
                    warnings.push(format!("asset {old_id}'s company {company_id} has been deleted; restored as unaffiliated"));
                }
            }

            let typ = str_value(row, "type");
            if typ == "service" || typ == "endpoint" {
                if let Some(url) = row.get("url").and_then(|v| v.as_str()) {
                    row.insert("url".to_string(), Json::String(normalize_url(url)));
                }
            }

            if let Some(existing) = find_archive_asset_natural_id(&tx, row).await? {
                mapping.insert(old_id, existing);
                tx.execute(
                    "UPDATE assets SET task_ids=CASE WHEN $1=ANY(task_ids) THEN task_ids ELSE array_append(task_ids,$1) END WHERE id=$2",
                    &[&task_id, &existing],
                ).await?;
                continue;
            }

            let mut candidate = old_id;
            if row_exists(&tx, "assets", old_id).await? {
                let next: i64 = tx
                    .query_one("SELECT nextval(pg_get_serial_sequence('assets','id'))", &[])
                    .await?
                    .get(0);
                candidate = next;
                row.insert(
                    "id".to_string(),
                    Json::Number(serde_json::Number::from(candidate)),
                );
                warnings.push(format!(
                    "asset ID {old_id} is already taken; restored as {candidate}"
                ));
            }
            let tasks = merge_json_task_id(row.get("task_ids").unwrap_or(&Json::Null), task_id);
            row.insert(
                "task_ids".to_string(),
                serde_json::to_value(&tasks).map_err(|e| Error::msg(e.to_string()))?,
            );
            let asset_raw = serde_json::to_string(row).map_err(|e| Error::msg(e.to_string()))?;
            let n = tx.execute(
                "INSERT INTO assets SELECT * FROM json_populate_record(NULL::assets,$1::json) ON CONFLICT DO NOTHING",
                &[&asset_raw],
            ).await?;
            if n == 0 {
                let existing = find_archive_asset_natural_id(&tx, row)
                    .await?
                    .ok_or_else(|| Error::msg(format!("could not restore asset {old_id}")))?;
                candidate = existing;
                tx.execute(
                    "UPDATE assets SET task_ids=CASE WHEN $1=ANY(task_ids) THEN task_ids ELSE array_append(task_ids,$1) END WHERE id=$2",
                    &[&task_id, &existing],
                ).await?;
            }
            mapping.insert(old_id, candidate);
        }

        tx.commit().await?;
        Ok((mapping, warnings))
    }
}
