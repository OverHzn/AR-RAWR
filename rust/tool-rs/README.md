# tool-rs

Behavioral-parity Rust port of the **pure helpers of the external `norma/tool` package**
(`github.com/Autumn-27/norma/tool`), verified against the real Go package. `norma/tool` is
mostly IO-bound (bash / web fetch / grep / ripgrep / shell sessions); this crate ports only
its vendor-neutral, value-typed surface — the result constructors and the deferred-tools /
todo-list formatters.

## Why a standalone oracle module (with a full go.mod/go.sum)

`norma/tool` lives in the read-only Go module cache, so the oracle cannot be copied **into**
the package. Instead `oracle/` is a self-contained Go module that `import`s `norma/tool`
(via `replace github.com/Autumn-27/norma => github.com/BaaRai/Norma v0.4.11`) and prints the
cases. Unlike `llm-rs`/`compaction-rs` (whose packages only need the stdlib), importing
`norma/tool` pulls its full dependency set (e.g. `github.com/creack/pty`, `golang.org/x/net`),
so the oracle's `go.mod`/`go.sum` mirror the versions the main ARTEX module resolves to —
all present in the module cache, so `record.sh` runs fully offline and reproducibly
(`GOWORK=off GOPROXY=off`). A consequence: only **exported** functions are reachable here.

## Functions (4)

| Go | Signature | Notes |
|----|-----------|-------|
| `Text` (result.go) | `(string) Result` | success result with one text block (`IsError:false`) |
| `Errorf` (result.go) | `(string) Result` | error result with one text block (`IsError:true`) |
| `RenderDeferredToolsBlock` (deferred.go) | `([]string) string` | sorted `<available-deferred-tools>…` block + discovery/invocation instructions; empty for no names |
| `RenderTodoReminder` (todo.go) | `([]Todo) string` | numbered `N. [status] content` lines, trailing newlines trimmed; empty for no todos |

Ported types: `Result` (`Content`/`IsError`/`Extra`, PascalCase like Go's untagged struct —
`Extra` is `Option` so a nil slice renders `null`) and `Todo` (`content`/`status`), plus the
`SearchExtraTools`/`ExecuteExtraTool` and todo-status string constants. `Result`/`Todo`
reuse the `norma/llm` model (`ContentBlock`/`Message`) from the sibling
[`llm-rs`](../llm-rs) crate.

## Stays in Go

Every tool implementation and the IO helpers: `NewBash`/`NewRead`/`NewWrite`/`NewEdit`/
`NewGrep`/`NewGlob`/`NewWebFetch`/`NewWebSearch`, shell sessions, the `Registry`, `Capture`/
spill logic, `ValidateInput`, etc. — all touch the filesystem, network, processes, or
`context`. The four formatters above are the portable surface.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go norma/tool (deterministic, offline)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```