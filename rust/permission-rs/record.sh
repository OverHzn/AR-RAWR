#!/usr/bin/env bash
# record.sh — regenerate cases.jsonl from the *real* Go norma/permission pure decision logic.
#
# norma/permission lives in the read-only Go module cache, so the oracle cannot be copied
# into the package the way the in-repo packages do. Instead oracle/ is a standalone Go
# module that imports norma/permission (via a `replace` to github.com/BaaRai/Norma) and
# prints the recorded cases. go.sum is committed so the run is fully offline and
# reproducible. Only *exported* functions are reachable from this external module
# (Allowed, Denied, AskUser, Evaluate); unexported matchRule is exercised transitively.
#
# Usage: ./record.sh   (from the crate dir)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ORACLE="$HERE/oracle"

echo "recording cases from Go norma/permission (standalone oracle module) ..."
( cd "$ORACLE" && GOWORK=off GOPROXY=off go test -run TestZZOracle -count=1 -v 2>/dev/null ) \
    | grep -E '^\{' | LC_ALL=C sort > "$HERE/cases.jsonl"

n=$(wc -l < "$HERE/cases.jsonl")
echo "wrote $n cases to $HERE/cases.jsonl"