# sidequestion-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helper** in the
Go `sidequestion/` package, verified against the Go original via a recorded oracle
(the bridge can't reach unexported / value-struct methods).

## Functions (1)

| Go | Signature | Notes |
|----|-----------|-------|
| `Parent.Key` (capture.go) | `Parent{conversation_id, task_id, exploration_id, intent_id}.key() string` | `conv-<id>` if conversation>0, else `task-<t>-exp-<e>-worker-i<i>` if intent>0, else `task-<t>-exp-<e>-main` |

`Parent` is a small value struct (four `int64` fields), so it ports faithfully.

Skipped: `Exchange.Running()` (`return e.Status == "running"`) is pure but is a
method on a large IO-laden struct (`time.Time`, `llm.Usage`, `Model`,
`ContextInfo`, …) that cannot be modelled as a value type.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go sidequestion/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

11 recorded cases. `sidequestion/` is not modified; `record.sh` cleans up its oracle.