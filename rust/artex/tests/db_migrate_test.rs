//! Differential tests for the `db` migration boot path — port of `db/migrate_test.go`.
//!
//! These run against the live Postgres from `ARTEX_PG_DSN` (the same image the
//! Go oracle uses) and skip cleanly when it is unset or unreachable, exactly
//! like the Go specs. Each test that needs a database carves out a per-test
//! scratch database (`<db>_mig_<suffix>`), dropped first to clear leftovers, and
//! serialized behind a shared async lock so concurrent `CREATE`/`DROP DATABASE`
//! never collide (the Go `TestMain` serializes on an advisory lock).
//!
//! The pure `load_migrations` validation test needs no database.

use artex::db::{self, DB};
use tokio_postgres::{Client, NoTls};

/// Serializes the DB tests: only one may create/drop a scratch database at a
/// time. `tokio::sync::Mutex` is runtime-agnostic, so a guard held by one
/// `#[tokio::test]` runtime correctly blocks a waiter on another.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `probeMigration`: records how many migrations were applied before it ran and
/// fails if it ever runs twice (its `CREATE TABLE` has no IF NOT EXISTS).
const PROBE: &str = "\nCREATE TABLE migration_probe (applied_before INTEGER);\nINSERT INTO migration_probe SELECT count(*) FROM schema_migrations;\n";

/// A migration that fails partway (`SELECT 1/0`), to prove rollback.
const BROKEN: &str = "\nCREATE TABLE migration_broken (x INTEGER);\nSELECT 1/0;\n";

#[derive(Debug, Clone)]
struct Applied {
    version: i64,
    name: String,
    checksum: String,
    applied_at: String,
}

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(b);
    let mut s = String::with_capacity(64);
    for x in d {
        s.push_str(&format!("{:02x}", x));
    }
    s
}

fn qi(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Connect to the maintenance `postgres` database of the same server.
fn admin_config(base: &str) -> Option<tokio_postgres::Config> {
    let mut c: tokio_postgres::Config = base.parse().ok()?;
    c.dbname("postgres");
    Some(c)
}

async fn connect(cfg: tokio_postgres::Config) -> Option<Client> {
    let (client, conn) = cfg.connect(NoTls).await.ok()?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Some(client)
}

async fn raw_connect(dsn: &str) -> Option<Client> {
    connect(dsn.parse().ok()?).await
}

/// `scratchDSN`: drop any leftover `<db>_mig_<suffix>` and return the scratch
/// DSN plus its database name. Returns `None` if Postgres is unreachable.
async fn scratch(base: &str, suffix: &str) -> Option<(String, String)> {
    let base_cfg: tokio_postgres::Config = base.parse().ok()?;
    let name = format!("{}_mig_{}", base_cfg.get_dbname().unwrap_or(""), suffix);
    let admin = connect(admin_config(base)?).await?;
    admin
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            qi(&name)
        ))
        .await
        .ok()?;
    drop(admin);
    let dsn = db::suffix_test_database(base, &format!("_mig_{}", suffix)).ok()?;
    Some((dsn, name))
}

async fn drop_database(base: &str, name: &str) {
    if let Some(cfg) = admin_config(base) {
        if let Some(admin) = connect(cfg).await {
            let _ = admin
                .batch_execute(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    qi(name)
                ))
                .await;
        }
    }
}

/// `withEmbedded`: the two LLM migrations plus extra files.
fn with_embedded(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = db::embedded_migrations()
        .into_iter()
        .take(2)
        .map(|(n, s)| (n.to_string(), s.to_string()))
        .collect();
    for (n, s) in extra {
        v.push((n.to_string(), s.to_string()));
    }
    v
}

async fn read_applied(c: &Client) -> Vec<Applied> {
    let rows = c
        .query(
            "SELECT version, name, checksum, applied_at::text FROM schema_migrations ORDER BY version",
            &[],
        )
        .await
        .expect("read schema_migrations");
    rows.iter()
        .map(|r| Applied {
            version: r.get(0),
            name: r.get(1),
            checksum: r.get(2),
            applied_at: r.get(3),
        })
        .collect()
}

/// `assertEmbeddedApplied`: both LLM tables exist and every embedded migration
/// is recorded with the sha256 of its file.
async fn assert_embedded_applied(c: &Client) -> Vec<Applied> {
    for table in ["llm_records", "llm_usage"] {
        let row = c
            .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
            .await
            .expect("to_regclass");
        let exists: bool = row.get(0);
        assert!(exists, "table {table} missing after Open");
    }
    let applied = read_applied(c).await;
    let expected: Vec<(i64, String, String)> = db::embedded_migrations()
        .into_iter()
        .map(|(n, s)| {
            let v: i64 = n[..4].parse().unwrap();
            (v, n.to_string(), sha256_hex(s.as_bytes()))
        })
        .collect();
    assert_eq!(
        applied.len(),
        expected.len(),
        "schema_migrations = {applied:?}, want versions 1 to 7"
    );
    for (i, (v, name, sum)) in expected.iter().enumerate() {
        let got = &applied[i];
        assert_eq!(got.version, *v, "schema_migrations row {i} version");
        assert_eq!(&got.name, name, "schema_migrations row {i} name");
        assert_eq!(&got.checksum, sum, "schema_migrations row {i} checksum");
    }
    applied
}

/// `assertReopenIsNoop`: reopening adds no migration row and re-applies nothing;
/// the Ensure functions stay idempotent.
async fn assert_reopen_is_noop(dsn: &str, before: &[Applied]) {
    let d = DB::open(dsn).await.expect("second Open");
    let c = raw_connect(dsn).await.expect("assertion client");
    let after = assert_embedded_applied(&c).await;
    for i in 0..before.len() {
        assert_eq!(
            before[i].applied_at, after[i].applied_at,
            "migration {:04} applied_at changed: {} -> {}",
            before[i].version, before[i].applied_at, after[i].applied_at
        );
    }
    for _ in 0..2 {
        d.ensure_llm_records_table()
            .await
            .expect("EnsureLLMRecordsTable");
        d.ensure_llm_usage_table()
            .await
            .expect("EnsureLLMUsageTable");
    }
    let got = read_applied(&c).await;
    assert_eq!(
        got.len(),
        before.len(),
        "Ensure functions changed schema_migrations: {got:?}"
    );
    drop(c);
    drop(d);
}

// --- TestLoadMigrationsValidation (pure, no database) -----------------------

#[test]
fn load_migrations_validation() {
    let files = vec![
        ("0010_b.sql".to_string(), "SELECT 10;".to_string()),
        ("0002_a.sql".to_string(), "SELECT 2;".to_string()),
        ("README.md".to_string(), "not a migration".to_string()),
    ];
    let ms = db::load_migrations(&files).expect("load");
    assert_eq!(ms.len(), 2, "README.md must be skipped");
    assert_eq!(ms[0].version, 2);
    assert_eq!(ms[1].version, 10);
    assert_eq!(ms[0].checksum, sha256_hex(b"SELECT 2;"));
    assert_eq!(ms[0].name, "0002_a.sql");

    // A `.sql` file whose name is not NNNN_slug.sql is an error.
    let bad = vec![("2_a.sql".to_string(), "SELECT 1;".to_string())];
    assert!(db::load_migrations(&bad).is_err(), "2_a.sql should error");

    // Two files claiming the same version is an error.
    let dup = vec![
        ("0001_a.sql".to_string(), "SELECT 1;".to_string()),
        ("0001_b.sql".to_string(), "SELECT 1;".to_string()),
    ];
    assert!(
        db::load_migrations(&dup).is_err(),
        "duplicate version should error"
    );
}

// --- DB-backed tests --------------------------------------------------------

#[tokio::test]
async fn open_records_embedded_migrations() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let d = match DB::open(&dsn).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            return;
        }
    };
    let c = raw_connect(&dsn).await.expect("assertion client");
    let applied = assert_embedded_applied(&c).await;
    drop(c);
    drop(d);
    assert_reopen_is_noop(&dsn, &applied).await;
}

#[tokio::test]
async fn open_migrates_fresh_database() {
    let Some(base) = base_dsn() else { return };
    let _g = DB_LOCK.lock().await;
    let Some((dsn, name)) = scratch(&base, "fresh").await else {
        eprintln!("skipping: postgres unavailable");
        return;
    };
    let d = DB::open(&dsn).await.expect("Open fresh");
    let c = raw_connect(&dsn).await.expect("assertion client");
    let applied = assert_embedded_applied(&c).await;
    drop(c);
    drop(d);
    assert_reopen_is_noop(&dsn, &applied).await;
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn open_adopts_existing_tables_without_schema_migrations() {
    let Some(base) = base_dsn() else { return };
    let _g = DB_LOCK.lock().await;
    let Some((dsn, name)) = scratch(&base, "legacy").await else {
        return;
    };
    let d = DB::open_with_migrations(&dsn, vec![])
        .await
        .expect("open without migrations");
    d.ensure_llm_records_table()
        .await
        .expect("EnsureLLMRecordsTable");
    d.ensure_llm_usage_table()
        .await
        .expect("EnsureLLMUsageTable");
    {
        let c = raw_connect(&dsn).await.expect("assertion client");
        for q in [
            "ALTER TABLE llm_records DROP COLUMN raw_request, DROP COLUMN raw_response",
            "INSERT INTO llm_records(model) VALUES ('legacy')",
            "INSERT INTO llm_usage(model, status) VALUES ('legacy', 'ok')",
            "DROP TABLE schema_migrations",
        ] {
            c.batch_execute(q)
                .await
                .unwrap_or_else(|e| panic!("{q}: {e}"));
        }
    }
    drop(d);

    let d = DB::open(&dsn).await.expect("Open on existing tables");
    let c = raw_connect(&dsn).await.expect("assertion client");
    let applied = assert_embedded_applied(&c).await;
    let records: i64 = c
        .query_one("SELECT count(*) FROM llm_records WHERE model='legacy'", &[])
        .await
        .unwrap()
        .get(0);
    let usage: i64 = c
        .query_one("SELECT count(*) FROM llm_usage WHERE model='legacy'", &[])
        .await
        .unwrap()
        .get(0);
    let raw_cols: i64 = c
        .query_one(
            "SELECT count(*) FROM information_schema.columns WHERE table_name='llm_records' AND column_name IN ('raw_request','raw_response')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(records, 1, "llm_records legacy rows");
    assert_eq!(usage, 1, "llm_usage legacy rows");
    assert_eq!(raw_cols, 2, "raw columns");
    drop(c);
    drop(d);
    assert_reopen_is_noop(&dsn, &applied).await;
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn migrations_apply_new_version_once_in_order() {
    let Some(base) = base_dsn() else { return };
    let _g = DB_LOCK.lock().await;
    let Some((dsn, name)) = scratch(&base, "order").await else {
        return;
    };
    let files = with_embedded(&[("0003_probe.sql", PROBE)]);
    let migrations = db::load_migrations(&files).unwrap();
    for pass in 1..=2 {
        let d = DB::open_with_migrations(&dsn, migrations.clone())
            .await
            .unwrap_or_else(|e| panic!("pass {pass}: {e}"));
        let c = raw_connect(&dsn).await.expect("assertion client");
        let applied = read_applied(&c).await;
        assert_eq!(
            applied.len(),
            3,
            "pass {pass}: schema_migrations = {applied:?}"
        );
        assert_eq!(applied[2].version, 3);
        assert_eq!(applied[2].name, "0003_probe.sql");
        let row = c
            .query_one(
                "SELECT count(*)::bigint, max(applied_before)::bigint FROM migration_probe",
                &[],
            )
            .await
            .unwrap();
        let runs: i64 = row.get(0);
        let before: i64 = row.get(1);
        assert_eq!(runs, 1, "pass {pass}: probe ran {runs} times, want 1");
        assert_eq!(
            before, 2,
            "pass {pass}: probe saw {before} migrations before it, want 2"
        );
        drop(c);
        drop(d);
    }
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn migrations_fail_closed() {
    let Some(base) = base_dsn() else { return };
    let _g = DB_LOCK.lock().await;
    let Some((dsn, name)) = scratch(&base, "closed").await else {
        return;
    };
    {
        let files = with_embedded(&[("0003_probe.sql", PROBE)]);
        let d = DB::open_with_migrations(&dsn, db::load_migrations(&files).unwrap())
            .await
            .expect("seed open");
        drop(d);
    }

    let probe_edited = format!("-- edited\n{PROBE}");
    struct Case {
        name: &'static str,
        files: Vec<(String, String)>,
        want: Vec<&'static str>,
    }
    let cases = vec![
        Case {
            name: "checksum changed",
            files: with_embedded(&[("0003_probe.sql", probe_edited.as_str())]),
            want: vec!["0003", "0003_probe.sql", "checksum"],
        },
        Case {
            name: "version missing from binary",
            files: with_embedded(&[]),
            want: vec!["0003", "0003_probe.sql", "missing from this binary"],
        },
        Case {
            name: "name changed",
            files: with_embedded(&[("0003_renamed.sql", PROBE)]),
            want: vec!["0003", "0003_probe.sql", "0003_renamed.sql"],
        },
    ];
    for c in cases {
        let err = DB::open_with_migrations(&dsn, db::load_migrations(&c.files).unwrap()).await;
        match err {
            Ok(_) => panic!("{}: Open succeeded, want error", c.name),
            Err(e) => {
                let msg = e.to_string();
                for w in &c.want {
                    assert!(
                        msg.contains(w),
                        "{}: error {:?} does not mention {:?}",
                        c.name,
                        msg,
                        w
                    );
                }
            }
        }
    }
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn failing_migration_rolls_back_and_blocks_open() {
    let Some(base) = base_dsn() else { return };
    let _g = DB_LOCK.lock().await;
    let Some((dsn, name)) = scratch(&base, "fail").await else {
        return;
    };
    let files = with_embedded(&[("0003_probe.sql", PROBE), ("0004_broken.sql", BROKEN)]);
    let err = match DB::open_with_migrations(&dsn, db::load_migrations(&files).unwrap()).await {
        Ok(_) => panic!("Open succeeded with a failing migration"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("0004"), "error {msg:?} does not name 0004");
    assert!(
        msg.contains("0004_broken.sql"),
        "error {msg:?} does not name 0004_broken.sql"
    );

    // Inspect with the migrations that succeeded: 1..3 stay applied, 0004 left
    // nothing behind.
    let files2 = with_embedded(&[("0003_probe.sql", PROBE)]);
    let d = DB::open_with_migrations(&dsn, db::load_migrations(&files2).unwrap())
        .await
        .expect("reopen");
    let c = raw_connect(&dsn).await.expect("assertion client");
    let applied = read_applied(&c).await;
    assert_eq!(
        applied.len(),
        3,
        "schema_migrations = {applied:?}, want 1..3"
    );
    assert_eq!(applied[2].version, 3);
    let row = c
        .query_one("SELECT to_regclass('migration_broken') IS NOT NULL", &[])
        .await
        .unwrap();
    let broken: bool = row.get(0);
    assert!(
        !broken,
        "migration_broken exists: the failed migration was not rolled back"
    );
    drop(c);
    drop(d);
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn migrations_refuse_out_of_order_version() {
    let Some(base) = base_dsn() else { return };
    let _g = DB_LOCK.lock().await;
    let Some((dsn, name)) = scratch(&base, "outoforder").await else {
        return;
    };
    let records = db::embedded_migrations()
        .into_iter()
        .find(|(n, _)| *n == "0001_llm_records.sql")
        .map(|(_, s)| s.to_string())
        .unwrap();
    let setup_files = vec![
        ("0001_llm_records.sql".to_string(), records),
        ("0003_probe.sql".to_string(), PROBE.to_string()),
    ];
    let d = DB::open_with_migrations(&dsn, db::load_migrations(&setup_files).unwrap())
        .await
        .expect("setup open");
    let c = raw_connect(&dsn).await.expect("assertion client");
    let before = read_applied(&c).await;
    drop(c);
    drop(d);
    assert_eq!(
        before.len(),
        2,
        "setup schema_migrations = {before:?}, want 1 and 3"
    );
    assert_eq!(before[0].version, 1);
    assert_eq!(before[1].version, 3);

    // 0002 pending below applied 0003 → refuse before writing anything.
    let files = with_embedded(&[("0003_probe.sql", PROBE)]);
    let err = match DB::open_with_migrations(&dsn, db::load_migrations(&files).unwrap()).await {
        Ok(_) => panic!("Open succeeded with 0002 pending below applied 0003"),
        Err(e) => e,
    };
    let msg = err.to_string();
    for w in [
        "0002_llm_usage.sql",
        "(0002)",
        "0003_probe.sql",
        "(0003)",
        "never inserted",
    ] {
        assert!(msg.contains(w), "error {msg:?} does not mention {w:?}");
    }

    let c = raw_connect(&dsn).await.expect("assertion client");
    let after = read_applied(&c).await;
    assert_eq!(
        after.len(),
        before.len(),
        "schema_migrations changed: before {before:?}, after {after:?}"
    );
    for i in 0..before.len() {
        assert_eq!(after[i].version, before[i].version);
        assert_eq!(after[i].checksum, before[i].checksum);
        assert_eq!(after[i].applied_at, before[i].applied_at);
    }
    let row = c
        .query_one("SELECT to_regclass('llm_usage') IS NOT NULL", &[])
        .await
        .unwrap();
    let usage: bool = row.get(0);
    assert!(!usage, "llm_usage exists: the refused 0002 migration ran");
    drop(c);
    drop_database(&base, &name).await;
}
