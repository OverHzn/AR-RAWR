# sidequestion-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helper** in the
Go `sidequestion/` package, verified against the Go original via a recorded oracle
(the bridge can't reach unexported / value-struct methods).

## Functions (3)

| Go | Signature | Notes |
|----|-----------|-------|
| `Parent.Key` (capture.go) | `Parent{conversation_id, task_id, exploration_id, intent_id}.key() string` | `conv-<id>` if conversation>0, else `task-<t>-exp-<e>-worker-i<i>` if intent>0, else `task-<t>-exp-<e>-main` |
| `outputBudget` (request.go) | `outputBudget(llm.CompletionRequest, configured) int` | default 8192 when `configured<=0`, cap 32768, then cap at `req.MaxTokens` if set |
| `inputBudget` (request.go) | `inputBudget(Snapshot, output) int` | `window` (default 200000) `- output - min(8192, max(128, window/20))` |

`Parent` is a small value struct (four `int64` fields), so it ports faithfully.
`outputBudget`/`inputBudget` are struct-param functions whose result depends on a
single field each — the request's `MaxTokens` and the snapshot's
`Model.WindowTokens` — so the harness passes those scalars and the port mirrors
the arithmetic. The oracle still calls the real Go functions with real
`llm.CompletionRequest` / `Snapshot` values (it imports `norma/llm`).

Skipped: `Exchange.Running()` (pure but on a large IO-laden struct), and the
message-building functions (`messageGroups`, `exchangeMessages`, `assemble`,
`BuildRequest`) — those construct `llm.Message`/`llm.CompletionRequest` values and
would require porting a slice of the external `norma/llm` package (Message,
ContentBlock, CompletionRequest, …) plus matching its JSON shape.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go sidequestion/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

134 recorded cases. `sidequestion/` is not modified; `record.sh` cleans up its oracle.