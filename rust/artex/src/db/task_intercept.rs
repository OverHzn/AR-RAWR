//! Per-task interception rules — port of the writer half of `db/task_intercept.go`.
//!
//! A `task_intercept_rules` row is one asset-interception rule scoped to a single
//! task (block or allow a domain / ip / url / cidr). This slice is just the
//! creation-time bulk insert (`insertTaskInterceptRules`, called by
//! `CreateTaskWithOptions`) plus `normalizeRuleAction`. The `AssetStore` per-task
//! CRUD (`ListTaskInterceptRules` / `Create`/`Update`/`Delete`/`Toggle`) lands with
//! the AssetStore group.

use crate::db::error::{Error, Result};
use crate::db::task_templates::TaskInterceptRuleInput;

/// `normalizeRuleAction`: anything other than `allow` normalizes to `block`.
pub fn normalize_rule_action(action: &str) -> String {
    if action == "allow" {
        "allow".to_string()
    } else {
        "block".to_string()
    }
}

/// `insertTaskInterceptRules`: write every rule for a task inside the create tx.
/// Wraps a failing insert with the offending pattern (Go's `%q`).
pub(crate) async fn insert_task_intercept_rules(
    tx: &deadpool_postgres::Transaction<'_>,
    task_id: i64,
    rules: &[TaskInterceptRuleInput],
) -> Result<()> {
    for r in rules {
        let action = normalize_rule_action(&r.action);
        tx.execute(
            "INSERT INTO task_intercept_rules(task_id, enabled, action, kind, pattern, note) \
             VALUES ($1,$2,$3,$4,$5,$6)",
            &[&task_id, &r.enabled, &action, &r.kind, &r.pattern, &r.note],
        )
        .await
        .map_err(|e| Error::msg(format!("insert task intercept rule {:?}: {e}", r.pattern)))?;
    }
    Ok(())
}
