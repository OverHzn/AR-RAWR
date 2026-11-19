#!/usr/bin/env bash
# record.sh — regenerate cases.jsonl from the *real* Go norma/transcript pure functions.
#
# norma/transcript lives in the read-only Go module cache, so the oracle cannot be copied
# into the package the way the in-repo packages do. Instead oracle/ is a standalone Go
# module that imports norma/transcript (via a `replace` to github.com/BaaRai/Norma) and
# prints the recorded cases. go.sum is committed so the run is fully offline and
# reproducible. Only *exported* functions are reachable from this external module
# (Messages, LastUUID).
#
# Usage: ./record.sh   (from the crate dir)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ORACLE="$HERE/oracle"

echo "recording cases from Go norma/transcript (standalone oracle module) ..."
( cd "$ORACLE" && GOWORK=off GOPROXY=off go test -run TestZZOracle -count=1 -v 2>/dev/null ) \
    | grep -E '^\{' | LC_ALL=C sort > "$HERE/cases.jsonl"

n=$(wc -l < "$HERE/cases.jsonl")
echo "wrote $n cases to $HERE/cases.jsonl"