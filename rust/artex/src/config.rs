//! Rust port of the Go `config/` package: load runtime configuration from a JSON
//! file with environment variables taking precedence (currently the PostgreSQL
//! connection). Go-stdlib semantics (net/url, path/filepath, string trimming) come
//! from `gocompat` so DSN assembly and path resolution match byte-for-byte.

use std::fmt;

use gocompat::{filepath, neturl, strings};
use serde::{Deserialize, Serialize};

/// `Database` (`config/config.go`): the PostgreSQL connection config. Either set
/// `dsn` directly, or set the component fields and a DSN is assembled from them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Database {
    pub dsn: String,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub password: String,
    pub dbname: String,
    pub sslmode: String,
}

impl Database {
    /// `(*Database).buildDSN` (`config/config.go`): assemble a `postgres://` DSN
    /// from the component fields, defaulting host=127.0.0.1, port=5432,
    /// sslmode=disable. Faithful to Go's `url.URL{…}.String()`.
    pub fn build_dsn(&self) -> String {
        let host = if self.host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            self.host.clone()
        };
        let port = if self.port == 0 { 5432 } else { self.port };
        let ssl = if self.sslmode.is_empty() {
            "disable".to_string()
        } else {
            self.sslmode.clone()
        };
        let user = if self.user.is_empty() {
            None
        } else if !self.password.is_empty() {
            Some(neturl::Userinfo::user_password(&self.user, &self.password))
        } else {
            Some(neturl::Userinfo::user(&self.user))
        };
        let u = neturl::Url {
            scheme: "postgres".to_string(),
            // Go: host + ":" + strconv.Itoa(port) — NOT net.JoinHostPort (no IPv6 brackets).
            host: format!("{host}:{port}"),
            path: format!("/{}", self.dbname),
            user,
            raw_query: neturl::values_encode(&[("sslmode", &ssl)]),
            ..Default::default()
        };
        u.to_url_string()
    }
}

/// `Config` (`config/config.go`): the on-disk config file shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: Database,
    pub skill_dir: String,
}

/// Error loading configuration. `Display` matches the Go `fmt.Errorf` wrapping so
/// callers/tests can match on substrings such as `parse config file`.
#[derive(Debug)]
pub enum ConfigError {
    /// `fmt.Errorf("read config file %s: %w", path, err)`.
    Read {
        path: String,
        source: std::io::Error,
    },
    /// `fmt.Errorf("parse config file %s: %w", path, err)`.
    Parse {
        path: String,
        source: serde_json::Error,
    },
    /// A free-form message (e.g. the "no database config" error).
    Msg(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "read config file {path}: {source}")
            }
            ConfigError::Parse { path, source } => {
                write!(f, "parse config file {path}: {source}")
            }
            ConfigError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
            ConfigError::Msg(_) => None,
        }
    }
}

/// `BaseDir` (`config/config.go`): the directory anchoring all runtime artifacts —
/// the running binary's directory, or `.` for a throwaway `go run` binary.
pub fn base_dir() -> String {
    let exe = match filepath::os_executable() {
        Ok(e) => e,
        Err(_) => return ".".to_string(),
    };
    let dir = filepath::path_dir(&exe);
    if is_go_run_dir(&dir) {
        return ".".to_string();
    }
    dir
}

/// `isGoRunDir` (`config/config.go`): whether `dir` is under the system temp dir
/// or inside a Go build cache (the throwaway `go run` binary locations).
fn is_go_run_dir(dir: &str) -> bool {
    let tmp = filepath::temp_dir();
    if !tmp.is_empty() {
        if let Ok(rel) = filepath::path_rel(&tmp, dir) {
            let sep = std::path::MAIN_SEPARATOR;
            if rel != ".." && !rel.starts_with(&format!("..{sep}")) {
                return true;
            }
        }
    }
    for seg in dir.replace('\\', "/").split('/') {
        if seg == "go-build" {
            return true;
        }
    }
    false
}

/// `KeyDir` (`config/config.go`): directory holding jwt.key/secret.key —
/// `ARTEX_JWT_KEY_DIR`, or the base dir, made absolute when possible.
pub fn key_dir() -> String {
    let mut dir =
        strings::go_trim_space(&std::env::var("ARTEX_JWT_KEY_DIR").unwrap_or_default()).to_string();
    if dir.is_empty() {
        dir = base_dir();
    }
    if let Ok(abs) = filepath::path_abs(&dir) {
        dir = abs;
    }
    dir
}

/// `Path` (`config/config.go`): resolve the config file path — `ARTEX_CONFIG`
/// override, else `./config.json`, else `config.json` beside the executable. The
/// first existing file wins; if none exist, the CWD candidate is returned.
pub fn path() -> String {
    let v = strings::go_trim_space(&std::env::var("ARTEX_CONFIG").unwrap_or_default()).to_string();
    if !v.is_empty() {
        return v;
    }
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(filepath::path_join(&[
            &cwd.to_string_lossy(),
            "config.json",
        ]));
    }
    candidates.push(filepath::path_join(&[&base_dir(), "config.json"]));
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return c.clone();
        }
    }
    candidates[0].clone()
}

/// `Load` (`config/config.go`): read and parse the config file. A missing file
/// yields a zero `Config`; an existing-but-unreadable/unparseable file is an error.
pub fn load() -> Result<Config, ConfigError> {
    let path = path();
    let b = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(ConfigError::Read { path, source: e }),
    };
    serde_json::from_slice(&b).map_err(|e| ConfigError::Parse { path, source: e })
}

/// `SkillDir` (`config/config.go`): the skill root directory —
/// `ARTEX_SKILL_DIR` > config file `skill_dir` > `BaseDir()/skills`. Created if
/// it does not exist.
pub fn skill_dir() -> Result<String, ConfigError> {
    let v =
        strings::go_trim_space(&std::env::var("ARTEX_SKILL_DIR").unwrap_or_default()).to_string();
    let d = if !v.is_empty() {
        v
    } else {
        let c = load()?;
        let sv = strings::go_trim_space(&c.skill_dir).to_string();
        if !sv.is_empty() {
            sv
        } else {
            filepath::path_join(&[&base_dir(), "skills"])
        }
    };
    let _ = std::fs::create_dir_all(&d);
    Ok(d)
}

/// `PostgresDSN` (`config/config.go`): resolve the connection string —
/// `ARTEX_PG_DSN` > config file `database.dsn` > assembled from fields. No
/// built-in fallback: returns an error naming the inspected path when no database
/// config is found. `source` describes where the DSN came from (for logging).
pub fn postgres_dsn() -> Result<(String, String), ConfigError> {
    let v = strings::go_trim_space(&std::env::var("ARTEX_PG_DSN").unwrap_or_default()).to_string();
    if !v.is_empty() {
        return Ok((v, "environment variable ARTEX_PG_DSN".to_string()));
    }
    let c = load()?;
    let db = &c.database;
    let d = strings::go_trim_space(&db.dsn).to_string();
    if !d.is_empty() {
        return Ok((d, format!("config file {} (database.dsn)", path())));
    }
    if !db.host.is_empty() || !db.dbname.is_empty() || !db.user.is_empty() {
        return Ok((
            db.build_dsn(),
            format!("config file {} (database fields)", path()),
        ));
    }
    Err(ConfigError::Msg(format!(
        "no database config found: the environment variable ARTEX_PG_DSN is not set, and config file {} provides no database (dsn or host/user/dbname). Create the config file or set the environment variable and retry",
        path()
    )))
}

/// `Redact` (`config/config.go`): a redacted view of a DSN (password masked as
/// `****`) for logging. Returns the input unchanged if it does not parse.
pub fn redact(dsn: &str) -> String {
    let mut u = match neturl::parse_url(dsn) {
        Ok(u) => u,
        Err(_) => return dsn.to_string(),
    };
    if let Some(ui) = &u.user {
        if ui.has_password() {
            let name = ui.username.clone();
            u.user = Some(neturl::Userinfo::user_password(&name, "****"));
        }
    }
    u.to_url_string()
}
