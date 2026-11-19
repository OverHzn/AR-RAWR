//! `permission-rs` — Rust port of the *pure* decision logic of the external
//! `github.com/Autumn-27/norma/permission` package.
//!
//! `permission` is the agent's safety boundary: the `Evaluate` deny-priority pipeline
//! plus the `Allowed`/`Denied`/`AskUser` constructors and the `Decision`/`Update` value
//! model. The pipeline's only impure input is the host approval *callback*
//! (`Request.Ask`); this crate models that callback by its already-resolved return
//! value, making the whole pipeline a pure, deterministic function of value-typed
//! inputs. Parity is proven against the *real* Go `norma/permission` package via a
//! standalone oracle module (`oracle/`) that imports it and prints recorded cases.
//!
//! The package imports only the standard library, so the oracle has no heavy deps.

#![forbid(unsafe_code)]

pub mod port;
