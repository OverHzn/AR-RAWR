# sidequestion-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helper** in the
Go `sidequestion/` package, verified against the Go original via a recorded oracle
(the bridge can't reach unexported / value-struct methods).

## Functions (4)

| Go | Signature | Notes |
|----|-----------|-------|
| `Parent.Key` (capture.go) | `Parent{conversation_id, task_id, exploration_id, intent_id}.key() string` | `conv-<id>` if conversation>0, else `task-<t>-exp-<e>-worker-i<i>` if intent>0, else `task-<t>-exp-<e>-main` |
| `outputBudget` (request.go) | `outputBudget(llm.CompletionRequest, configured) int` | default 8192 when `configured<=0`, cap 32768, then cap at `req.MaxTokens` if set |
| `inputBudget` (request.go) | `inputBudget(Snapshot, output) int` | `window` (default 200000) `- output - min(8192, max(128, window/20))` |
| `messageGroups` (context.go) | `messageGroups([]llm.Message) [][]llm.Message` | split at every message that leaves no tool call pending (`tool_use` opens an id, matching `tool_result` closes it); a trailing run with an open call is its own group; empty input → `nil` (JSON `null`) |

`Parent` is a small value struct (four `int64` fields), so it ports faithfully.
`outputBudget`/`inputBudget` are struct-param functions whose result depends on a
single field each — the request's `MaxTokens` and the snapshot's
`Model.WindowTokens` — so the harness passes those scalars and the port mirrors
the arithmetic. `messageGroups` operates on the `norma/llm` model, now ported in the
sibling [`llm-rs`](../llm-rs) crate (`Message` / `ContentBlock`); this crate depends on
it. The map it uses is membership-only (`len`), so the grouping is deterministic. The
oracle still calls the real Go functions with real `llm.CompletionRequest` /
`Snapshot` / `[]llm.Message` values (it imports `norma/llm`).

Skipped: `Exchange.Running()` (pure but on a large IO-laden struct), and
`assemble`/`exchangeMessages`/`BuildRequest` — `exchangeMessages` needs `time.Time`
RFC3339 formatting, and `BuildRequest` needs `llm.MessagesForAPI`/`CloneRequest`/
`EstimateInputTokens`.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go sidequestion/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

144 recorded cases. `sidequestion/` is not modified; `record.sh` cleans up its oracle.