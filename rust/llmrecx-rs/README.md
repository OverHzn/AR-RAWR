# llmrecx-rs

Behavioral-parity Rust port of the unexported, side-effect-free helper in the Go
`llmrec/` package that is reachable without the external `norma/llm` types or
`context`, verified against the Go original via a recorded oracle (the bridge can't
reach unexported functions).

## Functions (1)

| Go | Signature | Notes |
|----|-----------|-------|
| `parseSession` (session.go) | `(string) (taskID, worker string)` | split `exp<N>-<role>[-suffix]`; no `exp` prefix → `("","")`; task id up to first `-`, worker up to the next `-` |

`parseSession` is pure and value-typed (only `strings.HasPrefix` / `strings.IndexByte`
in Go, `strip_prefix` / `find` in Rust). The oracle records the two return strings as
a 2-element JSON array.

The rest of `llmrec`'s unexported surface needs `context` (`workerFrom`, `reportStop`)
or the external `norma/llm.Message` (`thinkingText`) and stays in Go.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go llmrec/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`llmrec/` is not modified; `record.sh` cleans up its oracle.