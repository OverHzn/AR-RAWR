# llmpoolx-rs

Behavioral-parity Rust port of the unexported, side-effect-free helpers in the Go
`llmpool/` package, verified against the Go original via a recorded oracle (the bridge
can't reach unexported functions).

## Functions (3)

| Go | Signature | Notes |
|----|-----------|-------|
| `trimErr` (pool.go) | `(error) string` | newlines → spaces, trim, then if >300 bytes keep first 300 bytes + `…` |
| `blocksLen` (pool.go) | `([]llm.ContentBlock) int` | Σ `len(Text)+len(Thinking)+len(Input)+len(Name)`, recursive into nested `Content` |
| `estimateTokens` (pool.go) | `(llm.CompletionRequest) int` | `len(System)+blocksLen(Messages)+Σ(len(Name)+len(Description)+120*#props)`, then `*2/7` |

`trimErr` is pure and value-typed; the harness passes `err.Error()` as the input
string. Trimming uses `gocompat::strings::go_trim_space`. The 300-byte cap slices
bytes (Go `s[:300]`), which can split a UTF-8 rune; the port mirrors Go's lossy
rendering with `from_utf8_lossy` (exact for the ASCII corpus, where truncation is
exercised).

`blocksLen` / `estimateTokens` operate on the `norma/llm` message model, now ported in
the sibling [`llm-rs`](../llm-rs) crate (`ContentBlock`, `Message`, `ToolSchema`,
`CompletionRequest`); this crate depends on it. `len(Input)` is Go's raw-JSON byte
length — the harness keeps tool inputs compact and ASCII so the Rust side reproduces it
by re-serializing the decoded value. `len(InputSchema)` is the top-level key count of
the decoded map (nil → 0). The whole thing is integer math, matching Go `int`.

The rest of `llmpool`'s unexported surface needs pointer types (`rotateGroups` over
`[]*Member`), regexp (`statusOf`/`isHardFailure` via `statusRe`), or context
(`shouldFailover`), and stays in Go.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go llmpool/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`llmpool/` is not modified; `record.sh` cleans up its oracle.