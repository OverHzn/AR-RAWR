//! `sidequestion-rs` — unexported, side-effect-free helper of the Go
//! `sidequestion/` package (`Parent::key`), parity-tested against cases recorded
//! from the Go original. Not reachable by `rust-anything`'s bridge (unexported /
//! method on a value struct), so parity uses a Go oracle instead.

#![forbid(unsafe_code)]

pub mod port;
