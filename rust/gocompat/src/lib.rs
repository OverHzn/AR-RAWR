//! `gocompat` — faithful Rust ports of the Go standard-library pieces the ARTEX
//! rewrite relies on, shared across every unit crate.
//!
//! The ARTEX units (`config`, `report`, `traffic/rewrite`, and later `db`,
//! `server`, `agent`, `intercept`) lean on a handful of Go stdlib behaviors that
//! no Rust crate reproduces byte-for-byte — most importantly `net/url` (Go's RFC
//! 3986 relative-URL parsing and percent-encoding; the `url` crate is WHATWG and
//! rejects relative inputs), `path/filepath`, and the exact `os`/`encoding/json`
//! error-message shapes. Rather than re-implement them per unit, they live here
//! once. Each module is a 1:1 behavioral port, not a crate swap.
//!
//! Modules:
//! - [`neturl`]   — `net/url`: escaping, `URL` parse/`String`, `Values.Encode`
//! - [`filepath`] — `path/filepath` (Unix) + `os.TempDir`/`os.Executable`
//! - [`gojson`]   — `os`/`encoding/json` error-message shapes
//! - [`strings`]  — `strings.TrimSpace`/`TrimLeft`/`Fields`/`ToUpper`/…
//! - [`gotime`]   — `time.Time.Format`
//! - [`regexerr`] — `regexp` compile-error text (feature `regex`)
//! - [`encoding`] — `encoding/base64` (`StdEncoding.DecodeString`)

#![forbid(unsafe_code)]

pub mod encoding;
pub mod filepath;
pub mod gojson;
pub mod gocase;
pub mod gotime;
pub mod neturl;
pub mod strings;

#[cfg(feature = "regex")]
pub mod regexerr;