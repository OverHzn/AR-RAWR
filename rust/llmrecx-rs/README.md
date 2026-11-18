# llmrecx-rs

Behavioral-parity Rust port of the unexported, side-effect-free helpers in the Go
`llmrec/` package, verified against the Go original via a recorded oracle (the bridge
can't reach unexported functions).

## Functions (2)

| Go | Signature | Notes |
|----|-----------|-------|
| `parseSession` (session.go) | `(string) (taskID, worker string)` | split `exp<N>-<role>[-suffix]`; no `exp` prefix → `("","")`; task id up to first `-`, worker up to the next `-` |
| `thinkingText` (llmrec.go) | `(llm.Message) string` | concat the `Thinking` of every thinking block whose `Thinking` is non-empty; text / tool blocks ignored |

`parseSession` is pure and value-typed (only `strings.HasPrefix` / `strings.IndexByte`
in Go, `strip_prefix` / `find` in Rust). The oracle records the two return strings as
a 2-element JSON array.

`thinkingText` operates on the `norma/llm` message model, now ported in the sibling
[`llm-rs`](../llm-rs) crate (`Message` / `ContentBlock`); this crate depends on it. The
harness passes the message as JSON (Go marshals a nil `Content` as `null`, which the
Rust `Message` deserializer tolerates).

The rest of `llmrec`'s unexported surface needs `context` (`workerFrom`, `reportStop`)
and stays in Go.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go llmrec/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`llmrec/` is not modified; `record.sh` cleans up its oracle.