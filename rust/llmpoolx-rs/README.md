# llmpoolx-rs

Behavioral-parity Rust port of the unexported, side-effect-free helper in the Go
`llmpool/` package that is reachable without the heavy `norma/llm` types or regexp,
verified against the Go original via a recorded oracle (the bridge can't reach
unexported functions).

## Functions (1)

| Go | Signature | Notes |
|----|-----------|-------|
| `trimErr` (pool.go) | `(error) string` | newlines → spaces, trim, then if >300 bytes keep first 300 bytes + `…` |

`trimErr` is pure and value-typed; the harness passes `err.Error()` as the input
string. Trimming uses `gocompat::strings::go_trim_space`. The 300-byte cap slices
bytes (Go `s[:300]`), which can split a UTF-8 rune; the port mirrors Go's lossy
rendering with `from_utf8_lossy` (exact for the ASCII corpus, where truncation is
exercised).

The rest of `llmpool`'s unexported surface is pointer-based (`rotateGroups`), needs
regexp (`statusOf`/`isHardFailure` via `statusRe`), context (`shouldFailover`), or the
external `norma/llm` structs (`estimateTokens`, `blocksLen`), and stays in Go.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go llmpool/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`llmpool/` is not modified; `record.sh` cleans up its oracle.