# traffic-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helpers** in the
Go `traffic/` package (the request-recording subsystem — not `traffic/rewrite/`,
which is `rewrite-rs`), verified against the Go original via a recorded oracle
(the bridge can't reach unexported functions).

## Functions (11)

| Go | Signature | Notes |
|----|-----------|-------|
| `sanitize` (traffic.go) | `sanitize(string) string` | map `/\:?*"<>\|` → `_`, empty/`_` → `root`, cut to 120 bytes |
| `ftsQuote` (traffic.go) | `ftsQuote(string) string` | wrap in `"`, double inner `"` (FTS5 string literal) |
| `clip` (traffic.go) | `clip(string, int) string` | if > `max` bytes, cut + append truncation note |
| `boolInt` (stream.go) | `boolInt(bool) int` | 1 / 0 |
| `validHeaderName` (replay.go) | `validHeaderName(string) bool` | RFC 7230 token check |
| `uniqueArchiveHosts` (archive.go) | `uniqueArchiveHosts([]string) []string` | lowercase + trim, drop empty/dup, sort |
| `clipBytes` (traffic.go) | `clipBytes([]byte, int) []byte` | `b[:n]` (returns `b` whole when `len(b) <= n`) |
| `truncateUTF8` (traffic.go) | `truncateUTF8([]byte, int) string` | cut to `n` bytes, back off an incomplete trailing rune |
| `isBinaryBody` (traffic.go) | `isBinaryBody(string, []byte) bool` | `binaryTypes` prefix, or NUL in first 512 bytes |
| `binaryTag` (traffic.go) | `binaryTag(string, []byte) string` | `[binary <ct>, <n> bytes, magic=<hex4>]` |
| `appliedRules` (traffic.go) | `appliedRules([]rewrite.TraceLine) (bool, []int64)` | applied lines? + ordered unique non-zero `RuleID`s |

`sanitize`/`clip` truncate on a **byte** boundary in Go (`out[:120]`, `s[:max]`).
The recorded corpus keeps every truncation on an ASCII boundary so the byte slice
is always valid UTF-8; the Rust port mirrors the byte semantics. The `[]byte`
functions (`clipBytes`/`truncateUTF8`/`isBinaryBody`/`binaryTag`) travel through
the harness as **base64** (Go's `encoding/json` encodes `[]byte` that way);
`truncateUTF8`'s corpus uses valid UTF-8 bodies so the byte cut lands on a rune
boundary the port reproduces via `str::from_utf8`'s `valid_up_to`.

`appliedRules` is the first struct-param function: the harness passes
`[]rewrite.TraceLine` and reads each element's `RuleID`/`Outcome` (the other
fields don't affect the result). Go returns a nil `[]int64` when no rule id is
collected, which marshals to `null` — the harness mirrors that instead of `[]`.

Excluded: `runTestHook` (mutates the package-level `testRuleHook`; not pure).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go traffic/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

810 recorded cases. `traffic/` is not modified; `record.sh` cleans up its oracle.