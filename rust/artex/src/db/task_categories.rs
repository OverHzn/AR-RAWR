//! Task categories — port of `db/task_categories.go`.
//!
//! A `TaskCategory` is a globally reusable task-grouping label. The CRUD is
//! self-contained; `TaskCount` is computed live by a `LEFT JOIN tasks … GROUP BY`
//! (only non-deleted tasks count). `SetTasksCategory` moves a batch inside one
//! transaction so a half-applied move is never observable.
//!
//! Task assignment/clearing uses the `tasks.category_id` FK (`ON DELETE SET NULL`
//! clears tasks when their category is deleted). The tasks themselves are ported
//! with the tasks group; here we only own the category rows and the UPDATEs.

use crate::db::error::{Error, Result};
use crate::db::DB;
use gocompat::strings::{go_fields_join, go_to_lower};
use std::time::SystemTime;

/// `MaxTaskCategoryNameRunes` — inclusive cap on the normalized name.
pub const MAX_TASK_CATEGORY_NAME_RUNES: usize = 80;

/// `MaxTaskCategoryBatchSize` — bound on one batch move.
pub const MAX_TASK_CATEGORY_BATCH_SIZE: usize = 100;

/// `ErrTaskCategoryInvalid`: "invalid task category".
pub const ERR_TASK_CATEGORY_INVALID: &str = "invalid task category";
/// `ErrTaskCategoryNameConflict`: "task category name already exists".
pub const ERR_TASK_CATEGORY_NAME_CONFLICT: &str = "task category name already exists";
/// `ErrTaskCategoryNotFound`: "task category not found".
pub const ERR_TASK_CATEGORY_NOT_FOUND: &str = "task category not found";
/// `ErrTaskCategoryTaskNotFound`: "task not found".
pub const ERR_TASK_CATEGORY_TASK_NOT_FOUND: &str = "task not found";

/// `TaskCategory` — a globally reusable task grouping label.
#[derive(Debug, Clone)]
pub struct TaskCategory {
    pub id: i64,
    pub name: String,
    pub nkey: String,
    pub task_count: i64,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

/// `taskCategoryCols`: the shared read column list (with the live task count).
const TASK_CATEGORY_COLS: &str = "category.id, category.name, category.nkey, \
    count(task.id) FILTER (WHERE task.deleted_at IS NULL), \
    category.created_at, category.updated_at";

/// `scanTaskCategory`: scan a row on [`TASK_CATEGORY_COLS`].
fn scan_task_category(row: &tokio_postgres::Row) -> TaskCategory {
    TaskCategory {
        id: row.get(0),
        name: row.get(1),
        nkey: row.get(2),
        task_count: row.get(3),
        created_at: row.get(4),
        updated_at: row.get(5),
    }
}

/// `normalizeTaskCategoryName`: collapse whitespace, validate, and lowercase the nkey.
/// Returns `(name, nkey)`.
fn normalize_task_category_name(name: &str) -> Result<(String, String)> {
    let name = go_fields_join(name);
    if name.is_empty() {
        return Err(Error::sentinel(
            ERR_TASK_CATEGORY_INVALID,
            format!("{ERR_TASK_CATEGORY_INVALID}: name is required"),
        ));
    }
    if name.chars().count() > MAX_TASK_CATEGORY_NAME_RUNES {
        return Err(Error::sentinel(
            ERR_TASK_CATEGORY_INVALID,
            format!("{ERR_TASK_CATEGORY_INVALID}: name exceeds {MAX_TASK_CATEGORY_NAME_RUNES} characters"),
        ));
    }
    let nkey = go_to_lower(&name);
    Ok((name, nkey))
}

impl DB {
    /// `CreateTaskCategory`: insert a category; a name conflict (same nkey) yields
    /// [`ERR_TASK_CATEGORY_NAME_CONFLICT`].
    pub async fn create_task_category(&self, name: &str) -> Result<TaskCategory> {
        let (name, nkey) = normalize_task_category_name(name)?;
        let row = self.query_opt(
            "WITH inserted AS (\
                INSERT INTO task_categories(name, nkey) VALUES ($1,$2) \
                ON CONFLICT (nkey) DO NOTHING RETURNING *\
             ) SELECT inserted.id, inserted.name, inserted.nkey, 0::bigint, inserted.created_at, inserted.updated_at \
             FROM inserted",
            &[&name, &nkey],
        ).await?;
        match row {
            Some(row) => Ok(TaskCategory {
                id: row.get(0),
                name: row.get(1),
                nkey: row.get(2),
                task_count: row.get(3),
                created_at: row.get(4),
                updated_at: row.get(5),
            }),
            None => Err(Error::sentinel(
                ERR_TASK_CATEGORY_NAME_CONFLICT,
                ERR_TASK_CATEGORY_NAME_CONFLICT,
            )),
        }
    }

    /// `ListTaskCategories`: all categories by (name, id), with the live task count.
    pub async fn list_task_categories(&self) -> Result<Vec<TaskCategory>> {
        let sql = format!(
            "SELECT {TASK_CATEGORY_COLS} FROM task_categories category \
             LEFT JOIN tasks task ON task.category_id=category.id \
             GROUP BY category.id ORDER BY category.name, category.id"
        );
        let rows = self.query(&sql, &[]).await?;
        Ok(rows.iter().map(scan_task_category).collect())
    }

    /// `GetTaskCategory`: one category, or `None` if absent.
    pub async fn get_task_category(&self, id: i64) -> Result<Option<TaskCategory>> {
        let sql = format!(
            "SELECT {TASK_CATEGORY_COLS} FROM task_categories category \
             LEFT JOIN tasks task ON task.category_id=category.id \
             WHERE category.id=$1 GROUP BY category.id"
        );
        let row = self.query_opt(&sql, &[&id]).await?;
        Ok(row.as_ref().map(scan_task_category))
    }

    /// `RenameTaskCategory`: rename (and rekey). Name conflict -> NameConflict;
    /// missing id -> NotFound.
    pub async fn rename_task_category(&self, id: i64, name: &str) -> Result<TaskCategory> {
        let (name, nkey) = normalize_task_category_name(name)?;
        let res = self
            .query_opt(
                "WITH updated AS (\
                    UPDATE task_categories SET name=$2, nkey=$3 WHERE id=$1 RETURNING *\
                 ) SELECT updated.id, updated.name, updated.nkey, \
                    (SELECT count(*) FROM tasks WHERE category_id=updated.id AND deleted_at IS NULL), \
                    updated.created_at, updated.updated_at FROM updated",
                &[&id, &name, &nkey],
            )
            .await;
        match res {
            Ok(Some(row)) => Ok(TaskCategory {
                id: row.get(0),
                name: row.get(1),
                nkey: row.get(2),
                task_count: row.get(3),
                created_at: row.get(4),
                updated_at: row.get(5),
            }),
            // A unique violation on the new nkey is a name conflict.
            Err(e) if e.is_unique_violation() => Err(Error::sentinel(
                ERR_TASK_CATEGORY_NAME_CONFLICT,
                ERR_TASK_CATEGORY_NAME_CONFLICT,
            )),
            Err(e) => Err(e),
            // 0 rows: the id does not exist.
            Ok(None) => Err(Error::sentinel(
                ERR_TASK_CATEGORY_NOT_FOUND,
                ERR_TASK_CATEGORY_NOT_FOUND,
            )),
        }
    }

    /// `DeleteTaskCategory`: remove a category; tasks fall back to uncategorized via
    /// the FK. Returns whether a row was deleted.
    pub async fn delete_task_category(&self, id: i64) -> Result<bool> {
        let n = self
            .execute("DELETE FROM task_categories WHERE id=$1", &[&id])
            .await?;
        Ok(n > 0)
    }

    /// `SetTaskCategory`: set (or clear, when `category_id` is None) one live task's
    /// category. Returns the refreshed category (or None when cleared / not found).
    pub async fn set_task_category(
        &self,
        task_id: i64,
        category_id: Option<i64>,
    ) -> Result<Option<TaskCategory>> {
        let Some(cid) = category_id else {
            let n = self
                .execute(
                    "UPDATE tasks SET category_id=NULL WHERE id=$1 AND deleted_at IS NULL",
                    &[&task_id],
                )
                .await?;
            if n == 0 {
                return Err(Error::sentinel(
                    ERR_TASK_CATEGORY_TASK_NOT_FOUND,
                    ERR_TASK_CATEGORY_TASK_NOT_FOUND,
                ));
            }
            return Ok(None);
        };
        if cid <= 0 {
            return Err(Error::sentinel(
                ERR_TASK_CATEGORY_INVALID,
                format!("{ERR_TASK_CATEGORY_INVALID}: category id must be positive"),
            ));
        }
        let row = self
            .query_opt(
                "WITH selected AS (\
                SELECT * FROM task_categories WHERE id=$2\
             ), updated AS (\
                UPDATE tasks SET category_id=$2 \
                WHERE id=$1 AND deleted_at IS NULL AND EXISTS (SELECT 1 FROM selected) RETURNING id\
             ) SELECT selected.id, selected.name, selected.nkey, \
                (SELECT count(*) FROM tasks WHERE category_id=selected.id AND deleted_at IS NULL), \
                selected.created_at, selected.updated_at FROM selected, updated",
                &[&task_id, &cid],
            )
            .await?;
        match row {
            Some(row) => Ok(Some(TaskCategory {
                id: row.get(0),
                name: row.get(1),
                nkey: row.get(2),
                task_count: row.get(3),
                created_at: row.get(4),
                updated_at: row.get(5),
            })),
            None => {
                // Either the task is missing or the category is missing.
                let task_exists: bool = self
                    .query_one(
                        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=$1 AND deleted_at IS NULL)",
                        &[&task_id],
                    )
                    .await?
                    .get(0);
                if !task_exists {
                    return Err(Error::sentinel(
                        ERR_TASK_CATEGORY_TASK_NOT_FOUND,
                        ERR_TASK_CATEGORY_TASK_NOT_FOUND,
                    ));
                }
                Err(Error::sentinel(
                    ERR_TASK_CATEGORY_NOT_FOUND,
                    ERR_TASK_CATEGORY_NOT_FOUND,
                ))
            }
        }
    }

    /// `SetTasksCategory`: move several tasks into one category (None = uncategorized)
    /// inside a single transaction. Returns the ids actually updated (missing/deleted
    /// ids are absent) plus the refreshed category (None when cleared).
    pub async fn set_tasks_category(
        &self,
        task_ids: Vec<i64>,
        category_id: Option<i64>,
    ) -> Result<(Vec<i64>, Option<TaskCategory>)> {
        if task_ids.is_empty() {
            return Err(Error::sentinel(
                ERR_TASK_CATEGORY_INVALID,
                format!("{ERR_TASK_CATEGORY_INVALID}: task ids are required"),
            ));
        }
        if task_ids.len() > MAX_TASK_CATEGORY_BATCH_SIZE {
            return Err(Error::sentinel(
                ERR_TASK_CATEGORY_INVALID,
                format!("{ERR_TASK_CATEGORY_INVALID}: at most {MAX_TASK_CATEGORY_BATCH_SIZE} tasks per request"),
            ));
        }
        if task_ids.iter().any(|&id| id <= 0) {
            return Err(Error::sentinel(
                ERR_TASK_CATEGORY_INVALID,
                format!("{ERR_TASK_CATEGORY_INVALID}: task id must be positive"),
            ));
        }
        if let Some(cid) = category_id {
            if cid <= 0 {
                return Err(Error::sentinel(
                    ERR_TASK_CATEGORY_INVALID,
                    format!("{ERR_TASK_CATEGORY_INVALID}: category id must be positive"),
                ));
            }
        }

        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;

        // Verify the category inside the transaction (avoids a concurrent delete
        // turning the UPDATE into an FK violation).
        if let Some(cid) = category_id {
            let exists: bool = tx
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM task_categories WHERE id=$1)",
                    &[&cid],
                )
                .await?
                .get(0);
            if !exists {
                return Err(Error::sentinel(
                    ERR_TASK_CATEGORY_NOT_FOUND,
                    ERR_TASK_CATEGORY_NOT_FOUND,
                ));
            }
        }

        let rows = tx
            .query(
                "UPDATE tasks SET category_id=$2 \
                 WHERE id=ANY($1::bigint[]) AND deleted_at IS NULL RETURNING id",
                &[&task_ids, &category_id],
            )
            .await?;
        let mut updated = Vec::with_capacity(rows.len());
        for row in &rows {
            updated.push(row.get::<_, i64>(0));
        }

        let category = if let Some(cid) = category_id {
            let sql = format!(
                "SELECT {TASK_CATEGORY_COLS} FROM task_categories category \
                 LEFT JOIN tasks task ON task.category_id=category.id \
                 WHERE category.id=$1 GROUP BY category.id"
            );
            let row = tx.query_one(&sql, &[&cid]).await?;
            Some(TaskCategory {
                id: row.get(0),
                name: row.get(1),
                nkey: row.get(2),
                task_count: row.get(3),
                created_at: row.get(4),
                updated_at: row.get(5),
            })
        } else {
            None
        };

        tx.commit().await?;
        Ok((updated, category))
    }
}
