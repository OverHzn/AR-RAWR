//! Task archive record reads + blockers — slice of `db/task_archives.go`.
//!
//! A task with a live, non-queued dependent cannot be archived (the FIFO worker
//! compacts dependents first). This slice ports the record model, format gate and
//! read/blocker queries; snapshot/restore land separately.

use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::DB;

pub const TASK_ARCHIVE_FORMAT_VERSION: i32 = 3;
pub const TASK_ARCHIVE_LEGACY_FORMAT_VERSION: i32 = 1;
pub const TASK_ARCHIVE_LLM_RECORDS_PATH: &str = "database/llm_records.ndjson";

pub const ARCHIVE_QUEUED: &str = "archive_queued";
pub const ARCHIVING: &str = "archiving";
pub const ARCHIVE_FAILED: &str = "archive_failed";
pub const ARCHIVE_READY: &str = "ready";
pub const RESTORE_QUEUED: &str = "restore_queued";
pub const RESTORING: &str = "restoring";
pub const RESTORE_FAILED: &str = "restore_failed";
pub const DELETE_QUEUED: &str = "delete_queued";
pub const DELETING: &str = "deleting";
pub const DELETE_FAILED: &str = "delete_failed";

pub const ERR_TASK_ARCHIVE_NOT_FOUND: &str = "task archive not found";
pub const ERR_TASK_ARCHIVE_FORMAT_MISMATCH: &str = "task archive format is not supported";

pub fn is_task_archive_format_supported(version: i32) -> bool {
    (TASK_ARCHIVE_LEGACY_FORMAT_VERSION..=TASK_ARCHIVE_FORMAT_VERSION).contains(&version)
}

/// `TaskArchive`: the compact record retained while a task is cold (no secrets).
#[derive(Debug, Clone, PartialEq)]
pub struct TaskArchive {
    pub id: i64,
    pub task_id: i64,
    pub state: String,
    pub phase: String,
    pub progress: i32,
    pub error: String,
    pub warnings: serde_json::Value,
    pub format_version: i32,
    pub archive_path: String,
    pub sha256: String,
    pub original_size: i64,
    pub compressed_size: i64,
    pub task_name: String,
    pub task_description: String,
    pub task_goal: String,
    pub original_status: String,
    pub category_id_snapshot: Option<i64>,
    pub category_name_snapshot: String,
    pub source_task_ids: Vec<i64>,
    pub remaining_timeout_seconds: i64,
    pub data_counts: serde_json::Value,
    pub aggregate_stats: serde_json::Value,
    pub archived_at: Option<SystemTime>,
    pub requested_at: SystemTime,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl Default for TaskArchive {
    fn default() -> Self {
        TaskArchive {
            id: 0,
            task_id: 0,
            state: String::new(),
            phase: String::new(),
            progress: 0,
            error: String::new(),
            warnings: serde_json::json!([]),
            format_version: 0,
            archive_path: String::new(),
            sha256: String::new(),
            original_size: 0,
            compressed_size: 0,
            task_name: String::new(),
            task_description: String::new(),
            task_goal: String::new(),
            original_status: String::new(),
            category_id_snapshot: None,
            category_name_snapshot: String::new(),
            source_task_ids: Vec::new(),
            remaining_timeout_seconds: 0,
            data_counts: serde_json::json!({}),
            aggregate_stats: serde_json::json!({}),
            archived_at: None,
            requested_at: SystemTime::UNIX_EPOCH,
            created_at: SystemTime::UNIX_EPOCH,
            updated_at: SystemTime::UNIX_EPOCH,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskArchivePage {
    pub items: Vec<TaskArchive>,
    pub total: i64,
    pub page: i32,
    pub size: i32,
}

const TASK_ARCHIVE_COLS: &str = "id, task_id, state, phase, progress, COALESCE(error,''), warnings, format_version, COALESCE(archive_path,''), COALESCE(sha256,''), original_size, compressed_size, COALESCE(task_name,''), COALESCE(task_description,''), COALESCE(task_goal,''), COALESCE(original_status,''), category_id_snapshot, COALESCE(category_name_snapshot,''), array_to_json(source_task_ids)::text, remaining_timeout_seconds, data_counts, aggregate_stats, archived_at, requested_at, created_at, updated_at";

const ARCHIVE_BLOCKER_SCOPE: &str = "FROM task_relations relation \
     JOIN tasks child ON child.id=relation.task_id AND child.deleted_at IS NULL \
     LEFT JOIN task_archives pending ON pending.task_id=child.id \
     WHERE (pending.id IS NULL OR pending.state NOT IN ('archive_queued','archiving'))";

fn scan_archive(row: &tokio_postgres::Row) -> Result<TaskArchive> {
    let sources: String = row.get(18);
    let mut a = TaskArchive {
        id: row.get(0),
        task_id: row.get(1),
        state: row.get(2),
        phase: row.get(3),
        progress: row.get(4),
        error: row.get(5),
        warnings: row.get(6),
        format_version: row.get(7),
        archive_path: row.get(8),
        sha256: row.get(9),
        original_size: row.get(10),
        compressed_size: row.get(11),
        task_name: row.get(12),
        task_description: row.get(13),
        task_goal: row.get(14),
        original_status: row.get(15),
        category_id_snapshot: row.get(16),
        category_name_snapshot: row.get(17),
        source_task_ids: serde_json::from_str(&sources).unwrap_or_default(),
        remaining_timeout_seconds: row.get(19),
        data_counts: row.get(20),
        aggregate_stats: row.get(21),
        archived_at: row.get(22),
        requested_at: row.get(23),
        created_at: row.get(24),
        updated_at: row.get(25),
    };
    if a.warnings.is_null() {
        a.warnings = serde_json::json!([]);
    }
    if a.data_counts.is_null() {
        a.data_counts = serde_json::json!({});
    }
    if a.aggregate_stats.is_null() {
        a.aggregate_stats = serde_json::json!({});
    }
    Ok(a)
}

impl DB {
    /// `TaskArchiveBlockers`: source task id -> one live non-queued dependent id.
    pub async fn task_archive_blockers(&self) -> Result<std::collections::HashMap<i64, i64>> {
        let rows = self.query(
            &format!("SELECT relation.source_task_id, MIN(child.id){ARCHIVE_BLOCKER_SCOPE} GROUP BY relation.source_task_id"),
            &[],
        ).await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// `TaskArchiveBlocker`: one task's blocker id (0 when none).
    pub async fn task_archive_blocker(&self, task_id: i64) -> Result<i64> {
        let row = self
            .query_opt(
                &format!(
                    "SELECT MIN(child.id){ARCHIVE_BLOCKER_SCOPE} AND relation.source_task_id=$1"
                ),
                &[&task_id],
            )
            .await?;
        Ok(row.and_then(|r| r.get::<_, Option<i64>>(0)).unwrap_or(0))
    }

    pub async fn get_task_archive(&self, id: i64) -> Result<Option<TaskArchive>> {
        let row = self
            .query_opt(
                &format!("SELECT {TASK_ARCHIVE_COLS} FROM task_archives WHERE id=$1"),
                &[&id],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(scan_archive(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn get_task_archive_by_task(&self, task_id: i64) -> Result<Option<TaskArchive>> {
        let row = self
            .query_opt(
                &format!("SELECT {TASK_ARCHIVE_COLS} FROM task_archives WHERE task_id=$1"),
                &[&task_id],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(scan_archive(&r)?)),
            None => Ok(None),
        }
    }

    /// `ListTaskArchives`: paginated archive list, newest requested first.
    pub async fn list_task_archives(
        &self,
        search: &str,
        state: &str,
        page: i32,
        size: i32,
    ) -> Result<TaskArchivePage> {
        let page = if page < 1 { 1 } else { page };
        let size = if size <= 0 || size > 100 { 20 } else { size };
        let search = search.trim();
        let state = state.trim();
        let where_sql = "WHERE ($1='' OR task_id::text ILIKE '%'||$1||'%' OR task_name ILIKE '%'||$1||'%' OR task_description ILIKE '%'||$1||'%') AND ($2='' OR state=$2)";
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&search, &state];
        let total: i64 = self
            .query_one(
                &format!("SELECT count(*) FROM task_archives {where_sql}"),
                &params,
            )
            .await?
            .get(0);
        let limit = size as i64;
        let offset = (page - 1) as i64 * size as i64;
        let mut refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.to_vec();
        refs.push(&limit);
        refs.push(&offset);
        let rows = self.query(
            &format!("SELECT {TASK_ARCHIVE_COLS} FROM task_archives {where_sql} \
                      ORDER BY COALESCE(archived_at, requested_at) DESC, id DESC LIMIT $3 OFFSET $4"),
            &refs,
        ).await?;
        let mut items = Vec::with_capacity(rows.len());
        for r in &rows {
            items.push(scan_archive(r)?);
        }
        Ok(TaskArchivePage {
            items,
            total,
            page,
            size,
        })
    }
}
