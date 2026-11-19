//! Error type for the `db` module — port of the Go package's `error` returns.
//!
//! Go returns plain `error` values built with `fmt.Errorf`; the migration runner
//! builds human-readable messages whose substrings the Go tests assert on
//! (`missing from this binary`, `checksum`, `never inserted`, …). We keep the
//! same messages so the ported differential tests match the Go oracle byte for
//! byte. Driver / pool failures are converted from their native error types.
//!
//! On top of the bare message, an error may carry:
//!   * a **sentinel identity** (`sentinel`), standing in for one of the package's
//!     `var Err… = errors.New(…)` values so a caller can do the equivalent of
//!     Go's `errors.Is(err, Err…)` via [`Error::is`]; and
//!   * a **Postgres SQLSTATE** (`sqlstate`), captured when the error wraps a
//!     `tokio_postgres::Error`, so callers can distinguish a unique violation
//!     (`23505`) from a missing row without re-inspecting the driver error.

use std::fmt;

/// `db` error: a message string, plus an optional sentinel identity and an
/// optional Postgres SQLSTATE code.
#[derive(Debug, Clone)]
pub struct Error {
    msg: String,
    sentinel: Option<&'static str>,
    sqlstate: Option<String>,
}

impl Error {
    /// Build a plain error from a formatted message (mirrors `fmt.Errorf`).
    pub fn msg<S: Into<String>>(msg: S) -> Error {
        Error {
            msg: msg.into(),
            sentinel: None,
            sqlstate: None,
        }
    }

    /// Build a sentinel error identified by a stable `key` — the Rust analogue
    /// of a Go `var Err… = errors.New(msg)`. [`Error::is`] matches on `key`.
    pub fn sentinel(key: &'static str, msg: impl Into<String>) -> Error {
        Error {
            msg: msg.into(),
            sentinel: Some(key),
            sqlstate: None,
        }
    }

    /// `errors.Is(err, Err…)`: true when this error carries the sentinel `key`.
    pub fn is(&self, key: &str) -> bool {
        matches!(self.sentinel, Some(s) if s == key)
    }

    /// The Postgres SQLSTATE code, when this error wraps a driver error.
    pub fn sqlstate(&self) -> Option<&str> {
        self.sqlstate.as_deref()
    }

    /// True when the underlying Postgres error is a unique_violation (`23505`).
    pub fn is_unique_violation(&self) -> bool {
        self.sqlstate.as_deref() == Some("23505")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for Error {}

impl From<tokio_postgres::Error> for Error {
    fn from(e: tokio_postgres::Error) -> Error {
        let sqlstate = e.as_db_error().map(|d| d.code().code().to_string());
        Error {
            msg: e.to_string(),
            sentinel: None,
            sqlstate,
        }
    }
}

impl From<deadpool_postgres::PoolError> for Error {
    fn from(e: deadpool_postgres::PoolError) -> Error {
        Error::msg(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::msg(e.to_string())
    }
}

/// Convenience result alias used throughout the `db` module.
pub type Result<T> = std::result::Result<T, Error>;
