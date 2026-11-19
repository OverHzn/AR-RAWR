//! Differential tests for the built-in seed boot step — port of `db/db_test.go`
//! (`TestOpenSeed`), `db/browser_mcp_test.go` (`TestSeedAndMigrateBrowserMCP`),
//! and the DB-bound half of `db/target_pins_test.go`
//! (`TestTargetGateExemptionSeedAndLog`, seed portion only).
//!
//! Each test carves out a per-test scratch database (`<db>_seed_<suffix>`),
//! dropped first to clear leftovers, serialized behind a shared async lock so
//! concurrent `CREATE`/`DROP DATABASE` never collide. Everything runs against
//! the live Postgres from `ARTEX_PG_DSN` (the same image the Go oracle uses) and
//! skips cleanly when it is unset or unreachable.
//!
//! The Go target-gate spec leans on the resource-group methods
//! (`TargetExemptions`/`ReplaceTargetExemptions`/`ListTargetExemptionLog`), which
//! are not ported yet; this port drives `seedTargetGateExemptions` through a
//! fresh `Open` and asserts the seeded settings row directly.

use artex::db::{self, DB};
use tokio_postgres::{Client, NoTls};

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn qi(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

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

/// Drop any leftover `<db>_seed_<suffix>` and return the scratch DSN.
async fn scratch(base: &str, suffix: &str) -> Option<String> {
    let base_cfg: tokio_postgres::Config = base.parse().ok()?;
    let name = format!("{}_seed_{}", base_cfg.get_dbname().unwrap_or(""), suffix);
    let admin = connect(admin_config(base)?).await?;
    admin
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            qi(&name)
        ))
        .await
        .ok()?;
    drop(admin);
    db::suffix_test_database(base, &format!("_seed_{}", suffix)).ok()
}

async fn drop_database(base: &str, suffix: &str) {
    let base_cfg: tokio_postgres::Config = match base.parse() {
        Ok(c) => c,
        Err(_) => return,
    };
    let name = format!("{}_seed_{}", base_cfg.get_dbname().unwrap_or(""), suffix);
    if let Some(cfg) = admin_config(base) {
        if let Some(admin) = connect(cfg).await {
            let _ = admin
                .batch_execute(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    qi(&name)
                ))
                .await;
        }
    }
}

/// Open a fresh scratch DB (running the full boot: schema + migrations + seed).
async fn open_scratch(base: &str, suffix: &str) -> Option<DB> {
    let dsn = scratch(base, suffix).await?;
    DB::open(&dsn).await.ok()
}

/// Read `(command, args)` of the `browser` MCP row.
async fn read_browser_mcp(db: &DB) -> (String, Vec<String>) {
    let conn = db.pool.get().await.expect("pool");
    let row = conn
        .query_one(
            "SELECT COALESCE(command,''), args FROM mcp_servers WHERE name='browser'",
            &[],
        )
        .await
        .expect("browser row");
    let command: String = row.get(0);
    let args: serde_json::Value = row.get(1);
    let args = args
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    (command, args)
}

/// Overwrite the `browser` MCP row's command + args.
async fn put_browser_mcp(db: &DB, command: &str, args: &[&str]) {
    let conn = db.pool.get().await.expect("pool");
    let raw = serde_json::Value::from(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let n = conn
        .execute(
            "UPDATE mcp_servers SET command=$1, args=$2 WHERE name='browser'",
            &[&command, &raw],
        )
        .await
        .expect("put browser mcp");
    assert_eq!(n, 1, "browser mcp row missing");
}

async fn clear_direct_bin_setting(db: &DB) {
    let conn = db.pool.get().await.expect("pool");
    let _ = conn
        .execute(
            "DELETE FROM settings WHERE key='browser_mcp_direct_bin_v1'",
            &[],
        )
        .await;
}

fn sv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// --- db_test.go TestOpenSeed -------------------------------------------------

#[tokio::test]
async fn open_seed_inserts_builtins_and_is_idempotent() {
    let Some(base) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(db) = open_scratch(&base, "openseed").await else {
        eprintln!("skipping: postgres unavailable");
        return;
    };

    // The number of built-in agents (goals/planner/mainagent/worker/auto/pentest).
    let agents: i64 = db
        .query_one("SELECT count(*) FROM agents WHERE builtin", &[])
        .await
        .expect("count agents")
        .get(0);
    assert_eq!(agents, 6, "want 6 builtin agents, got {agents}");

    // planner has at least its two seeded catalog vars.
    let planner_vars: i64 = db
        .query_one(
            "SELECT count(*) FROM agent_prompt_vars v JOIN agents a ON a.id=v.agent_id WHERE a.key='planner'",
            &[],
        )
        .await
        .expect("count planner vars")
        .get(0);
    assert!(
        planner_vars >= 2,
        "want at least 2 planner vars, got {planner_vars}"
    );

    // Re-open is idempotent: still exactly 6 builtin agents.
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_openseed").unwrap())
        .await
        .expect("reopen");
    let agents: i64 = db
        .query_one("SELECT count(*) FROM agents WHERE builtin", &[])
        .await
        .expect("count agents")
        .get(0);
    assert_eq!(agents, 6, "after re-open want 6 agents, got {agents}");

    drop_database(&base, "openseed").await;
}

// --- browser_mcp_test.go TestSeedAndMigrateBrowserMCP ------------------------

#[tokio::test]
async fn seed_and_migrate_browser_mcp() {
    let Some(base) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(db) = open_scratch(&base, "browsermcp").await else {
        eprintln!("skipping: postgres unavailable");
        return;
    };

    // "stock npx rewritten once", then not clobbered on the second boot.
    put_browser_mcp(&db, "npx", &["@playwright/mcp", "--headless"]).await;
    clear_direct_bin_setting(&db).await;
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_browsermcp").unwrap())
        .await
        .expect("reopen 1");
    let (cmd, args) = read_browser_mcp(&db).await;
    assert_eq!(cmd, "playwright-mcp");
    assert_eq!(args, sv(&["--headless", "--browser", "chromium"]));

    // A second boot with the row put back to npx must NOT rewrite it (flag done).
    put_browser_mcp(&db, "npx", &["@playwright/mcp", "--headless"]).await;
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_browsermcp").unwrap())
        .await
        .expect("reopen 2");
    let (cmd, args) = read_browser_mcp(&db).await;
    assert_eq!(cmd, "npx", "second pass clobbered command");
    assert_eq!(args, sv(&["@playwright/mcp", "--headless"]));

    // npx -y rewritten.
    put_browser_mcp(&db, "npx", &["-y", "@playwright/mcp", "--headless"]).await;
    clear_direct_bin_setting(&db).await;
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_browsermcp").unwrap())
        .await
        .expect("reopen 3");
    let (cmd, args) = read_browser_mcp(&db).await;
    assert_eq!(cmd, "playwright-mcp");
    assert_eq!(args, sv(&["--headless", "--browser", "chromium"]));

    // playwright-mcp missing --browser gets it added.
    put_browser_mcp(&db, "playwright-mcp", &["--headless"]).await;
    clear_direct_bin_setting(&db).await;
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_browsermcp").unwrap())
        .await
        .expect("reopen 4");
    let (cmd, args) = read_browser_mcp(&db).await;
    assert_eq!(cmd, "playwright-mcp");
    assert_eq!(args, sv(&["--headless", "--browser", "chromium"]));

    // A custom package is left alone.
    put_browser_mcp(&db, "npx", &["@modelcontextprotocol/server-github"]).await;
    clear_direct_bin_setting(&db).await;
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_browsermcp").unwrap())
        .await
        .expect("reopen 5");
    let (cmd, args) = read_browser_mcp(&db).await;
    assert_eq!(cmd, "npx", "custom row command rewritten");
    assert_eq!(args, sv(&["@modelcontextprotocol/server-github"]));

    // Proxy args preserved through the npx→binary rewrite.
    put_browser_mcp(
        &db,
        "npx",
        &[
            "@playwright/mcp",
            "--headless",
            "--proxy-server",
            "http://127.0.0.1:8788",
        ],
    )
    .await;
    clear_direct_bin_setting(&db).await;
    drop(db);
    let db = DB::open(&db::suffix_test_database(&base, "_seed_browsermcp").unwrap())
        .await
        .expect("reopen 6");
    let (cmd, args) = read_browser_mcp(&db).await;
    assert_eq!(cmd, "playwright-mcp");
    assert_eq!(
        args,
        sv(&[
            "--headless",
            "--proxy-server",
            "http://127.0.0.1:8788",
            "--browser",
            "chromium",
        ])
    );

    drop_database(&base, "browsermcp").await;
}

// --- target_pins_test.go TestTargetGateExemptionSeedAndLog (seed portion) ----

#[tokio::test]
async fn target_gate_exemptions_seeded_once() {
    let Some(base) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(db) = open_scratch(&base, "gateexempt").await else {
        eprintln!("skipping: postgres unavailable");
        return;
    };

    let conn = db.pool.get().await.expect("pool");
    let raw: String = conn
        .query_one(
            "SELECT value FROM settings WHERE key='target_gate_exempt_hosts'",
            &[],
        )
        .await
        .expect("exempt setting present after seed")
        .get(0);
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("exempt json");
    let want = serde_json::json!([
        {"host":"pypi.org","port":443,"note":"[built-in] Python package index"},
        {"host":"files.pythonhosted.org","port":443,"note":"[built-in] Python package files"},
        {"host":"registry.npmjs.org","port":443,"note":"[built-in] npm registry"},
        {"host":"deb.debian.org","port":80,"note":"[built-in] Debian packages"},
        {"host":"deb.debian.org","port":443,"note":"[built-in] Debian packages"},
    ]);
    assert_eq!(parsed, want, "seeded exemption list mismatch");

    // The gate flag is set so a second boot does not re-seed over an operator
    // list. Replace the list, re-open, and confirm it is left untouched.
    let replacement =
        serde_json::json!([{"host":"mirror.example.org","port":443,"note":"internal mirror"}])
            .to_string();
    conn.execute(
        "UPDATE settings SET value=$1 WHERE key='target_gate_exempt_hosts'",
        &[&replacement],
    )
    .await
    .expect("replace exemptions");
    drop(conn);
    drop(db);

    let db = DB::open(&db::suffix_test_database(&base, "_seed_gateexempt").unwrap())
        .await
        .expect("reopen");
    let conn = db.pool.get().await.expect("pool");
    let raw: String = conn
        .query_one(
            "SELECT value FROM settings WHERE key='target_gate_exempt_hosts'",
            &[],
        )
        .await
        .expect("exempt setting")
        .get(0);
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(
        parsed,
        serde_json::json!([{"host":"mirror.example.org","port":443,"note":"internal mirror"}]),
        "second boot re-seeded over the operator list"
    );

    drop_database(&base, "gateexempt").await;
}
