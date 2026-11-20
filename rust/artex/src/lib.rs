//! `artex` is the full Rust rewrite of the ARTEX autonomous pentest system.
//!
//! Go packages are ported here module-by-module, in dependency order (config,
//! llm model, egress, guard, enrich, report, intercept, traffic, notify, …, db,
//! agent, server). Pure/deterministic helpers are additionally proven by the
//! oracle-parity crates under `rust/*-rs`; this crate is the runnable application.

pub mod agent;
pub mod config;
pub mod db;
pub mod egress;
pub mod harness;
pub mod intercept;
pub mod llm;
pub mod llmpool;
pub mod llmrec;
pub mod mcphttp;
pub mod notify;
pub mod permission;
pub mod report;
pub mod sidequestion;
pub mod tool;
pub mod traffic;
