# guard-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helpers** in the
Go `guard/` package, verified against the Go original via a recorded oracle
(the bridge can't reach unexported functions).

## Functions (3)

| Go | Signature | Notes |
|----|-----------|-------|
| `capBlockReason` (guard.go) | `capBlockReason(string) string` | keep ≤ `maxBlockReasonRunes` (4000) runes, else append ` …(truncated)` |
| `systemBlockMessage` (guard.go) | `systemBlockMessage(string) string` | fixed platform-control message wrapping `capBlockReason` |
| `TargetBlockMessage` (guard.go) | `TargetBlockMessage(intercept.TargetResult) string` | model-facing target-gate block text; reads `Code`/`Host`/`Port`/`Reason` |

`capBlockReason` counts **runes** (Go `for i := range s`) and cuts at the byte index
of the 4001st rune — multibyte-safe, so the corpus includes non-ASCII inputs.
`TargetBlockMessage` is exported but takes a struct (`intercept.TargetResult`) the
bridge also skips, so it is verified through the same oracle: the harness reads its
`Code`/`Host`/`Port`/`Reason` and reuses `gocompat::neturl::join_host_port` for the
`host:port` form (Go `net.JoinHostPort`). The oracle covers the six known codes,
an unknown code, an IPv6 host (raw and bracketed), an empty host/code, and a
reason long enough to truncate past the host name.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go guard/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

42 recorded cases. `guard/` is not modified; `record.sh` cleans up its oracle.