//! Schema migration runner — port of `db/migrate.go`.
//!
//! Embedding and byte-identical parity: the schema and migration assets are
//! pulled straight from the Go tree with `include_str!`, so the sha256
//! checksums recorded in `schema_migrations` match the Go binary exactly and
//! cannot drift. A migration is one `NNNN_slug.sql` file applied inside its
//! own transaction: `-- migrate:Up` is a no-op comment (Go's `database/sql`
//! Exec runs the whole script as one multi-statement simple-protocol call), so
//! `batch_execute` is the faithful equivalent.

use crate::db::error::{Error, Result};
use crate::db::DB;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use tokio_postgres::Client;

/// The literal SQL for the bookkeeping table. `execEmbeddedMigration` runs it
/// inside its transaction because it never opens the DB through `Open`.
const SCHEMA_MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (
    version BIGINT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    checksum TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);";

/// `schemaMigrationLockKey` — the integer handed to `pg_advisory_lock`. It must
/// stay in sync with the Go binary (9router) so a mixed Go/Rust rollout
/// contends on the same lock.
pub fn schema_migration_lock_key() -> i64 {
    7337741001
}

/// `migrationNameRE`: `^([0-9]{4})_[a-z0-9_]+\.sql$`.
pub fn migration_name_re(name: &str) -> bool {
    let b = name.as_bytes();
    // NNNN _ X .sql  → at least 4+1+1+4 bytes.
    if b.len() < 4 + 1 + 1 + 4 {
        return false;
    }
    if !b[..4].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if b[4] != b'_' {
        return false;
    }
    if !name.ends_with(".sql") {
        return false;
    }
    let mid = &b[5..b.len() - 4];
    if mid.is_empty() {
        return false;
    }
    mid.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// A single parsed migration file: its sequence number (also the schema
/// version), base filename, ordered version, file bytes and sha256 checksum.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Base name, e.g. `0001_llm_records.sql` (the `name` column).
    pub name: String,
    /// Version parsed from the `NNNN` prefix (also used for ordering).
    pub version: i64,
    /// sha256 checksum of `sql` as lowercase hex.
    pub checksum: String,
    /// The migration's SQL bytes.
    pub sql: String,
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// `loadMigrations`: parse, validate, checksum and order the embedded files.
///
/// Every file must match `migrationNameRE`, carry a positive version, have a
/// unique version and name, and come back sorted by version. Tests call this
/// with hand-built files to exercise the validation branches.
pub fn load_migrations(files: &[(String, String)]) -> Result<Vec<Migration>> {
    let mut migrations: Vec<Migration> = Vec::new();
    let mut seen: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    for (name, sql) in files {
        // Skip non-`.sql` files (Go: path.Ext(name) != ".sql"). A `.sql` file
        // whose name is not NNNN_slug.sql is an error, not a skip.
        if !name.ends_with(".sql") {
            continue;
        }
        if !migration_name_re(name) {
            return Err(Error::msg(format!(
                "migration file {}: name must be NNNN_slug.sql",
                name
            )));
        }
        // The regex guarantees the first four bytes are ASCII digits.
        let version: i64 = name[..4].parse().unwrap();
        if version == 0 {
            return Err(Error::msg(format!(
                "migration file {}: version must be > 0",
                name
            )));
        }
        if let Some(prev) = seen.get(&version) {
            return Err(Error::msg(format!(
                "migration version {:04} is used by both {} and {}",
                version, prev, name
            )));
        }
        seen.insert(version, name.clone());
        migrations.push(Migration {
            name: name.clone(),
            version,
            checksum: to_hex(&Sha256::digest(sql.as_bytes())),
            sql: sql.clone(),
        });
    }
    migrations.sort_by_key(|m| m.version);
    Ok(migrations)
}

/// `applyMigration`: run one migration's SQL and record it in one transaction.
pub async fn apply_migration(client: &mut Client, m: &Migration) -> Result<()> {
    let tx = client.transaction().await?;
    tx.batch_execute(&m.sql).await?;
    tx.execute(
        "INSERT INTO schema_migrations (version, name, checksum) VALUES ($1, $2, $3)",
        &[&m.version, &m.name, &m.checksum],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `applyMigrations`: apply every not-yet-applied migration in order. Returns
/// the names of the migrations it applied. Refuses an out-of-order pending
/// migration (one older than a version already recorded) before writing
/// anything, so a partially applied history can never skip a version.
pub async fn apply_migrations(
    client: &mut Client,
    migrations: &[Migration],
) -> Result<Vec<String>> {
    // Go's applyMigrations recreates the bookkeeping table (CREATE TABLE IF NOT
    // EXISTS) before reading it — an install may predate schema_migrations and
    // have had it dropped.
    client
        .batch_execute(SCHEMA_MIGRATIONS_TABLE)
        .await
        .map_err(|e| Error::msg(format!("create schema_migrations: {}", e)))?;

    let applied_rows: Vec<(i64, String, String)> = client
        .query(
            "SELECT version, name, checksum FROM schema_migrations ORDER BY version",
            &[],
        )
        .await
        .map_err(|e| Error::msg(format!("read schema_migrations: {}", e)))?
        .iter()
        .map(|r| {
            (
                r.get::<_, i64>(0),
                r.get::<_, String>(1),
                r.get::<_, String>(2),
            )
        })
        .collect();

    let applied: std::collections::HashMap<i64, (String, String)> = applied_rows
        .iter()
        .cloned()
        .map(|(v, n, c)| (v, (n, c)))
        .collect();
    let applied_versions: Vec<i64> = applied_rows.iter().map(|(v, _, _)| *v).collect();
    let by_version: std::collections::HashMap<i64, &Migration> =
        migrations.iter().map(|m| (m.version, m)).collect();

    // Every version the database records must exist in this binary, under the
    // same name and with the same checksum it was applied with.
    for &v in &applied_versions {
        let (r_name, r_checksum) = &applied[&v];
        match by_version.get(&v).copied() {
            None => {
                return Err(Error::msg(format!(
                    "migration {:04} ({}) is applied in the database but missing from this binary",
                    v, r_name
                )))
            }
            Some(m) => {
                if r_name != &m.name {
                    return Err(Error::msg(format!(
                        "migration {:04}: database records file {} but this binary has {}",
                        v, r_name, m.name
                    )));
                }
                if r_checksum != &m.checksum {
                    return Err(Error::msg(format!(
                        "migration {:04} ({}): checksum changed since it was applied (database {}, file {}); published migrations must not be edited",
                        v, m.name, r_checksum, m.checksum
                    )));
                }
            }
        }
    }

    // A pending version below the newest applied one would run out of order on
    // this database: refuse it before writing anything.
    if let Some(&max_version) = applied_versions.last() {
        for m in migrations {
            if !applied.contains_key(&m.version) && m.version < max_version {
                let max_name = &applied[&max_version].0;
                return Err(Error::msg(format!(
                    "migration {} ({:04}) is older than the applied migration {} ({:04}); migrations must be appended, never inserted",
                    m.name, m.version, max_name, max_version
                )));
            }
        }
    }

    let mut ran: Vec<String> = Vec::new();
    for m in migrations {
        if applied.contains_key(&m.version) {
            continue;
        }
        apply_migration(client, m)
            .await
            .map_err(|e| Error::msg(format!("migration {:04} ({}): {}", m.version, m.name, e)))?;
        ran.push(m.name.clone());
    }
    Ok(ran)
}

/// `execEmbeddedMigration`: apply an embedded migration's SQL by name from
/// outside the normal boot path (used by tests and the field repair helper).
/// Creates `schema_migrations`, takes the shared `pg_advisory_xact_lock` and
/// runs the script, all in one transaction.
pub async fn exec_embedded_migration(db: &DB, name: &str) -> Result<String> {
    let mut client = db.pool.get().await?;
    let tx = client.transaction().await?;
    tx.batch_execute(SCHEMA_MIGRATIONS_TABLE).await?;
    let coordinate = coordinate_with_schema_migration(tx.client());
    match coordinate.await {
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    let sql = embedded_migrations()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| s.to_string())
        .ok_or_else(|| Error::msg(format!("embedded migration {} not found", name)))?;
    tx.batch_execute(&sql).await?;
    tx.commit().await?;
    Ok(sql)
}

/// `coordinateWithSchemaMigration`: the transaction-level advisory lock the
/// schema boot path also holds (session-level `pg_advisory_lock`). Take this
/// before migrating a live schema.
async fn coordinate_with_schema_migration(
    client: &Client,
) -> std::result::Result<(), tokio_postgres::Error> {
    client
        .query_one(
            &format!(
                "SELECT pg_advisory_xact_lock({})",
                schema_migration_lock_key()
            ),
            &[],
        )
        .await?;
    Ok(())
}

/// The embedded base schema (`db/schema.sql`), byte-identical to the Go binary.
pub const SCHEMA_SQL: &str = include_str!("../../../../db/schema.sql");

/// `embeddedMigrations`: the seven ordered migrations, byte-identical to the Go
/// binary. Pulled straight from the Go tree so the recorded checksums match.
pub fn embedded_migrations() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "0001_llm_records.sql",
            include_str!("../../../../db/migrations/0001_llm_records.sql"),
        ),
        (
            "0002_llm_usage.sql",
            include_str!("../../../../db/migrations/0002_llm_usage.sql"),
        ),
        (
            "0003_task_scope_icp_index.sql",
            include_str!("../../../../db/migrations/0003_task_scope_icp_index.sql"),
        ),
        (
            "0004_llm_usage_ts_index.sql",
            include_str!("../../../../db/migrations/0004_llm_usage_ts_index.sql"),
        ),
        (
            "0005_traffic_rewrite_rules.sql",
            include_str!("../../../../db/migrations/0005_traffic_rewrite_rules.sql"),
        ),
        (
            "0006_activity_outbox.sql",
            include_str!("../../../../db/migrations/0006_activity_outbox.sql"),
        ),
        (
            "0007_finding_report_state.sql",
            include_str!("../../../../db/migrations/0007_finding_report_state.sql"),
        ),
    ]
}
