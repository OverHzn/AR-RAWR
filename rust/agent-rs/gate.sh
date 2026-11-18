#!/usr/bin/env bash
# Reproduce the agent-rs gate.
#
# `agent/` exposes several functions whose generated bridge needs two small
# post-generation fixes that `rust-anything bridge`/`refresh` does not emit:
#   - go/agent.go: the copied `Config` struct references `llm.Format` and
#     `RetryConfig`, packages the throwaway driver does not import.
#   - src/lib.rs: the zero-arg `BuiltinPromptSeeds` wire shim leaves an unused
#     reader, which `-D warnings` rejects.
# patch-generated.py re-applies both (idempotently) so the gate reproduces from a
# clean bridge. The default path assumes `bridge` has already been run; pass
# --bridge to (re)generate go/, src/lib.rs and the dispatcher first.
set -euo pipefail
export PATH="$HOME/.cargo/bin:/usr/local/go/bin:$PATH"
cd "$(dirname "$0")"

if [ "${1:-}" = "--bridge" ]; then
  # Regenerate the Go bridge + Rust dispatcher from src/port.rs, then re-patch.
  # `bridge` takes <ROOT> <UNIT> and runs from the repo root.
  root="$(cd ../.. && pwd)"
  ( cd "$root" && rust-anything bridge . agent/ --force \
    --keep "SetToolAugment: installs a global ToolAugmentFunc hook (mutable global fn pointer)" \
    --keep "ToolAugment: returns the global ToolAugmentFunc hook" \
    --keep "SetToolResolve: installs a global ToolResolveFunc hook (mutable global fn pointer)" \
    --keep "ToolResolve: returns the global ToolResolveFunc hook" \
    --keep "BudgetsFromEnv: reads ARTEX_*_BUDGET_* env vars" \
    --keep "FromEnv: reads ARTEX_LLM_* / ANTHROPIC_API_KEY / OPENAI_API_KEY env vars" \
    --keep "BuiltinToolSeeds: constructs actool.CoreTool values (heavy SDK types)" )
fi

python3 patch-generated.py

if [ "${1:-}" = "--record" ]; then
  rust-anything record .
fi

rust-anything build .
rust-anything diff .
rust-anything gate .