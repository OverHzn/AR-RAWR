#!/usr/bin/env python3
"""Re-apply the bridge-template-gap fixes that `rust-anything bridge`/`refresh`
does not emit for `agent/`.

Both edits are on *generated* files, so a fresh `bridge`/`refresh` drops them.
This script is idempotent: running it on already-patched files is a no-op.

1. `go/agent.go` — the generator copies the `Config` struct verbatim, but its
   field types reference packages the throwaway driver module does not import:
     - `Format  llm.Format`  → the original's `llm.Format` is a string alias.
     - `Retry  RetryConfig`  → a struct the generator never emits.
   We rewrite the field to a plain `string` and inject a local `RetryConfig`
   (int + int64-nanosecond duration fields, matching agent/provider.go).

2. `src/lib.rs` — the wire shim for a zero-argument function (`BuiltinPromptSeeds`)
   still builds a `wire::Reader` it never reads, so `-D warnings` rejects the
   unused `r`. Replace that line with `let _ = input;` so the input is consumed.
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def patch_agent_go(text: str) -> str:
    if "type RetryConfig struct" in text and "Format  string" in text:
        return text  # already patched

    # Field type: llm.Format -> string.
    text = text.replace("\tFormat  llm.Format\n", "\tFormat  string\n")

    # Inject RetryConfig just before the Config struct, once.
    retry = (
        "// RetryConfig mirrors agent.RetryConfig (duration fields are int64 nanoseconds).\n"
        "type RetryConfig struct {\n"
        "\tConnectAttempts int\n"
        "\tConnectInterval int64\n"
        "\tEmptyAttempts   int\n"
        "\tEmptyInterval   int64\n"
        "\tStreamAttempts  int\n"
        "\tStreamInterval  int64\n"
        "}\n\n"
    )
    marker = "type Config struct {"
    if "type RetryConfig struct" not in text:
        text = text.replace(marker, retry + marker, 1)
    return text


def patch_lib_rs(text: str) -> str:
    needle = "        let r = &mut wire::Reader::new(input);\n"
    # Only the BuiltinPromptSeeds shim has no reader use after it; patch the one
    # whose body calls port::builtin_prompt_seeds().
    lines = text.splitlines(keepends=True)
    out = []
    for i, line in enumerate(lines):
        if line == needle:
            # Look ahead: the matching reader for BuiltinPromptSeeds is followed
            # (within a few lines) by the builtin_prompt_seeds call and never
            # uses `r`.
            window = "".join(lines[i + 1 : i + 5])
            if "port::builtin_prompt_seeds()" in window and "r." not in window:
                out.append("        let _ = input;\n")
                continue
        out.append(line)
    return "".join(out)


def main() -> int:
    changed = 0

    agent_go = ROOT / "go" / "agent.go"
    if agent_go.exists():
        before = agent_go.read_text()
        after = patch_agent_go(before)
        if after != before:
            agent_go.write_text(after)
            changed += 1
            print(f"patched {agent_go.relative_to(ROOT)}")

    lib_rs = ROOT / "src" / "lib.rs"
    if lib_rs.exists():
        before = lib_rs.read_text()
        after = patch_lib_rs(before)
        if after != before:
            lib_rs.write_text(after)
            changed += 1
            print(f"patched {lib_rs.relative_to(ROOT)}")

    print(f"patch-generated: {changed} file(s) changed")
    return 0


if __name__ == "__main__":
    sys.exit(main())