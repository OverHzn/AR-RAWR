#!/usr/bin/env bash
# gate.sh — quality gate for this manual-parity crate (fmt, clippy, parity tests).
# No bridge/diff stage: parity is tests/parity.rs replaying cases.jsonl (recorded
# by record.sh from the real Go norma/llm package).
#
# Usage: ./gate.sh   (from the crate dir)
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