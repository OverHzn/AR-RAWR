#!/usr/bin/env bash
# record.sh — regenerate cases.jsonl from the *real* Go mcphttp/ helpers.
#
# The helpers are unexported, so rust-anything's bridge cannot reach them. We
# record ground truth with a temporary Go oracle (oracle/zz_oracle_test.go.tmpl),
# copied into mcphttp/, run under `go test`, then deleted — the package is left
# untouched. Output is sorted, so re-running is byte-reproducible.
#
# The crate is named mcphttpx-rs but the Go package is `mcphttp`, so PKG is set
# explicitly (the basename heuristic would yield `mcphttpx`).
#
# Usage: ./record.sh   (from the crate dir)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PKG="mcphttp"
ORACLE_TMPL="$HERE/oracle/zz_oracle_test.go.tmpl"
ORACLE_DST="$ROOT/$PKG/zz_oracle_test.go"

cleanup() { rm -f "$ORACLE_DST"; }
trap cleanup EXIT

cp "$ORACLE_TMPL" "$ORACLE_DST"

echo "recording cases from Go $PKG/ ..."
( cd "$ROOT" && GOWORK=off go test "./$PKG/" -run TestZZOracle -count=1 -v 2>/dev/null ) \
    | grep -E '^\{' | LC_ALL=C sort > "$HERE/cases.jsonl"

n=$(wc -l < "$HERE/cases.jsonl")
echo "wrote $n cases to $HERE/cases.jsonl"