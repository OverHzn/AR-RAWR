//! Per-task test credentials — port of `db/task_credentials.go`.
//!
//! A `TaskCredential` is a per-task test account: declared by the user at task
//! creation (source=user) or registered by the agent (source=agent). Passwords are
//! encrypted at rest (`secrets.go`, column `task_credentials.password`) and masked
//! by the HTTP API; agent tools read them decrypted.
//!
//! The `ExplorationStore` thin wrappers in Go just forward to these `*DB` methods,
//! so the store inherits them for free once ported.

use crate::db::error::{Error, Result};
use crate::db::secrets::{open_secret, seal_secret, COL_CRED_PASS};
use crate::db::DB;
use gocompat::strings::go_trim_space;
use std::time::SystemTime;

/// `TaskCredential` — one per-task test account.
#[derive(Debug, Clone)]
pub struct TaskCredential {
    pub id: i64,
    pub task_id: i64,
    pub domain: String,
    pub username: String,
    pub password: String,
    /// "user" | "agent".
    pub source: String,
    pub note: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

/// `TaskCredentialInput` — validated + normalized upsert payload.
#[derive(Debug, Clone, Default)]
pub struct TaskCredentialInput {
    pub domain: String,
    pub username: String,
    pub password: String,
    pub source: String,
    pub note: String,
}

/// `ErrTaskCredentialInvalid`: "credential domain and username are required".
pub const ERR_TASK_CREDENTIAL_INVALID: &str = "credential domain and username are required";

impl TaskCredentialInput {
    /// `normalize`: trim fields, default source to "user", validate.
    pub fn normalize(&self) -> std::result::Result<TaskCredentialInput, Error> {
        let out = TaskCredentialInput {
            domain: go_trim_space(&self.domain).to_string(),
            username: go_trim_space(&self.username).to_string(),
            password: go_trim_space(&self.password).to_string(),
            source: if self.source == "agent" {
                "agent".to_string()
            } else {
                "user".to_string()
            },
            note: go_trim_space(&self.note).to_string(),
        };
        if out.domain.is_empty() || out.username.is_empty() {
            return Err(Error::msg(ERR_TASK_CREDENTIAL_INVALID));
        }
        if out.password.is_empty() && out.source == "agent" {
            return Err(Error::msg(
                "agent-recorded credentials need the password that was set",
            ));
        }
        Ok(out)
    }
}

/// `scanTaskCredential`: scan a row on the credential column list.
fn scan_credential(row: &tokio_postgres::Row) -> TaskCredential {
    TaskCredential {
        id: row.get(0),
        task_id: row.get(1),
        domain: row.get(2),
        username: row.get(3),
        password: row.get(4),
        source: row.get(5),
        note: row.get(6),
        created_at: row.get(7),
        updated_at: row.get(8),
    }
}

const CRED_COLS: &str =
    "id, task_id, domain, username, password, source, note, created_at, updated_at";

impl DB {
    /// `UpsertTaskCredential`: insert or update the (task, domain, username) entry.
    /// An agent re-registering the same account overwrites the password in place.
    pub async fn upsert_task_credential(
        &self,
        task_id: i64,
        input: TaskCredentialInput,
    ) -> Result<TaskCredential> {
        let input = input.normalize()?;
        let pass = seal_secret(&COL_CRED_PASS, &input.password).map_err(Error::msg)?;
        let sql = format!(
            "INSERT INTO task_credentials (task_id, domain, username, password, source, note) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (task_id, domain, username) DO UPDATE \
             SET password = EXCLUDED.password, \
                 source   = EXCLUDED.source, \
                 note     = CASE WHEN EXCLUDED.note = '' THEN task_credentials.note ELSE EXCLUDED.note END, \
                 updated_at = now() \
             RETURNING {CRED_COLS}"
        );
        let row = self
            .query_one(
                &sql,
                &[
                    &task_id,
                    &input.domain,
                    &input.username,
                    &pass,
                    &input.source,
                    &input.note,
                ],
            )
            .await?;
        let mut c = scan_credential(&row);
        // The DB round-trips the sealed password; callers see the plaintext.
        c.password = input.password;
        Ok(c)
    }

    /// `TaskCredentials`: list a task's credentials, oldest first.
    pub async fn task_credentials(&self, task_id: i64) -> Result<Vec<TaskCredential>> {
        let sql =
            format!("SELECT {CRED_COLS} FROM task_credentials WHERE task_id = $1 ORDER BY id");
        let rows = self.query(&sql, &[&task_id]).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut c = scan_credential(row);
            c.password = open_secret(&COL_CRED_PASS, &c.password).map_err(Error::msg)?;
            out.push(c);
        }
        Ok(out)
    }

    /// `DeleteTaskCredential`: remove one entry (taskID-scoped). Errors if none.
    pub async fn delete_task_credential(&self, task_id: i64, id: i64) -> Result<()> {
        let n = self
            .execute(
                "DELETE FROM task_credentials WHERE id = $1 AND task_id = $2",
                &[&id, &task_id],
            )
            .await?;
        if n == 0 {
            return Err(Error::msg("credential not found"));
        }
        Ok(())
    }
}
