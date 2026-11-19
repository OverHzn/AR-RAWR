//! Error type for the `db` module — port of the Go package's `error` returns.
//!
//! Go returns plain `error` values built with `fmt.Errorf`; the migration runner
//! builds human-readable messages whose substrings the Go tests assert on
//! (`missing from this binary`, `checksum`, `never inserted`, …). We keep the
//! same messages so the ported differential tests match the Go oracle byte for
//! byte. Driver / pool failures are converted from their native error types.

use std::fmt;

/// `db` error: a message string plus an optional wrapped cause.
#[derive(Debug, Clone)]
pub struct Error {
    msg: String,
}

impl Error {
    /// Build an error from a formatted message (mirrors `fmt.Errorf`).
    pub fn msg<S: Into<String>>(msg: S) -> Error {
        Error { msg: msg.into() }
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
        Error { msg: e.to_string() }
    }
}

impl From<deadpool_postgres::PoolError> for Error {
    fn from(e: deadpool_postgres::PoolError) -> Error {
        Error { msg: e.to_string() }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error { msg: e.to_string() }
    }
}

/// Convenience result alias used throughout the `db` module.
pub type Result<T> = std::result::Result<T, Error>;
