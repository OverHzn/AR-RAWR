#!/usr/bin/env bash
# record.sh — regenerate cases.jsonl from the *real* Go norma/compaction pure functions.
#
# norma/compaction lives in the read-only Go module cache, so the oracle cannot be copied
# into the package the way the in-repo packages do. Instead oracle/ is a standalone Go
# module that imports norma/compaction (via a `replace` to github.com/BaaRai/Norma) and
# prints the recorded cases. go.sum is committed so the run is fully offline and
# reproducible. Only *exported* functions are reachable from this external module
# (EstimateTokens, IsOverflow); unexported helpers are exercised transitively.
#
# Usage: ./record.sh   (from the crate dir)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ORACLE="$HERE/oracle"

echo "recording cases from Go norma/compaction (standalone oracle module) ..."
( cd "$ORACLE" && GOWORK=off GOPROXY=off go test -run TestZZOracle -count=1 -v 2>/dev/null ) \
    | grep -E '^\{' | LC_ALL=C sort > "$HERE/cases.jsonl"

n=$(wc -l < "$HERE/cases.jsonl")
echo "wrote $n cases to $HERE/cases.jsonl"