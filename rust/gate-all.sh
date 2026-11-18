#!/usr/bin/env bash
# gate-all.sh — verify that EVERY unit crate under rust/ is gate-green, in one
# command. Each unit is run with whatever gate it defines:
#
#   - has ./gate.sh  -> run it (config-rs, agent-rs: env/patch wrappers around
#                       rust-anything; server-rs/traffic-rs/enrich-rs/guard-rs/
#                       sidequestion-rs: manual-parity crates)
#   - otherwise      -> ../ra gate .   (rust-anything units)
#
# gocompat (the shared stdlib-port crate) is build-checked first, since every unit
# depends on it.
#
# The go-shim (tools/go-shim) is put FIRST on PATH so units that transitively
# import the norma fork build; ra adds it again (harmless/idempotent), and the
# wrapper gate.sh scripts that call `rust-anything` directly also pick it up.
#
# Usage:
#   ./gate-all.sh            # incremental (reuses each crate's target/)
#   ./gate-all.sh --clean    # wipe every target/ first (full from-scratch rebuild)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"

export PATH="$here/tools/go-shim:$HOME/.cargo/bin:/usr/local/go/bin:$PATH"

clean=0
[ "${1:-}" = "--clean" ] && clean=1
if [ "$clean" = 1 ]; then
  echo "== cleaning target/ dirs =="
  find . -type d -name target -prune -exec rm -rf {} + 2>/dev/null
  echo
fi

logdir="$(mktemp -d)"
trap 'rm -rf "$logdir"' EXIT

pass=0
fail=0
failed=""

run_gate() {
  local name="$1"
  shift
  local log="$logdir/$name.log"
  printf '  %-16s ' "$name"
  if ( "$@" ) >"$log" 2>&1; then
    local line
    line="$(grep -hiE 'behavior +[0-9]+/[0-9]+|parity_all_cases \.\.\. ok|Gate passed' "$log" | tail -1)"
    printf 'PASS  %s\n' "$line"
    pass=$((pass + 1))
  else
    printf 'FAIL  (full log: %s)\n' "$log"
    tail -6 "$log" | sed 's/^/       | /'
    fail=$((fail + 1))
    failed="$failed $name"
  fi
}

echo "== gocompat (shared crate, build) =="
run_gate gocompat bash -c 'cd gocompat && cargo build --offline'
echo

echo "== unit crates =="
for d in *-rs; do
  [ -d "$d" ] || continue
  if [ -x "$d/gate.sh" ]; then
    run_gate "$d" bash -c "cd '$d' && ./gate.sh"
  else
    run_gate "$d" bash -c "cd '$d' && ../ra gate ."
  fi
done

echo
echo "======================================"
printf 'PASS: %d   FAIL: %d\n' "$pass" "$fail"
if [ "$fail" -ne 0 ]; then
  echo "FAILED units:$failed"
  exit 1
fi
echo "All units gate-green."