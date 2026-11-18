# mcphttpx-rs

Behavioral-parity Rust port of the **unexported, side-effect-free** helpers in the
Go `mcphttp/` package that the `mcphttp-rs` bridge leaves behind, verified against
the Go original via a recorded oracle (the bridge can't reach unexported functions).

## Functions (2)

| Go | Signature | Notes |
|----|-----------|-------|
| `normalizeHeaders` (client.go) | `normalizeHeaders(map[string]string) (map[string]string, error)` | trim each key; reject empty / CR / LF keys with `invalid HTTP header name %q` (Go `%q`); values untouched |
| `isUnsupportedVersion` (client.go) | `isUnsupportedVersion(err error) bool` | lower-cased `err.Error()` contains `unsupported protocol version` / `unsupported_protocol_version` |

`normalizeHeaders` takes a `map[string]string` — a **composite map parameter** the
bridge can't reach; the harness passes it as a JSON object (Go marshals keys sorted).
Go builds its result with `range` (random order), so the parity corpus keeps at most
one invalid key per case and avoids two keys that trim to the same name — that keeps
the success map and the error message deterministic. This port iterates in sorted
order, which yields the same observable result under that constraint. `isUnsupportedVersion`
takes an `error` but only reads `err.Error()`, so the harness passes the message string
(the oracle wraps it with `errors.New`). Error messages use `gocompat::gojson::go_quote_string`
(Go `%q`); string trimming/lowering use `gocompat::strings`.

Skipped from `mcphttp/`: `readSSEEndpoint`, `parseSSE`, `readSnippet` are I/O-bound
(`bufio.Reader` / `io.Reader`).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go mcphttp/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`mcphttp/` is not modified; `record.sh` cleans up its oracle.