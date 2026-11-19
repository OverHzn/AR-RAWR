//! Task templates — port of `db/task_templates.go`.
//!
//! A `TaskTemplate` is a globally reusable task preset (description/goal plus an
//! optional category and a snapshot of task-level intercept/allow rules). The
//! pure name helpers (`taskTemplateName`/`taskTemplateNKey`) live in
//! [`dbx_rs::port`] (gate-pinned, single source of truth); this module keeps the
//! normalization/validation and the DB CRUD.
//!
//! The JSONB `intercept_rules` element type (`TaskInterceptRuleInput`) is the
//! same type used by `db/task_intercept.go` for live task rules; it lives here
//! because the template snapshot is its first consumer (the `*AssetStore`
//! task-rule methods are ported with the assets group).

use crate::db::error::{Error, Result};
use crate::db::DB;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// `MaxTaskTemplateNameRunes`.
pub const MAX_TASK_TEMPLATE_NAME_RUNES: i64 = 120;
/// `MaxTaskTemplateTextRunes`.
pub const MAX_TASK_TEMPLATE_TEXT_RUNES: i64 = 16000;

/// Sentinel identities — Rust analogues of the package's `Err…` values, matched
/// via [`Error::is`].
pub const ERR_TASK_TEMPLATE_INVALID: &str = "ErrTaskTemplateInvalid";
pub const ERR_TASK_TEMPLATE_NAME_CONFLICT: &str = "ErrTaskTemplateNameConflict";
pub const ERR_TASK_TEMPLATE_NOT_FOUND: &str = "ErrTaskTemplateNotFound";

const MSG_TASK_TEMPLATE_INVALID: &str = "invalid task template";
const MSG_TASK_TEMPLATE_NAME_CONFLICT: &str = "task template name already exists";
const MSG_TASK_TEMPLATE_NOT_FOUND: &str = "task template not found";

const TASK_TEMPLATE_COLS: &str =
    "id, name, nkey, description, goal, category_id, intercept_rules, created_at, updated_at";

/// `TaskInterceptRuleInput` — one task-level rule (block or allow). `Action` is
/// `block`/`allow` (empty is treated as `block` downstream). `#[serde(default)]`
/// mirrors Go's lenient unmarshal (missing fields stay at their zero value).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskInterceptRuleInput {
    pub enabled: bool,
    pub action: String,
    pub kind: String,
    pub pattern: String,
    pub note: String,
}

/// `TaskTemplate` — a reusable task preset as read back from the store.
#[derive(Debug, Clone)]
pub struct TaskTemplate {
    pub id: i64,
    pub name: String,
    pub n_key: String,
    pub description: String,
    pub goal: String,
    pub category_id: Option<i64>,
    pub intercept_rules: Vec<TaskInterceptRuleInput>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

/// `TaskTemplateInput` — the create/update payload after normalization.
#[derive(Debug, Clone, Default)]
pub struct TaskTemplateInput {
    pub name: String,
    pub description: String,
    pub goal: String,
    pub category_id: Option<i64>,
    pub intercept_rules: Vec<TaskInterceptRuleInput>,
}

/// `TaskTemplatePatch` — changes only fields flagged as set. `name`/`description`/
/// `goal` use `Option` (a `None` means "leave unchanged"); `category_id`/
/// `intercept_rules` use explicit `*_set` flags so a `None`/`empty` value can
/// still mean "clear" when the flag is true.
#[derive(Debug, Clone, Default)]
pub struct TaskTemplatePatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub goal: Option<String>,
    pub category_id: Option<i64>,
    pub set_category_id: bool,
    pub intercept_rules: Vec<TaskInterceptRuleInput>,
    pub set_intercept_rules: bool,
}

/// `utf8.RuneCountInString`.
fn rune_count(s: &str) -> i64 {
    s.chars().count() as i64
}

fn invalid(detail: &str) -> Error {
    Error::sentinel(
        ERR_TASK_TEMPLATE_INVALID,
        format!("{MSG_TASK_TEMPLATE_INVALID}: {detail}"),
    )
}

/// `scanTaskTemplate`.
fn scan_task_template(row: &tokio_postgres::Row) -> Result<TaskTemplate> {
    let raw: serde_json::Value = row.get(6);
    let intercept_rules = if raw.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(raw).map_err(|e| Error::msg(e.to_string()))?
    };
    Ok(TaskTemplate {
        id: row.get(0),
        name: row.get(1),
        n_key: row.get(2),
        description: row.get(3),
        goal: row.get(4),
        category_id: row.get(5),
        intercept_rules,
        created_at: row.get(7),
        updated_at: row.get(8),
    })
}

/// `marshalTemplateRules` — always a JSON array (never null).
fn marshal_template_rules(rules: &[TaskInterceptRuleInput]) -> Result<serde_json::Value> {
    serde_json::to_value(rules).map_err(|e| Error::msg(e.to_string()))
}

/// `normalizeTaskTemplateInput`: collapse name whitespace, trim description/goal,
/// validate, and return the case-insensitive `nkey`.
fn normalize_task_template_input(
    mut input: TaskTemplateInput,
) -> Result<(TaskTemplateInput, String)> {
    input.name = dbx_rs::port::task_template_name(&input.name);
    input.description = gocompat::strings::go_trim_space(&input.description).to_string();
    input.goal = gocompat::strings::go_trim_space(&input.goal).to_string();
    if input.name.is_empty() {
        return Err(invalid("name is required"));
    }
    if rune_count(&input.name) > MAX_TASK_TEMPLATE_NAME_RUNES {
        return Err(invalid(&format!(
            "name exceeds {MAX_TASK_TEMPLATE_NAME_RUNES} characters"
        )));
    }
    if input.description.is_empty() {
        return Err(invalid("description is required"));
    }
    if rune_count(&input.description) > MAX_TASK_TEMPLATE_TEXT_RUNES {
        return Err(invalid(&format!(
            "description exceeds {MAX_TASK_TEMPLATE_TEXT_RUNES} characters"
        )));
    }
    if input.goal.is_empty() {
        return Err(invalid("goal is required"));
    }
    if rune_count(&input.goal) > MAX_TASK_TEMPLATE_TEXT_RUNES {
        return Err(invalid(&format!(
            "goal exceeds {MAX_TASK_TEMPLATE_TEXT_RUNES} characters"
        )));
    }
    let nkey = dbx_rs::port::task_template_n_key(&input.name);
    Ok((input, nkey))
}

/// `normalizeTaskTemplatePatch`: validate the supplied fields and, when the name
/// changes, return the new `nkey` (`None` when the name is untouched).
fn normalize_task_template_patch(
    mut patch: TaskTemplatePatch,
) -> Result<(TaskTemplatePatch, Option<String>)> {
    if patch.name.is_none()
        && patch.description.is_none()
        && patch.goal.is_none()
        && !patch.set_category_id
        && !patch.set_intercept_rules
    {
        return Err(invalid("no fields supplied"));
    }
    let mut nkey: Option<String> = None;
    if let Some(name) = patch.name.take() {
        let name = dbx_rs::port::task_template_name(&name);
        if name.is_empty() {
            return Err(invalid("name is required"));
        }
        if rune_count(&name) > MAX_TASK_TEMPLATE_NAME_RUNES {
            return Err(invalid(&format!(
                "name exceeds {MAX_TASK_TEMPLATE_NAME_RUNES} characters"
            )));
        }
        nkey = Some(dbx_rs::port::task_template_n_key(&name));
        patch.name = Some(name);
    }
    if let Some(description) = patch.description.take() {
        let description = gocompat::strings::go_trim_space(&description).to_string();
        if description.is_empty() {
            return Err(invalid("description is required"));
        }
        if rune_count(&description) > MAX_TASK_TEMPLATE_TEXT_RUNES {
            return Err(invalid(&format!(
                "description exceeds {MAX_TASK_TEMPLATE_TEXT_RUNES} characters"
            )));
        }
        patch.description = Some(description);
    }
    if let Some(goal) = patch.goal.take() {
        let goal = gocompat::strings::go_trim_space(&goal).to_string();
        if goal.is_empty() {
            return Err(invalid("goal is required"));
        }
        if rune_count(&goal) > MAX_TASK_TEMPLATE_TEXT_RUNES {
            return Err(invalid(&format!(
                "goal exceeds {MAX_TASK_TEMPLATE_TEXT_RUNES} characters"
            )));
        }
        patch.goal = Some(goal);
    }
    Ok((patch, nkey))
}

impl DB {
    /// `CreateTaskTemplate`: insert one globally reusable preset.
    pub async fn create_task_template(&self, input: TaskTemplateInput) -> Result<TaskTemplate> {
        let (input, nkey) = normalize_task_template_input(input)?;
        let rules_json = marshal_template_rules(&input.intercept_rules)?;
        let sql = format!(
            "INSERT INTO task_templates(name, nkey, description, goal, category_id, intercept_rules)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (nkey) DO NOTHING
             RETURNING {TASK_TEMPLATE_COLS}"
        );
        let row = self
            .query_opt(
                &sql,
                &[
                    &input.name,
                    &nkey,
                    &input.description,
                    &input.goal,
                    &input.category_id,
                    &rules_json,
                ],
            )
            .await?;
        match row {
            Some(row) => scan_task_template(&row),
            None => Err(Error::sentinel(
                ERR_TASK_TEMPLATE_NAME_CONFLICT,
                MSG_TASK_TEMPLATE_NAME_CONFLICT,
            )),
        }
    }

    /// `ListTaskTemplates`: the most recently maintained templates first.
    pub async fn list_task_templates(&self) -> Result<Vec<TaskTemplate>> {
        let sql = format!(
            "SELECT {TASK_TEMPLATE_COLS} FROM task_templates ORDER BY updated_at DESC, id DESC"
        );
        let rows = self.query(&sql, &[]).await?;
        rows.iter().map(scan_task_template).collect()
    }

    /// `GetTaskTemplate`: `Ok(None)` when `id` does not exist.
    pub async fn get_task_template(&self, id: i64) -> Result<Option<TaskTemplate>> {
        let sql = format!("SELECT {TASK_TEMPLATE_COLS} FROM task_templates WHERE id=$1");
        match self.query_opt(&sql, &[&id]).await? {
            Some(row) => scan_task_template(&row).map(Some),
            None => Ok(None),
        }
    }

    /// `UpdateTaskTemplate`: replace the editable fields of one preset.
    pub async fn update_task_template(
        &self,
        id: i64,
        input: TaskTemplateInput,
    ) -> Result<TaskTemplate> {
        let (input, _) = normalize_task_template_input(input)?;
        let patch = TaskTemplatePatch {
            name: Some(input.name),
            description: Some(input.description),
            goal: Some(input.goal),
            ..Default::default()
        };
        self.patch_task_template(id, patch).await
    }

    /// `PatchTaskTemplate`: atomically change only the supplied fields. Keeping
    /// the merge in one `UPDATE` prevents concurrent disjoint patches from losing
    /// each other's changes.
    pub async fn patch_task_template(
        &self,
        id: i64,
        patch: TaskTemplatePatch,
    ) -> Result<TaskTemplate> {
        let (patch, nkey) = normalize_task_template_patch(patch)?;
        let rules_json = marshal_template_rules(&patch.intercept_rules)?;
        let sql = format!(
            "UPDATE task_templates
             SET name=CASE WHEN $2 THEN $3::text ELSE name END,
                 nkey=CASE WHEN $2 THEN $4::text ELSE nkey END,
                 description=CASE WHEN $5 THEN $6::text ELSE description END,
                 goal=CASE WHEN $7 THEN $8::text ELSE goal END,
                 category_id=CASE WHEN $9 THEN $10::bigint ELSE category_id END,
                 intercept_rules=CASE WHEN $11 THEN $12::jsonb ELSE intercept_rules END
             WHERE id=$1
             RETURNING {TASK_TEMPLATE_COLS}"
        );
        let name_set = patch.name.is_some();
        let description_set = patch.description.is_some();
        let goal_set = patch.goal.is_some();
        let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
            &id,
            &name_set,
            &patch.name,
            &nkey,
            &description_set,
            &patch.description,
            &goal_set,
            &patch.goal,
            &patch.set_category_id,
            &patch.category_id,
            &patch.set_intercept_rules,
            &rules_json,
        ];
        match self.query_opt(&sql, params).await {
            Ok(Some(row)) => scan_task_template(&row),
            Ok(None) => Err(Error::sentinel(
                ERR_TASK_TEMPLATE_NOT_FOUND,
                MSG_TASK_TEMPLATE_NOT_FOUND,
            )),
            Err(e) if e.is_unique_violation() => Err(Error::sentinel(
                ERR_TASK_TEMPLATE_NAME_CONFLICT,
                MSG_TASK_TEMPLATE_NAME_CONFLICT,
            )),
            Err(e) => Err(e),
        }
    }

    /// `DeleteTaskTemplate`: delete one preset and report whether it existed.
    pub async fn delete_task_template(&self, id: i64) -> Result<bool> {
        let n = self
            .execute("DELETE FROM task_templates WHERE id=$1", &[&id])
            .await?;
        Ok(n > 0)
    }
}
