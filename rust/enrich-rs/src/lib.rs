//! `enrich-rs` — unexported, side-effect-free helpers of the Go `enrich/` package,
//! parity-tested against cases recorded from the Go original. Not reachable by
//! `rust-anything`'s bridge (unexported), so parity uses a Go oracle instead.

#![forbid(unsafe_code)]

pub mod port;
