# sidequestion-rs

Behavioral-parity Rust port of the **side-effect-free helpers** in the Go `sidequestion/`
package, verified against the Go original via a recorded oracle (the bridge can't reach
unexported functions / value-struct methods).

## Functions (5)

| Go | Signature | Notes |
|----|-----------|-------|
| `Parent.Key` (capture.go) | `Parent{conversation_id, task_id, exploration_id, intent_id}.key() string` | `conv-<id>` if conversation>0, else `task-<t>-exp-<e>-worker-i<i>` if intent>0, else `task-<t>-exp-<e>-main` |
| `outputBudget` (request.go) | `outputBudget(llm.CompletionRequest, configured) int` | default 8192 when `configured<=0`, cap 32768, then cap at `req.MaxTokens` if set |
| `inputBudget` (request.go) | `inputBudget(Snapshot, output) int` | `window` (default 200000) `- output - min(8192, max(128, window/20))` |
| `messageGroups` (context.go) | `messageGroups([]llm.Message) [][]llm.Message` | split at every message that leaves no tool call pending (`tool_use` opens an id, matching `tool_result` closes it); a trailing run with an open call is its own group; empty input → `nil` (JSON `null`) |
| `EstimateInputTokens` (request.go) | `EstimateInputTokens(llm.CompletionRequest) int` | `compaction.EstimateTokens(msgs)*4/3 + 32 + len(msgs)*8`, then per system segment `(len+2)/3+8` and per tool `(len(json)+2)/3+8` |

`Parent` is a small value struct (four `int64` fields), so it ports faithfully.
`outputBudget`/`inputBudget` are struct-param functions whose result depends on a
single field each — the request's `MaxTokens` and the snapshot's
`Model.WindowTokens` — so the harness passes those scalars and the port mirrors
the arithmetic. `messageGroups` and `EstimateInputTokens` operate on the `norma/llm` model
(now in the sibling [`llm-rs`](../llm-rs) crate) and, for `EstimateInputTokens`, the
`norma/compaction` estimator (the sibling [`compaction-rs`](../compaction-rs) crate); this
crate depends on both. `EstimateInputTokens` re-serializes each `ToolSchema` to count its
JSON byte length (Go `json.Marshal`), so the harness keeps tool schemas compact ASCII and
relies on serde_json's sorted-map output matching Go's. The oracle still calls the real Go
functions with real `llm.CompletionRequest` / `Snapshot` / `[]llm.Message` values.

Skipped: `Exchange.Running()` (pure but on a large IO-laden struct), `CloneRequest` (a pure
JSON round-trip, but a faithful byte clone is blocked by Go's nil-slice-vs-`[]` encoding),
and `assemble`/`exchangeMessages`/`BuildRequest` — `exchangeMessages` needs `time.Time`
RFC3339 formatting, so `BuildRequest` (which would use `CloneRequest` + `EstimateInputTokens`
+ `MessagesForAPI`, all now available) stays in Go solely on `assemble`'s `time.Time`.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go sidequestion/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

149 recorded cases. `sidequestion/` is not modified; `record.sh` cleans up its oracle.