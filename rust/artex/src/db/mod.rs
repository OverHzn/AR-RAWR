//! `db` — the PostgreSQL data source for ARTEX (ported from the Go `db` package).
//!
//! This module is ported package-by-package in dependency order. The foundational
//! layer lands first:
//!
//! - [`secrets`] — AES-256-GCM at-rest encryption for the secret columns/settings
//!   (`secrets.go`). Self-contained (no DB), proven by `TestSecretBoxCrypto`.
//!
//! Subsequent sub-units add the connection/open + schema + migrations
//! (`db.go`/`migrate.go`), the settings key-value store (`settings.go`), and the
//! builtin seeding (`db.go`'s `seedBuiltins`). Resource groups (tasks, findings,
//! assets, …) follow, each differential-tested against the live `artex-pg`
//! Postgres container.

pub mod secrets;
