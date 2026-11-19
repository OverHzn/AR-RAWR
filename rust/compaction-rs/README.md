# compaction-rs

Behavioral-parity Rust port of the **pure functions of the external `norma/compaction`
package** (`github.com/Autumn-27/norma/compaction`), verified against the real Go package.
It ports the byte/char token estimator and the overflow classifier — the vendor-neutral,
value-typed surface — so the ARTEX packages built on top of it (`sidequestion.EstimateInputTokens`)
can be ported as thin layers over these types.

## Why a standalone oracle module

`norma/compaction` lives in the read-only Go module cache, so the oracle cannot be copied
**into** the package the way the in-repo packages do. Instead `oracle/` is a self-contained
Go module that `import`s `norma/compaction` (via `replace github.com/Autumn-27/norma =>
github.com/BaaRai/Norma v0.4.11`) and prints the cases. `oracle/go.sum` is committed, so
`record.sh` runs fully offline and reproducibly (`GOWORK=off GOPROXY=off`). A consequence:
only **exported** functions are reachable from this external module.

## Functions (2)

| Go | Signature | Notes |
|----|-----------|-------|
| `EstimateTokens` (compaction.go) | `([]llm.Message) int` | Σ `blockChars` over every content block, then `/4`. `blockChars`: text→`len(Text)`, thinking→`len(Thinking)`, tool_use→`len(Name)+len(Input)`, tool_result→Σ nested, default→`len(Text)` |
| `IsOverflow` (compaction.go) | `(error) bool` | `nil`→false; else case-insensitive substring match for `too long` / `context length` / `context_length` / `maximum context` / `status 413` |

Ported data type: none new — this crate reuses the `norma/llm` model from the sibling
[`llm-rs`](../llm-rs) crate (`Message`, `ContentBlock`, block-type tags). The private helper
`blockChars` is ported (exercised transitively through `EstimateTokens`); `len(Input)` is
Go's raw-JSON byte length, reproduced by re-serializing the decoded value (the harness keeps
tool inputs compact and ASCII). The `microClearedMessage` placeholder is exposed as
`MICRO_CLEARED_MESSAGE`.

## Stays in Go

The `Compactor` (its `Pre`/`Reactive`/`autoCompact`/`microCompact` methods need `context`,
a `Summarizer`, and mutable state), `IsOverflow`'s method wrapper, and the config/threshold
bookkeeping. The unexported helpers `isCleared`/`lastNNonMarkerStart`/`contentOnly` are pure
but **not reachable from the standalone external oracle** (unexported) and have no ARTEX
caller, so they are not ported; `EstimateTokens`/`IsOverflow` are the portable surface.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go norma/compaction (deterministic, offline)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```