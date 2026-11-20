//! Live Postgres database layer — port of the Go `db` package boot path
//! (`db.go` + `migrate.go`), on `tokio-postgres` + a `deadpool` pool.
//!
//! A Go `*sql.DB` with `SetMaxOpenConns(32)` maps onto a `deadpool` `Pool`
//! capped at 32 connections. `Open` mirrors `openWithMigrations`:
//!   1. ensure the target database exists (CREATE DATABASE if missing),
//!   2. ping,
//!   3. check out ONE connection, take the session-level advisory lock
//!      (`pg_advisory_lock`, key `schema_migration_lock_key()`),
//!   4. apply the base schema (retrying deadlocks) and then the migrations on
//!      that same connection,
//!   5. run `seedBuiltins` via the pool (separate connections, under the lock),
//!   6. release the lock and return the pool.
//!
//! The migration assets (`schema.sql` + `migrations/*.sql`) are pulled straight
//! from the Go tree with `include_str!` so the recorded checksums stay
//! byte-identical to the Go binary and cannot drift.

pub mod activity_outbox;
pub mod asset_intercept;
pub mod asset_intercept_match;
pub mod browser_mcp;
pub mod chat_mentions;
pub mod commands;
pub mod companies;
pub mod company_scope;
pub mod config_agents;
pub mod config_mcp;
pub mod config_profiles;
pub mod config_prompts;
pub mod config_visibility;
pub mod constants;
pub mod conversation;
pub mod coverage_ledger;
pub mod error;
pub mod exploration;
pub mod exploration_sources;
pub mod finding_evidence;
pub mod finding_retests;
pub mod finding_verification;
pub mod findings;
pub mod gotime;
pub mod intercept_store;
pub mod lessons;
pub mod llm_usage;
pub mod llmhealth;
pub mod llmretry;
pub mod logs;
pub mod migrate;
pub mod notification;
pub mod notification_channels;
pub mod notification_delivery;
pub mod notification_fanout;
pub mod publicsuffix;
pub mod secrets;
pub mod seed;
pub mod settings;
pub mod skill_gaps;
pub mod skill_usage;
pub mod task_archive_aggregate;
pub mod task_categories;
pub mod task_context;
pub mod task_credentials;
pub mod task_intercept;
pub mod task_outcome;
pub mod task_templates;
pub mod tasks;
pub mod tool_usage;
pub mod tools;
pub mod traffic_rewrite;
pub mod triggers;

/// Serde adapter module path alias used by `SkillStat`'s `#[serde(with = …)]`.
pub use gotime as gotime_serde;

pub use error::{Error, Result};
pub use migrate::{
    apply_migration, apply_migrations, embedded_migrations, exec_embedded_migration,
    load_migrations, migration_name_re, schema_migration_lock_key, Migration, SCHEMA_SQL,
};

use deadpool_postgres::{Manager, Pool};
use std::future::Future;
use std::pin::Pin;
use tokio_postgres::{Client, Config as PgConfig, NoTls};

/// `dbRetryMax` — bounded deadlock retries for the base-schema apply.
const DB_RETRY_MAX: usize = 5;

/// `DB` is the live Postgres handle: a `deadpool` pool standing in for Go's
/// `*sql.DB`. Cheap to clone (the pool is `Arc`-backed).
#[derive(Clone)]
pub struct DB {
    pub pool: Pool,
}

impl DB {
    /// `Open`: ensure the DB exists, then apply schema + embedded migrations +
    /// seed, all under the migration advisory lock.
    pub async fn open(dsn: &str) -> Result<DB> {
        let files: Vec<(String, String)> = embedded_migrations()
            .into_iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect();
        let migrations = load_migrations(&files)?;
        DB::open_with_migrations(dsn, migrations).await
    }

    /// `openWithMigrations`: the `Migrations` test seam — same boot path with a
    /// caller-supplied migration set.
    pub async fn open_with_migrations(dsn: &str, migrations: Vec<Migration>) -> Result<DB> {
        let cfg: PgConfig = dsn
            .parse()
            .map_err(|e: tokio_postgres::Error| Error::msg(e.to_string()))?;
        ensure_database(&cfg).await?;

        let pool = build_pool(cfg);

        // Ping so the caller learns the DB is reachable before we lock and seed.
        {
            let conn = pool.get().await?;
            conn.query_one("SELECT 1", &[]).await?;
        }

        // Schema DDL + migrations run on the lock-holder connection, and
        // seedBuiltins runs via the pool (separate connections) while the lock
        // is still held — matching Go, where the pool keeps ≥1 spare conn.
        let mut client = pool.get().await?;
        let seed_pool = pool.clone();
        with_schema_migration_lock(&mut client, move |client| {
            Box::pin(async move {
                apply_schema_with_retry(client, SCHEMA_SQL)
                    .await
                    .map_err(|e| Error::msg(format!("apply schema: {}", e)))?;
                apply_migrations(client, &migrations)
                    .await
                    .map_err(|e| Error::msg(format!("apply migrations: {}", e)))?;
                seed_builtins(&seed_pool)
                    .await
                    .map_err(|e| Error::msg(format!("seed builtins: {}", e)))?;
                Ok::<(), Error>(())
            }) as Pin<Box<dyn Future<Output = Result<()>> + '_>>
        })
        .await?;
        drop(client);

        Ok(DB { pool })
    }

    /// `EnsureLLMRecordsTable`: apply the llm_records migration outside the
    /// normal boot path (idempotent).
    pub async fn ensure_llm_records_table(&self) -> Result<()> {
        exec_embedded_migration(self, "0001_llm_records.sql")
            .await
            .map(|_| ())
    }

    /// `EnsureLLMUsageTable`: apply the llm_usage migration outside the normal
    /// boot path (idempotent).
    pub async fn ensure_llm_usage_table(&self) -> Result<()> {
        exec_embedded_migration(self, "0002_llm_usage.sql")
            .await
            .map(|_| ())
    }

    /// Convenience: check out a pooled connection and run one statement (mirrors
    /// Go's `(*sql.DB).Exec`, which borrows a conn per call).
    pub async fn execute(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<u64> {
        let conn = self.pool.get().await?;
        Ok(conn.execute(sql, params).await?)
    }

    /// Convenience: check out a pooled connection and run a query returning any
    /// number of rows (mirrors Go's `(*sql.DB).Query`).
    pub async fn query(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Vec<tokio_postgres::Row>> {
        let conn = self.pool.get().await?;
        Ok(conn.query(sql, params).await?)
    }

    /// Convenience: check out a pooled connection and run a query expected to
    /// return zero or one row (mirrors Go's `(*sql.DB).QueryRow` + `ErrNoRows`).
    pub async fn query_opt(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Option<tokio_postgres::Row>> {
        let conn = self.pool.get().await?;
        Ok(conn.query_opt(sql, params).await?)
    }

    /// Convenience: check out a pooled connection and run a query expected to
    /// return exactly one row (mirrors Go's `(*sql.DB).QueryRow`).
    pub async fn query_one(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<tokio_postgres::Row> {
        let conn = self.pool.get().await?;
        Ok(conn.query_one(sql, params).await?)
    }
}

/// Build a `deadpool` pool from a parsed config, capped at 32 connections to
/// match Go's `SetMaxOpenConns(32)`.
fn build_pool(cfg: PgConfig) -> Pool {
    let mgr = Manager::new(cfg, NoTls);
    Pool::builder(mgr)
        .max_size(32)
        .build()
        .expect("build postgres pool")
}

/// `withSchemaMigrationLock`: hold the session-level migration advisory lock for
/// the duration of `f`, which runs on the same connection. The closure gets
/// `&mut Client` so it can open a transaction for an individual migration.
pub(crate) async fn with_schema_migration_lock<T, F>(client: &mut Client, f: F) -> Result<T>
where
    F: for<'a> FnOnce(&'a mut Client) -> Pin<Box<dyn Future<Output = Result<T>> + 'a>>,
{
    client
        .query_one(
            &format!("SELECT pg_advisory_lock({})", schema_migration_lock_key()),
            &[],
        )
        .await?;
    let r = f(&mut *client).await;
    client
        .query_one(
            &format!("SELECT pg_advisory_unlock({})", schema_migration_lock_key()),
            &[],
        )
        .await?;
    r
}

/// `applySchemaWithRetry`: run the base schema, retrying Postgres deadlock
/// detections with exponential backoff, bounded by `DB_RETRY_MAX`.
async fn apply_schema_with_retry(client: &Client, schema_sql: &str) -> Result<()> {
    let mut attempt: usize = 0;
    loop {
        match client.batch_execute(schema_sql).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if is_deadlock(&e) {
                    if attempt >= DB_RETRY_MAX {
                        return Err(Error::msg(format!(
                            "apply schema after {} retries: {}",
                            DB_RETRY_MAX, e
                        )));
                    }
                    let sleep_ms = (50u64 * (1u64 << attempt)).min(1000);
                    tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                    attempt += 1;
                } else {
                    return Err(e.into());
                }
            }
        }
    }
}

/// True when the driver error carries Postgres deadlock code `40P01`.
fn is_deadlock(e: &tokio_postgres::Error) -> bool {
    e.as_db_error().map(|d| d.code())
        == Some(&tokio_postgres::error::SqlState::T_R_DEADLOCK_DETECTED)
}

/// `ensureDatabase`: create the target database if it does not already exist,
/// connecting to the maintenance `postgres` database. A duplicate-database race
/// is ignored.
async fn ensure_database(cfg: &PgConfig) -> Result<()> {
    let database = cfg.get_dbname().unwrap_or("").to_string();
    if database.is_empty() {
        return Ok(());
    }
    let mut admin_cfg = cfg.clone();
    admin_cfg.dbname("postgres");
    let pool = build_pool(admin_cfg);
    let conn = pool.get().await?;
    let exists = conn
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&database])
        .await?
        .is_some();
    if !exists {
        let create_sql = create_database_sql(cfg);
        if let Err(e) = conn.batch_execute(&create_sql).await {
            if e.as_db_error().map(|d| d.code())
                == Some(&tokio_postgres::error::SqlState::DUPLICATE_DATABASE)
            {
                // Lost the create race with another process; fine.
            } else {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

/// `createDatabaseSQL`: `CREATE DATABASE <name>`, with `TEMPLATE template0` when
/// the name contains a `/` (mirrors the Go guard).
fn create_database_sql(cfg: &PgConfig) -> String {
    let database = cfg.get_dbname().unwrap_or("");
    if database.contains('/') {
        format!(
            "CREATE DATABASE {} TEMPLATE template0",
            quote_ident(database)
        )
    } else {
        format!("CREATE DATABASE {}", quote_ident(database))
    }
}

/// Quote an identifier for safe interpolation (double-quote, doubling any
/// embedded double quotes).
fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Escape a libpq keyword-value connection-string value (backslash then quote).
fn escape_conf(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// `seedBuiltins`: seed the built-in agents + default rows via a temporary `DB`
/// handle over the shared pool (mirrors Go, where the seed runs through the pool
/// while the boot holds the migration lock on one connection). Each default-row
/// seed is behind its own settings gate so a user's later edits are preserved.
async fn seed_builtins(pool: &Pool) -> Result<()> {
    let db = DB { pool: pool.clone() };
    seed::seed_builtins(&db).await
}

/// `SuffixTestDatabase`: clone a DSN, changing only the database name by
/// appending `_` + suffix. Used by tests to create per-test scratch DBs. Handles
/// the TCP-host case (the only shape the differential harness uses).
pub fn suffix_test_database(dsn: &str, suffix: &str) -> Result<String> {
    let cfg: PgConfig = dsn
        .parse()
        .map_err(|e: tokio_postgres::Error| Error::msg(e.to_string()))?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(user) = cfg.get_user() {
        if !user.is_empty() {
            parts.push(format!("user={}", user));
        }
    }
    if let Some(password) = cfg.get_password() {
        let pw = String::from_utf8_lossy(password);
        parts.push(format!("password='{}'", escape_conf(&pw)));
    }
    let host = cfg.get_hosts().iter().find_map(|h| match h {
        tokio_postgres::config::Host::Tcp(s) => Some(s.clone()),
        #[cfg(unix)]
        _ => None,
    });
    if let Some(host) = host {
        parts.push(format!("host={}", host));
    }
    if let Some(port) = cfg.get_ports().first() {
        parts.push(format!("port={}", port));
    }
    let dbname = cfg.get_dbname().unwrap_or("");
    parts.push(format!("dbname={}{}", dbname, suffix));
    if let Some(options) = cfg.get_options() {
        if !options.is_empty() {
            parts.push(format!("options='{}'", escape_conf(options)));
        }
    }
    Ok(parts.join(" "))
}
