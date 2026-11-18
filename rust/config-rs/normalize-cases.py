#!/usr/bin/env python3
"""Normalize record-time artifacts in cases.jsonl.

`BaseDir()` returns the directory of the running executable (Go's os.Executable),
so `rust-anything record` captures the Go driver's directory
(`.rust-anything/go-original`). The Rust driver runs from `.rust-anything/go-rust`.
That difference is a record-time artifact, not a behavior difference, so rewrite
the recorded expectation to the Rust driver's directory. `KeyDir` inherits it.

Idempotent: already-normalized cases are left unchanged.
"""
import json
import sys

ORIG = ".rust-anything/go-original"
RUST = ".rust-anything/go-rust"


def main(path: str) -> int:
    out = []
    n = 0
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if not line.strip():
                continue
            case = json.loads(line)
            ok = case.get("ok")
            if isinstance(ok, str) and ORIG in ok:
                case["ok"] = ok.replace(ORIG, RUST)
                n += 1
            out.append(json.dumps(case))
    with open(path, "w") as f:
        f.write("\n".join(out) + "\n")
    print(f"normalized {n} case(s) in {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "cases.jsonl"))