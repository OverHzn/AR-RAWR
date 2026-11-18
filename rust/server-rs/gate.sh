#!/usr/bin/env bash
# gate.sh — quality gate for server-rs (fmt, clippy, parity tests).
#
# Unlike rust-anything units there is no bridge/diff stage: parity is proven by
# tests/parity.rs replaying cases.jsonl (recorded by record.sh from the Go
# original).
#
# Usage: ./gate.sh   (from rust/server-rs)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"

echo "== fmt =="
cargo fmt --check

echo "== clippy =="
cargo clippy --offline --all-targets -- -D warnings

echo "== parity tests =="
cargo test --offline

echo "Gate passed."