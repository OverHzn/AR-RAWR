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
    pub(crate) async fn restore_archive_assets_inner(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        task_id: i64,
        raw: &Json,
    ) -> Result<(std::collections::HashMap<i64, i64>, Vec<String>)> {
        let rows = decode_archive_rows(raw);
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
                if company_id > 0 && !row_exists(tx, "companies", company_id).await? {
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

            if let Some(existing) = find_archive_asset_natural_id(tx, row).await? {
                mapping.insert(old_id, existing);
                tx.execute(
                    "UPDATE assets SET task_ids=CASE WHEN $1=ANY(task_ids) THEN task_ids ELSE array_append(task_ids,$1) END WHERE id=$2",
                    &[&task_id, &existing],
                ).await?;
                continue;
            }

            let mut candidate = old_id;
            if row_exists(tx, "assets", old_id).await? {
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
                let existing = find_archive_asset_natural_id(tx, row)
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

        Ok((mapping, warnings))
    }

    /// Public wrapper: restore archived assets in its own transaction.
    pub async fn restore_archive_assets(
        &self,
        task_id: i64,
        raw: &Json,
    ) -> Result<(std::collections::HashMap<i64, i64>, Vec<String>)> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let out = self.restore_archive_assets_inner(&tx, task_id, raw).await?;
        tx.commit().await?;
        Ok(out)
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
    pub(crate) async fn insert_archive_rows_inner(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        table: &str,
        raw: &Json,
    ) -> Result<()> {
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
        tx.execute(
            &format!(
                "INSERT INTO {table} SELECT * FROM json_populate_recordset(NULL::{table},$1::json)"
            ),
            &[&body],
        )
        .await?;
        Ok(())
    }
}

impl DB {
    /// `insertArchiveRows`: insert rows of one allowlisted archive table with
    /// defaults restored for columns older archives predate.
    pub async fn insert_archive_rows(&self, table: &str, raw: &Json) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        self.insert_archive_rows_inner(&tx, table, raw).await?;
        tx.commit().await?;
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

// ---------------------------------------------------------------------------
// Full archive restore (db/task_archives_restore.go:restoreTaskArchive)
// ---------------------------------------------------------------------------

pub const TASK_ARCHIVE_FORMAT_VERSION: i32 = 3;
pub const TASK_ARCHIVE_LEGACY_FORMAT_VERSION: i32 = 1;
pub const TASK_ARCHIVE_LLM_RECORDS_PATH: &str = "database/llm_records.ndjson";
pub const RESTORING: &str = "restoring";
pub const ERR_TASK_ARCHIVE_FORMAT_MISMATCH: &str = "task archive format is not supported";
pub const ERR_TASK_ARCHIVE_STATE: &str = "task archive state does not allow this operation";

/// The manifest of one archive package.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct TaskArchiveSnapshot {
    pub format_version: i32,
    pub created_at: Option<String>,
    pub task_id: i64,
    pub exploration_id: i64,
    #[serde(default)]
    pub source_task_ids: Vec<i64>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub exclusive_hosts: Vec<String>,
    #[serde(default)]
    pub exclusive_asset_ids: Vec<i64>,
    #[serde(default)]
    pub tables: std::collections::BTreeMap<String, Json>,
    #[serde(default)]
    pub streamed_tables: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub data_counts: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub aggregate_stats: Json,
}

fn first_archive_row(raw: &Json) -> String {
    raw.as_array()
        .and_then(|a| a.first())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string())
}

async fn restore_exploration_stub(
    tx: &deadpool_postgres::Transaction<'_>,
    raw: &Json,
    exp_id: i64,
) -> Result<()> {
    let body = first_archive_row(raw);
    tx.execute(
        "UPDATE explorations current SET
 description=archived.description,goal=archived.goal,status=archived.status,
 created_at=archived.created_at,updated_at=archived.updated_at
FROM json_populate_record(NULL::explorations,$2::json) archived
WHERE current.id=$1 AND archived.id=$1",
        &[&exp_id, &body],
    )
    .await?;
    Ok(())
}

async fn restore_task_stub(
    tx: &deadpool_postgres::Transaction<'_>,
    row: &Json,
    task_id: i64,
    remaining: i64,
) -> Result<()> {
    let body = serde_json::to_string(row).map_err(|e| Error::msg(e.to_string()))?;
    tx.execute(
        "UPDATE tasks current SET
 name=archived.name,category_id=archived.category_id,description=archived.description,goal=archived.goal,
 status=archived.status,paused=archived.paused,queued=false,queued_at=NULL,queue_mode='',
 llm_profile_id=archived.llm_profile_id,active_llm_profile_id=archived.active_llm_profile_id,
 llm_chain_revision=archived.llm_chain_revision,company_id=archived.company_id,parent_ref=archived.parent_ref,
 timeout_seconds=archived.timeout_seconds,plan_heartbeat_seconds=archived.plan_heartbeat_seconds,
 coverage_enabled=archived.coverage_enabled,pinned_at=archived.pinned_at,first_run_at=archived.first_run_at,
 deadline_at=CASE WHEN archived.paused AND $3>0 THEN now()+make_interval(secs=>$3::double precision)
                  ELSE archived.deadline_at END,
 deleted_at=NULL,archived_at=NULL,completed_at=archived.completed_at,
 created_at=archived.created_at,updated_at=archived.updated_at
FROM json_populate_record(NULL::tasks,$2::json) archived
WHERE current.id=$1 AND archived.id=$1",
        &[&task_id, &body, &remaining],
    ).await?;
    Ok(())
}

async fn restore_finding_traffic_tx(
    db: &DB,
    tx: &deadpool_postgres::Transaction<'_>,
    snapshot: &TaskArchiveSnapshot,
) -> Result<()> {
    let raw = snapshot.tables.get("traffic_evidence_snapshots");
    let count = raw
        .map(crate::db::archive_helpers::raw_row_count)
        .unwrap_or(0);
    if count == 0 {
        return Ok(());
    }
    if snapshot.format_version < 3 {
        return Err(Error::sentinel(
            ERR_TASK_ARCHIVE_FORMAT_MISMATCH,
            ERR_TASK_ARCHIVE_FORMAT_MISMATCH,
        ));
    }
    let raw = raw.unwrap();
    let items: Vec<crate::db::finding_evidence::TrafficEvidenceSnapshot> =
        serde_json::from_value(raw.clone()).map_err(|e| Error::msg(e.to_string()))?;
    let mut allowed = std::collections::HashSet::new();
    for item in &items {
        if !allowed.insert(item.id.clone()) {
            return Err(Error::msg("duplicate archived evidence snapshot"));
        }
        db.insert_evidence_snapshot_tx(tx, item).await?;
    }
    let bindings_raw = snapshot
        .tables
        .get("finding_traffic_bindings")
        .cloned()
        .unwrap_or(Json::Null);
    let bindings = crate::db::archive_helpers::decode_archive_rows(&bindings_raw);
    if !bindings.is_empty() && snapshot.format_version < 3 {
        return Err(Error::sentinel(
            ERR_TASK_ARCHIVE_FORMAT_MISMATCH,
            ERR_TASK_ARCHIVE_FORMAT_MISMATCH,
        ));
    }
    for row in &bindings {
        let Some(obj) = row.as_object() else { continue };
        let finding = obj.get("finding_id").and_then(json_int64).unwrap_or(0);
        let sid = obj
            .get("snapshot_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if !allowed.contains(sid) || !crate::db::finding_evidence::valid_traffic_role(role) {
            return Err(Error::msg("invalid archived evidence binding"));
        }
        let owned: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM findings WHERE id=$1 AND task_id=$2)",
                &[&finding, &snapshot.task_id],
            )
            .await?
            .get(0);
        if !owned {
            return Err(Error::msg(format!(
                "evidence references finding outside archived task: {finding}"
            )));
        }
    }
    if !bindings.is_empty() {
        let body = serde_json::to_string(&bindings).map_err(|e| Error::msg(e.to_string()))?;
        tx.execute(
            "INSERT INTO finding_traffic_bindings SELECT * FROM json_populate_recordset(NULL::finding_traffic_bindings,$1::json)",
            &[&body],
        ).await?;
        tx.execute(
            "UPDATE traffic_evidence_snapshots s SET unreferenced_at=NULL WHERE EXISTS(SELECT 1 FROM finding_traffic_bindings b WHERE b.snapshot_id=s.id)",
            &[],
        ).await?;
    }
    Ok(())
}

impl DB {
    /// `RestoreTaskArchive`: restore a verified manifest into an archived task
    /// stub in one transaction. Streamed LLM records are not yet supported.
    pub async fn restore_task_archive(
        &self,
        archive_id: i64,
        snapshot: &TaskArchiveSnapshot,
        remaining_timeout_seconds: i64,
    ) -> Result<Vec<String>> {
        if snapshot.format_version < TASK_ARCHIVE_LEGACY_FORMAT_VERSION
            || snapshot.format_version > TASK_ARCHIVE_FORMAT_VERSION
        {
            return Err(Error::sentinel(
                ERR_TASK_ARCHIVE_FORMAT_MISMATCH,
                ERR_TASK_ARCHIVE_FORMAT_MISMATCH,
            ));
        }
        let streamed = snapshot
            .streamed_tables
            .get("llm_records")
            .map(|s| s.as_str())
            .unwrap_or("");
        if (!snapshot.streamed_tables.is_empty() && snapshot.format_version < 2)
            || snapshot.streamed_tables.len() > 1
            || (snapshot.streamed_tables.len() == 1 && streamed != TASK_ARCHIVE_LLM_RECORDS_PATH)
        {
            return Err(Error::msg(format!(
                "{ERR_TASK_ARCHIVE_FORMAT_MISMATCH}: unsupported streamed table metadata"
            )));
        }
        if !streamed.is_empty() {
            return Err(Error::msg(format!("{ERR_TASK_ARCHIVE_FORMAT_MISMATCH}: streamed LLM records are not supported by this restore path")));
        }

        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            &format!(
                "SELECT pg_advisory_xact_lock({})",
                crate::db::schema_migration_lock_key()
            ),
            &[],
        )
        .await?;
        let row = tx
            .query_opt(
                "SELECT archive.task_id,task.exploration_id,archive.state
FROM task_archives archive JOIN tasks task ON task.id=archive.task_id
WHERE archive.id=$1 FOR UPDATE OF archive,task",
                &[&archive_id],
            )
            .await?;
        let Some(row) = row else {
            return Err(Error::msg("task archive not found"));
        };
        let task_id: i64 = row.get(0);
        let exp_id: i64 = row.get(1);
        let state: String = row.get(2);
        if state != RESTORING || task_id != snapshot.task_id || exp_id != snapshot.exploration_id {
            return Err(Error::sentinel(
                ERR_TASK_ARCHIVE_STATE,
                "restore snapshot identity/state mismatch",
            ));
        }

        let mut warnings = Vec::new();
        let assets_raw = snapshot.tables.get("assets").cloned().unwrap_or(Json::Null);
        let (asset_map, asset_warnings) = self
            .restore_archive_assets_inner(&tx, task_id, &assets_raw)
            .await?;
        warnings.extend(asset_warnings);

        let task_rows = crate::db::archive_helpers::decode_archive_rows(
            snapshot.tables.get("tasks").unwrap_or(&Json::Null),
        );
        if task_rows.len() != 1 {
            return Err(Error::msg("restore task row: expected one row"));
        }
        let mut task_row = task_rows[0].clone();
        if let Some(obj) = task_row.as_object_mut() {
            for key in [
                "category_id",
                "company_id",
                "llm_profile_id",
                "active_llm_profile_id",
            ] {
                let id = obj.get(key).and_then(json_int64).unwrap_or(0);
                let table = match key {
                    "category_id" => "task_categories",
                    "company_id" => "companies",
                    _ => "llm_profiles",
                };
                if id > 0 && !row_exists(&tx, table, id).await? {
                    obj.insert(key.to_string(), Json::Null);
                    warnings.push(format!("{key} {id} has been deleted; reference removed"));
                }
            }
        }
        restore_exploration_stub(
            &tx,
            snapshot.tables.get("explorations").unwrap_or(&Json::Null),
            exp_id,
        )
        .await?;
        restore_task_stub(&tx, &task_row, task_id, remaining_timeout_seconds).await?;

        let remapped = crate::db::archive_helpers::remap_archive_asset_references(
            &snapshot.tables,
            &asset_map,
        );
        let mut remapped = remapped;
        if let Some(findings) = remapped.get_mut("findings") {
            let filled = crate::db::archive_helpers::fill_archive_defaults("findings", findings)
                .map_err(Error::msg)?;
            *findings = filled;
        }
        for table in [
            "exploration_nodes",
            "exploration_edges",
            "exploration_anchors",
            "task_constraints",
            "activity",
        ] {
            let raw = remapped.get(table).cloned().unwrap_or(Json::Null);
            self.insert_archive_rows_inner(&tx, table, &raw).await?;
        }
        let relations = remapped
            .get("task_relations")
            .cloned()
            .unwrap_or(Json::Null);
        warnings.extend(restore_task_relations_tx(&tx, task_id, &relations).await?);
        for table in ["side_question_sessions", "side_question_requests"] {
            let raw = remapped.get(table).cloned().unwrap_or(Json::Null);
            self.insert_archive_rows_inner(&tx, table, &raw).await?;
        }
        let scopes = remapped.get("task_scope").cloned().unwrap_or(Json::Null);
        warnings.extend(restore_task_scopes_tx(&tx, &scopes).await?);
        let profiles = remapped
            .get("task_llm_profiles")
            .cloned()
            .unwrap_or(Json::Null);
        warnings.extend(restore_task_llm_profiles_tx(&tx, &profiles).await?);
        for table in ["task_asset_links", "findings"] {
            let raw = remapped.get(table).cloned().unwrap_or(Json::Null);
            self.insert_archive_rows_inner(&tx, table, &raw).await?;
        }
        restore_finding_traffic_tx(self, &tx, snapshot).await?;
        for table in ["llm_records", "llm_usage", "skill_usage", "tool_usage"] {
            let raw = remapped.get(table).cloned().unwrap_or(Json::Null);
            self.insert_archive_rows_inner(&tx, table, &raw).await?;
        }
        let intercept_raw = remapped
            .get("intercept_pending")
            .cloned()
            .unwrap_or(Json::Null);
        let rows = restore_intercept_rows_tx(&crate::db::archive_helpers::decode_archive_rows(
            &intercept_raw,
        ))?;
        if !rows.is_empty() {
            let body = serde_json::to_string(&rows).map_err(|e| Error::msg(e.to_string()))?;
            tx.execute(
                "INSERT INTO intercept_pending SELECT * FROM json_populate_recordset(NULL::intercept_pending,$1::json)",
                &[&body],
            ).await?;
        }

        let warnings_raw =
            serde_json::to_string(&warnings).map_err(|e| Error::msg(e.to_string()))?;
        tx.execute(
            "UPDATE task_archives SET warnings=$2,phase='database_restored',progress=85,error='' WHERE id=$1",
            &[&archive_id, &warnings_raw],
        ).await?;
        tx.commit().await?;
        Ok(warnings)
    }
}
