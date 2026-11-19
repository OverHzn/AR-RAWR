#!/usr/bin/env bash
# Gate for the `artex` application crate (the full Rust rewrite). Unlike the
# `*-rs` oracle-parity crates, `artex` is verified by porting the Go `_test.go`
# specs into `tests/` — fmt + clippy + test, all offline.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:/usr/local/go/bin:$PATH"

echo "== cargo fmt --check =="
cargo fmt --check

echo "== cargo clippy -D warnings =="
cargo clippy --offline --all-targets -- -D warnings

echo "== cargo test =="
cargo test --offline

echo
echo "Gate passed."