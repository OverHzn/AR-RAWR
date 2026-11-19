//! Rust port of the Go `config/config_test.go` — the behavioral spec for the
//! `config` package. Env vars are process-global, so tests serialize on a mutex
//! and use unique temp dirs (mirroring Go's `t.Setenv` + `t.TempDir`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use artex::config;

static ENV_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Unique temp dir for one test (Go `t.TempDir()` equivalent).
fn temp_dir() -> PathBuf {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("artex-cfg-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn setenv(key: &str, val: &str) {
    std::env::set_var(key, val);
}

#[test]
fn postgres_dsn_precedence() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = temp_dir();
    let cfg_path = dir.join("config.json");
    std::fs::write(
        &cfg_path,
        br#"{"database":{"host":"10.1.2.3","port":6000,"user":"u","password":"p","dbname":"d","sslmode":"require"}}"#,
    )
    .unwrap();
    setenv("ARTEX_CONFIG", cfg_path.to_str().unwrap());

    // no env DSN -> built from config file fields
    setenv("ARTEX_PG_DSN", "");
    let (got, _src) = config::postgres_dsn().expect("from file");
    assert_eq!(got, "postgres://u:p@10.1.2.3:6000/d?sslmode=require");

    // env wins over config file
    setenv("ARTEX_PG_DSN", "postgres://envwins/x");
    let (got, _src) = config::postgres_dsn().expect("env wins");
    assert_eq!(got, "postgres://envwins/x");

    // no env, no file -> error (no built-in default)
    setenv("ARTEX_PG_DSN", "");
    setenv("ARTEX_CONFIG", dir.join("nope.json").to_str().unwrap());
    assert!(
        config::postgres_dsn().is_err(),
        "missing config should error"
    );

    // full dsn in config file is used verbatim
    std::fs::write(&cfg_path, br#"{"database":{"dsn":"postgres://full/dsn"}}"#).unwrap();
    setenv("ARTEX_CONFIG", cfg_path.to_str().unwrap());
    let (got, _src) = config::postgres_dsn().expect("file dsn verbatim");
    assert_eq!(got, "postgres://full/dsn");

    setenv("ARTEX_PG_DSN", "");
    setenv("ARTEX_CONFIG", "");
}

#[test]
fn malformed_config_is_an_error() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = temp_dir();
    let cfg_path = dir.join("config.json");
    // What install.sh used to write for a password containing a double quote.
    std::fs::write(
        &cfg_path,
        br#"{"database":{"host":"h","password":"a"b","dbname":"d"},"skill_dir":"/s"}"#,
    )
    .unwrap();
    setenv("ARTEX_CONFIG", cfg_path.to_str().unwrap());
    setenv("ARTEX_PG_DSN", "");
    setenv("ARTEX_SKILL_DIR", "");

    let e = config::postgres_dsn().expect_err("want parse error");
    assert!(
        e.to_string().contains("parse config file"),
        "PostgresDSN: got {e}"
    );
    assert!(config::skill_dir().is_err(), "SkillDir: want a parse error");
    assert!(config::load().is_err(), "Load: want a parse error");

    setenv("ARTEX_CONFIG", "");
    setenv("ARTEX_SKILL_DIR", "");
}
