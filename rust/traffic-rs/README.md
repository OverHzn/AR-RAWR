# traffic-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helpers** in the
Go `traffic/` package (the request-recording subsystem — not `traffic/rewrite/`,
which is `rewrite-rs`), verified against the Go original via a recorded oracle
(the bridge can't reach unexported functions).

## Functions (5)

| Go | Signature | Notes |
|----|-----------|-------|
| `sanitize` (traffic.go) | `sanitize(string) string` | map `/\:?*"<>\|` → `_`, empty/`_` → `root`, cut to 120 bytes |
| `ftsQuote` (traffic.go) | `ftsQuote(string) string` | wrap in `"`, double inner `"` (FTS5 string literal) |
| `clip` (traffic.go) | `clip(string, int) string` | if > `max` bytes, cut + append truncation note |
| `boolInt` (stream.go) | `boolInt(bool) int` | 1 / 0 |
| `validHeaderName` (replay.go) | `validHeaderName(string) bool` | RFC 7230 token check |

`sanitize`/`clip` truncate on a **byte** boundary in Go (`out[:120]`, `s[:max]`).
The recorded corpus keeps every truncation on an ASCII boundary so the byte slice
is always valid UTF-8; the Rust port mirrors the byte semantics.

Excluded: `runTestHook` (mutates the package-level `testRuleHook`; not pure).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go traffic/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

91 recorded cases. `traffic/` is not modified; `record.sh` cleans up its oracle.