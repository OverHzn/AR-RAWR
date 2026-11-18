//! The Rust port of `config/`.
//!
//! Faithful port of `config/config.go`. The Go stdlib pieces it leans on
//! (`net/url`, `path/filepath`, the `os`/`encoding/json` error shapes) live in
//! the shared `gocompat` crate; `rust-anything diff` checks behavior against what
//! the source recorded.

#![forbid(unsafe_code)]

use gocompat::filepath::{os_executable, path_abs, path_dir, path_join, path_rel, temp_dir};
use gocompat::gojson::{go_io_error, go_json_error};
use gocompat::neturl::{ENC_PATH, Userinfo, escape, url_parse, values_encode};
use gocompat::strings::go_trim_space as trim_space;
use serde_json::{Map, Value, json};
use std::env;
use std::fs;

/// Port of Go's `isGoRunDir` (`config/config.go:75`): is `dir` where `go run`
/// parked its throwaway executable — under `os.TempDir()`, or inside a Go build
/// cache (`…/go-build/…`). Config-only, so it lives with the port.
fn is_go_run_dir(dir: &str) -> bool {
    let tmp = temp_dir();
    if !tmp.is_empty()
        && let Ok(rel) = path_rel(&tmp, dir)
        && rel != ".."
        && !rel.starts_with("../")
    {
        return true;
    }
    for seg in dir.split('/') {
        if seg == "go-build" {
            return true;
        }
    }
    false
}

/// Port of `BaseDir` (`config/config.go:45`).
pub fn base_dir() -> Result<String, String> {
    let exe = match os_executable() {
        Ok(e) => e,
        Err(_) => return Ok(".".to_string()),
    };
    let dir = path_dir(&exe);
    if is_go_run_dir(&dir) {
        return Ok(".".to_string());
    }
    Ok(dir)
}

/// Port of `KeyDir` (`config/config.go:59`).
pub fn key_dir() -> Result<String, String> {
    let mut dir = trim_space(&env::var("ARTEX_JWT_KEY_DIR").unwrap_or_default()).to_string();
    if dir.is_empty() {
        dir = base_dir()?;
    }
    if let Ok(abs) = path_abs(&dir) {
        dir = abs;
    }
    Ok(dir)
}

/// Port of `Path` (`config/config.go:97`).
pub fn path() -> Result<String, String> {
    let v = trim_space(&env::var("ARTEX_CONFIG").unwrap_or_default()).to_string();
    if !v.is_empty() {
        return Ok(v);
    }
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        candidates.push(path_join(&[&cwd.to_string_lossy(), "config.json"]));
    }
    candidates.push(path_join(&[&base_dir()?, "config.json"]));
    for c in &candidates {
        if fs::metadata(c).is_ok() {
            return Ok(c.clone());
        }
    }
    Ok(candidates[0].clone())
}

/// Port of the Go `Config` struct, as the bridge's expected JSON object.
fn empty_config() -> Value {
    json!({
        "database": {
            "dsn": "", "host": "", "port": 0, "user": "",
            "password": "", "dbname": "", "sslmode": ""
        },
        "skill_dir": ""
    })
}

/// Port of `Load` (`config/config.go:117`).
pub fn load() -> Result<Value, String> {
    let path = path()?;
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(empty_config());
            }
            return Err(format!(
                "read config file {}: {}",
                path,
                go_io_error(&e, &path)
            ));
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "parse config file {}: {}",
                path,
                go_json_error(&text, &e)
            ));
        }
    };
    Ok(json_to_config(&value))
}

/// Map a parsed JSON value to the Config wire shape (missing/invalid → Go zero).
fn json_to_config(v: &Value) -> Value {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return empty_config(),
    };
    let db = obj.get("database").and_then(|d| d.as_object());
    let get_str = |m: &Map<String, Value>, k: &str| -> String {
        m.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let (dsn, host, user, password, dbname, sslmode) = match db {
        Some(d) => (
            get_str(d, "dsn"),
            get_str(d, "host"),
            get_str(d, "user"),
            get_str(d, "password"),
            get_str(d, "dbname"),
            get_str(d, "sslmode"),
        ),
        None => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ),
    };
    let port = match db {
        Some(d) => d.get("port").and_then(|x| x.as_i64()).unwrap_or(0),
        None => 0,
    };
    let skill_dir = get_str(obj, "skill_dir");
    json!({
        "database": {
            "dsn": dsn, "host": host, "port": port, "user": user,
            "password": password, "dbname": dbname, "sslmode": sslmode
        },
        "skill_dir": skill_dir
    })
}

/// Port of `SkillDir` (`config/config.go:138`).
pub fn skill_dir() -> Result<String, String> {
    let d;
    let env_skill = trim_space(&env::var("ARTEX_SKILL_DIR").unwrap_or_default()).to_string();
    if !env_skill.is_empty() {
        d = env_skill;
    } else {
        let c = load()?;
        let from_cfg = c
            .get("skill_dir")
            .and_then(|x| x.as_str())
            .map(|s| trim_space(s).to_string())
            .unwrap_or_default();
        if !from_cfg.is_empty() {
            d = from_cfg;
        } else {
            d = path_join(&[&base_dir()?, "skills"]);
        }
    }
    let _ = fs::create_dir_all(&d);
    Ok(d)
}

/// Port of `(*Database).buildDSN` (`config/config.go:183`).
fn build_dsn(db: &Database) -> String {
    let host = if db.host.is_empty() {
        "127.0.0.1".to_string()
    } else {
        db.host.clone()
    };
    let port = if db.port == 0 { 5432 } else { db.port };
    let ssl = if db.sslmode.is_empty() {
        "disable".to_string()
    } else {
        db.sslmode.clone()
    };

    let mut buf = String::new();
    buf.push_str("postgres://");
    match &db.user {
        u if !u.is_empty() => {
            if !db.password.is_empty() {
                let mut ui = Userinfo::user_password(u, &db.password);
                buf.push_str(&ui.escaped());
                ui.password.clear();
            } else {
                let ui = Userinfo::user(u);
                buf.push_str(&ui.escaped());
            }
            buf.push('@');
        }
        _ => {}
    }
    buf.push_str(&host);
    buf.push(':');
    buf.push_str(&port.to_string());
    buf.push('/');
    buf.push_str(&escape(&db.dbname, ENC_PATH));
    buf.push('?');
    buf.push_str(&values_encode(&[("sslmode", ssl.as_str())]));
    buf
}

#[derive(Default, Clone)]
struct Database {
    dsn: String,
    host: String,
    port: i64,
    user: String,
    password: String,
    dbname: String,
    sslmode: String,
}

fn db_from_config(c: &Value) -> Database {
    let db = c.get("database").and_then(|d| d.as_object());
    let get_str = |k: &str| -> String {
        db.and_then(|d| d.get(k))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    Database {
        dsn: get_str("dsn"),
        host: get_str("host"),
        port: db
            .and_then(|d| d.get("port"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        user: get_str("user"),
        password: get_str("password"),
        dbname: get_str("dbname"),
        sslmode: get_str("sslmode"),
    }
}

/// Port of `PostgresDSN` (`config/config.go:165`).
pub fn postgres_dsn() -> Result<Value, String> {
    let env_dsn = trim_space(&env::var("ARTEX_PG_DSN").unwrap_or_default()).to_string();
    if !env_dsn.is_empty() {
        return Ok(json!([
            env_dsn,
            "environment variable ARTEX_PG_DSN".to_string()
        ]));
    }
    let c = load()?;
    let db = db_from_config(&c);
    let dsn_field = trim_space(&db.dsn).to_string();
    if !dsn_field.is_empty() {
        let source = format!("config file {} (database.dsn)", path()?);
        return Ok(json!([dsn_field, source]));
    }
    if !db.host.is_empty() || !db.dbname.is_empty() || !db.user.is_empty() {
        let dsn = build_dsn(&db);
        let source = format!("config file {} (database fields)", path()?);
        return Ok(json!([dsn, source]));
    }
    Err(format!(
        "no database config found: the environment variable ARTEX_PG_DSN is not set, and config file {} provides no database (dsn or host/user/dbname). Create the config file or set the environment variable and retry",
        path()?
    ))
}

/// Port of `Redact` (`config/config.go:213`).
pub fn redact(dsn: String) -> Result<String, String> {
    let mut u = match url_parse(&dsn) {
        Ok(u) => u,
        Err(_) => return Ok(dsn),
    };
    if let Some(ui) = &u.user
        && ui.has_password()
    {
        let username = ui.username.clone();
        u.user = Some(Userinfo::user_password(&username, "****"));
    }
    Ok(u.to_url_string())
}
