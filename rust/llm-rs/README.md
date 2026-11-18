# llm-rs

Behavioral-parity Rust port of the **pure functions of the external `norma/llm` message
model** (`github.com/Autumn-27/norma/llm`), verified against the real Go package. This
is the foundation unit: it ports the vendor-neutral value model so the ARTEX packages
built on top of it (`llmpool`, `llmrec`, most of `agent/`) can later be ported as thin
layers over these types.

## Why a standalone oracle module

Every other manual-parity crate records ground truth by copying an oracle **into** the Go
package and running `go test`. `norma/llm` lives in the read-only Go module cache, so
that is impossible. Instead `oracle/` is a self-contained Go module that `import`s
`norma/llm` (via `replace github.com/Autumn-27/norma => github.com/BaaRai/Norma v0.4.11`)
and prints the cases. `oracle/go.sum` is committed, so `record.sh` runs fully offline and
reproducibly (`GOWORK=off GOPROXY=off`).

## Functions (11)

| Go | Signature | Notes |
|----|-----------|-------|
| `Message.Text` (types.go) | `() string` | concatenate every `text` block; ignores thinking / tool_use / tool_result |
| `Message.ToolUses` (types.go) | `() []ContentBlock` | the `tool_use` blocks; a message with none returns a **nil slice** → recorded as `null`, not `[]` |
| `Usage.Add` (types.go) | `(*Usage) Add(o Usage)` | field-wise sum |
| `Usage.MergeEcho` (types.go) | `(*Usage) MergeEcho(o Usage)` | output sums; input/cache take the **max** (Anthropic re-echoes the input side across `message_start`/`message_delta`) |
| `IsBoundaryMarker` (boundary.go) | `(Message) bool` | assistant message whose only block is a `compact_boundary` |
| `LastBoundaryIndex` (boundary.go) | `([]Message) int` | index of the last boundary marker, or `-1` |
| `MessagesAfterBoundary` (boundary.go) | `([]Message) []Message` | messages strictly after the last boundary (whole slice if none) |
| `PairToolBlocks` (boundary.go) | `([]Message) []Message` | drop orphan `tool_result`s; a dangling `tool_use` is kept (no final filter) |
| `MessagesForAPI` (boundary.go) | `([]Message) []Message` | `PairToolBlocks(MessagesAfterBoundary(msgs))` — never send a boundary to the model |
| `BoundaryMessage` (boundary.go) | `(BoundaryMeta) Message` | a boundary marker carrying the meta as its `input` |
| `ParseBoundaryMeta` (boundary.go) | `(msg) (BoundaryMeta, BoundaryStatus)` | decode the boundary block's meta + a completeness status (text / content / tool branches) |

Ported data types: `ContentBlock` (with a custom `Serialize` mirroring Go's
`MarshalJSON` — thinking blocks always emit `"thinking"` and only that block's fields),
`Message`, `Usage`, `ToolSchema`, `CompletionRequest` (no JSON tags → PascalCase, like Go),
`BoundaryMeta` (manual `Serialize` for the `omitempty` `trigger`), `BoundaryStatus`, and the
`Role`/`BlockType` string tags as constants. Go's `omitempty`/nil-slice encoding is
reproduced with `Option`/`Vec` + `skip`-style logic and a `null`-tolerant deserializer
(`Message.Content` is `null` when nil, since it has no `omitempty`).

The `boundary.go` functions are proven by `tests/parity.rs::boundary_direct` (values
embedded from the read Go source); the four `types.go` functions are proven by recorded
`cases.jsonl` from the standalone oracle. (`BoundaryStatus`/`ParseBoundaryMeta` have no
ARTEX caller, so they are normed directly.)

## Stays in Go

The wire adapters (`anthropic.go`, `openai.go`, `openai_responses.go`) and the streaming
accumulator (`accumulate.go`) are I/O- and state-machine bound; the pure, vendor-neutral
functions of `types.go` and `boundary.go` are the portable surface and are ported here.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go norma/llm (deterministic, offline)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```