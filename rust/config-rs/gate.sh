#!/usr/bin/env bash
# Reproduce the config-rs gate.
#
# `Load`/`SkillDir`/`PostgresDSN` take no arguments — they read the config file
# named by $ARTEX_CONFIG (then CWD, then BaseDir). To exercise their error paths
# (config/config.go:125 read-error, :145 skill-dir) we point ARTEX_CONFIG at a
# directory: os.ReadFile on a directory fails with "is a directory", which both
# the Go original and the Rust port reproduce identically.
#
# `cases.jsonl` is the committed Go-recorded ground truth (recorded with this same
# ARTEX_CONFIG). `BaseDir`/`KeyDir` embed the driver's own directory
# (.rust-anything/go-original vs go-rust); that is a record-time artifact, so the
# two cases are normalized go-original -> go-rust (see normalize-cases.py).
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")"

export ARTEX_CONFIG="$(cd fixtures/cfgdir && pwd)"

if [ "${1:-}" = "--record" ]; then
  # Re-record from the Go original (requires the Go toolchain), then normalize.
  rust-anything record .
  python3 normalize-cases.py
fi

rust-anything build .
rust-anything diff .
rust-anything gate .