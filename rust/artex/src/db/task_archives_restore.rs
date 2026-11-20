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

const INSERTABLE_ARCHIVE_TABLES: &[&str] = &[
    "exploration_nodes",
    "exploration_edges",
    "exploration_anchors",
    "task_constraints",
    "activity",
    "task_asset_links",
    "findings",
    "llm_records",
    "llm_usage",
    "skill_usage",
    "tool_usage",
    "side_question_sessions",
    "side_question_requests",
];

impl DB {
    /// `insertArchiveRows`: insert rows of one allowlisted archive table with
    /// defaults restored for columns older archives predate.
    pub async fn insert_archive_rows(&self, table: &str, raw: &Json) -> Result<()> {
        if !INSERTABLE_ARCHIVE_TABLES.contains(&table) {
            return Err(Error::msg(format!(
                "archive restore table {table:?} is not allowed"
            )));
        }
        let filled =
            crate::db::archive_helpers::fill_archive_defaults(table, raw).map_err(Error::msg)?;
        if crate::db::archive_helpers::raw_row_count(&filled) == 0 {
            return Ok(());
        }
        let body = serde_json::to_string(&filled).map_err(|e| Error::msg(e.to_string()))?;
        self.execute(
            &format!(
                "INSERT INTO {table} SELECT * FROM json_populate_recordset(NULL::{table},$1::json)"
            ),
            &[&body],
        )
        .await?;
        Ok(())
    }
}

// Remaining restore helpers (task relations / scopes / LLM profiles / intercept rows).

fn is_model_reason(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    lower.starts_with(crate::db::intercept_store::MODEL_REASON_PREFIX)
        || reason.starts_with("[模型]")
}

async fn live_task_exists(tx: &deadpool_postgres::Transaction<'_>, id: i64) -> bool {
    tx.query_opt(
        "SELECT 1 FROM tasks WHERE id=$1 AND deleted_at IS NULL",
        &[&id],
    )
    .await
    .map(|r| r.is_some())
    .unwrap_or(false)
}

async fn restore_task_relations_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    task_id: i64,
    raw: &Json,
) -> Result<Vec<String>> {
    let rows = decode_archive_rows(raw);
    let mut warnings = Vec::new();
    for row in &rows {
        let Some(obj) = row.as_object() else { continue };
        let source = obj.get("source_task_id").and_then(json_int64).unwrap_or(0);
        if !live_task_exists(tx, source).await {
            warnings.push(format!(
                "source task {source} is unavailable; inheritance skipped"
            ));
            continue;
        }
        let created = obj.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let created = crate::db::archive_helpers::nil_if_empty_string(created);
        tx.execute(
            "INSERT INTO task_relations(task_id,source_task_id,created_at) VALUES($1,$2,COALESCE($3::timestamptz,now())) ON CONFLICT DO NOTHING",
            &[&task_id, &source, &created],
        ).await?;
    }
    Ok(warnings)
}

async fn restore_task_scopes_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    raw: &Json,
) -> Result<Vec<String>> {
    let rows = decode_archive_rows(raw);
    let mut warnings = Vec::new();
    let mut kept = Vec::new();
    for row in rows {
        let cid = row
            .as_object()
            .and_then(|o| o.get("company_id").and_then(json_int64))
            .unwrap_or(0);
        if cid > 0 && !row_exists(tx, "companies", cid).await? {
            warnings.push(format!(
                "company {cid} has been deleted; associated scope skipped"
            ));
            continue;
        }
        kept.push(row);
    }
    if kept.is_empty() {
        return Ok(warnings);
    }
    let body = serde_json::to_string(&kept).map_err(|e| Error::msg(e.to_string()))?;
    tx.execute(
        "INSERT INTO task_scope SELECT * FROM json_populate_recordset(NULL::task_scope,$1::json) ON CONFLICT DO NOTHING",
        &[&body],
    ).await?;
    Ok(warnings)
}

async fn restore_task_llm_profiles_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    raw: &Json,
) -> Result<Vec<String>> {
    let rows = decode_archive_rows(raw);
    let mut warnings = Vec::new();
    let mut position = 0i32;
    for row in &rows {
        let Some(obj) = row.as_object() else { continue };
        let profile = obj.get("profile_id").and_then(json_int64).unwrap_or(0);
        if !row_exists(tx, "llm_profiles", profile).await? {
            warnings.push(format!(
                "LLM profile {profile} has been deleted; chain entry skipped"
            ));
            continue;
        }
        let task = obj.get("task_id").and_then(json_int64).unwrap_or(0);
        let get = |k: &str| obj.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let status = get("status");
        let last_error = get("last_error");
        let exhausted = crate::db::archive_helpers::nil_if_empty_string(get("exhausted_at"));
        let created = crate::db::archive_helpers::nil_if_empty_string(get("created_at"));
        let updated = crate::db::archive_helpers::nil_if_empty_string(get("updated_at"));
        tx.execute(
            "INSERT INTO task_llm_profiles(task_id,profile_id,position,status,last_error,exhausted_at,created_at,updated_at)
VALUES($1,$2,$3,$4,NULLIF($5,''),$6::timestamptz,COALESCE($7::timestamptz,now()),COALESCE($8::timestamptz,now()))
ON CONFLICT DO NOTHING",
            &[&task, &profile, &position, &status, &last_error, &exhausted, &created, &updated],
        ).await?;
        position += 1;
    }
    Ok(warnings)
}

fn restore_intercept_rows_tx(rows_in: &[Json]) -> Result<Vec<Json>> {
    let mut rows = rows_in.to_vec();
    for row in &mut rows {
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        let source = obj
            .get("decision_source")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if source.is_empty() {
            let inferred = if obj.get("rule_id").is_some() && !obj.get("rule_id").unwrap().is_null()
            {
                "rule"
            } else if obj
                .get("reason")
                .and_then(|v| v.as_str())
                .map(is_model_reason)
                .unwrap_or(false)
            {
                "model"
            } else {
                "unknown"
            };
            obj.insert("decision_source".to_string(), Json::String(inferred.into()));
        }
        if obj.get("decided_by").map(|v| v.is_null()).unwrap_or(true) {
            obj.insert("decided_by".to_string(), Json::String(String::new()));
        }
        if obj.get("status").and_then(|v| v.as_str()) == Some("pending") {
            obj.insert("status".to_string(), Json::String("timeout".into()));
            obj.insert("decided_by".to_string(), Json::String("system".into()));
            obj.insert(
                "reason".to_string(),
                Json::String("approval timed out while the task was archived".into()),
            );
            obj.insert(
                "decided_at".to_string(),
                Json::String(crate::db::gotime::format_rfc3339(
                    std::time::SystemTime::now(),
                )),
            );
            if let Some(audit) = obj.get_mut("audit").and_then(|v| v.as_object_mut()) {
                audit.insert("effective_action".into(), Json::String("deny".into()));
                audit.insert(
                    "decision_reason".into(),
                    Json::String("approval timed out while the task was archived".into()),
                );
                audit.insert(
                    "execution_status".into(),
                    Json::String("not_executed".into()),
                );
            }
        } else if let Some(audit) = obj.get_mut("audit").and_then(|v| v.as_object_mut()) {
            if audit.get("execution_status").and_then(|v| v.as_str()) == Some("awaiting_result") {
                audit.insert("execution_status".into(), Json::String("unknown".into()));
            }
        }
    }
    Ok(rows)
}

impl DB {
    /// `RestoreTaskRelations`: restore inheritance rows, warning for unavailable sources.
    pub async fn restore_task_relations(&self, task_id: i64, raw: &Json) -> Result<Vec<String>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let out = restore_task_relations_tx(&tx, task_id, raw).await?;
        tx.commit().await?;
        Ok(out)
    }

    pub async fn restore_task_scopes(&self, raw: &Json) -> Result<Vec<String>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let out = restore_task_scopes_tx(&tx, raw).await?;
        tx.commit().await?;
        Ok(out)
    }

    pub async fn restore_task_llm_profiles(&self, raw: &Json) -> Result<Vec<String>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let out = restore_task_llm_profiles_tx(&tx, raw).await?;
        tx.commit().await?;
        Ok(out)
    }

    /// `RestoreInterceptRows`: normalize legacy target-gate fields and settle
    /// pending rows as timed out, then insert the batch.
    pub async fn restore_intercept_rows(&self, raw: &Json) -> Result<()> {
        let rows = restore_intercept_rows_tx(&decode_archive_rows(raw))?;
        if rows.is_empty() {
            return Ok(());
        }
        let body = serde_json::to_string(&rows).map_err(|e| Error::msg(e.to_string()))?;
        self.execute(
            "INSERT INTO intercept_pending SELECT * FROM json_populate_recordset(NULL::intercept_pending,$1::json)",
            &[&body],
        ).await?;
        Ok(())
    }
}
