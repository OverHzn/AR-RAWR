//! `interceptx-rs` — behavioral-parity Rust port of the unexported, side-effect-free
//! helper functions in the Go `intercept/` package that the `intercept-rs` bridge
//! leaves behind: the argv-prefix arity table lookup, program-name normalisation,
//! the code-fence stripper, the byte/word token predicates, UTF-8-boundary
//! truncation, and the port/risk/action/message/label/subset helpers.
//!
//! These are unexported, so `rust-anything`'s bridge cannot reach them. Parity is
//! proven against the real Go functions via `record.sh` (a temporary oracle over a
//! corpus -> `cases.jsonl`) and `tests/parity.rs`.

#![forbid(unsafe_code)]

pub mod port;
