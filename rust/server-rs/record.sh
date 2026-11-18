#!/usr/bin/env bash
# record.sh — regenerate cases.jsonl from the *real* Go server/ helpers.
#
# These helpers are unexported, so rust-anything's bridge cannot reach them. We
# record ground truth with a temporary Go oracle (oracle/zz_oracle_test.go.tmpl),
# copied into server/, run, then deleted — the package is left untouched.
#
# Usage: ./record.sh           (from rust/server-rs)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
ORACLE_TMPL="$HERE/oracle/zz_oracle_test.go.tmpl"
ORACLE_DST="$ROOT/server/zz_oracle_test.go"

cleanup() { rm -f "$ORACLE_DST"; }
trap cleanup EXIT

cp "$ORACLE_TMPL" "$ORACLE_DST"

echo "recording cases from Go server/ ..."
( cd "$ROOT" && GOWORK=off go test ./server/ -run TestZZOracle -count=1 -v 2>/dev/null ) \
    | grep -E '^\{' | LC_ALL=C sort > "$HERE/cases.jsonl"

n=$(wc -l < "$HERE/cases.jsonl")
echo "wrote $n cases to $HERE/cases.jsonl"