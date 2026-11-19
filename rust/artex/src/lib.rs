//! `artex` is the full Rust rewrite of the ARTEX autonomous pentest system.
//!
//! Go packages are ported here module-by-module, in dependency order (config,
//! evidence, llm, egress, guard, enrich, report, intercept, traffic, notify, …,
//! db, agent, server). Pure/deterministic helpers are additionally proven by the
//! oracle-parity crates under `rust/*-rs`; this crate is the runnable application.

pub mod config;
