# guard-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helpers** in the
Go `guard/` package, verified against the Go original via a recorded oracle
(the bridge can't reach unexported functions).

## Functions (2)

| Go | Signature | Notes |
|----|-----------|-------|
| `capBlockReason` (guard.go) | `capBlockReason(string) string` | keep ≤ `maxBlockReasonRunes` (4000) runes, else append ` …(truncated)` |
| `systemBlockMessage` (guard.go) | `systemBlockMessage(string) string` | fixed platform-control message wrapping `capBlockReason` |

The truncation counts **runes** (Go `for i := range s`) and cuts at the byte index
of the 4001st rune — multibyte-safe, so the corpus includes non-ASCII inputs.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go guard/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

28 recorded cases. `guard/` is not modified; `record.sh` cleans up its oracle.