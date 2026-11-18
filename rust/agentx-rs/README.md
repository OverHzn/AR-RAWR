# agentx-rs

Behavioral-parity Rust port of a batch of **unexported, side-effect-free, pure**
helpers in the Go `agent/` package that the `agent-rs` bridge leaves behind, verified
against the Go original via a recorded oracle (the bridge can't reach unexported
functions).

## Functions (18)

| Go | Signature | Notes |
|----|-----------|-------|
| `budgetCap` (budget.go) | `budgetCap(int64) int64` | `<=0` → `Unlimited` (`math.MaxInt64`), else `n` |
| `left` (budget.go) | `left(capacity, spent int64) int64` | `Unlimited` stays `Unlimited`; else `max(capacity-spent, 0)` |
| `relatedOverviewBudgetForSources` (tools.go) | `(int) int` | total rune budget ÷ capped source count, capped per source; `<=0` → 0 |
| `foldableKind` (coldgraph.go) | `(string) bool` | kind is `intent` / `fact` |
| `isFalsy` (provider.go) | `(string) bool` | trimmed+lowered in {`0`,`false`,`off`,`no`} |
| `firstLine` (capture.go) | `(string, max int) string` | trim, cut at first `\n`, rune-truncate to `max` + `…` |
| `truncOutput` (planner.go) | `(string, n int) string` | rune-truncate to `n` + ` …(truncated; …)` |
| `workerChatMarker` (worker.go) | `(string) string` | `workerChatMarkerPrefix + id + " -->"` |
| `normalizeDirection` (tools.go) | `(string) string` | lower, collapse spaces, each digit run → `#` |
| `hintAuthor` (planner.go) | `(string) string` | origin → human phrase |
| `normalizeFactSource` (tools.go) | `(string) (string, error)` | empty → `inference`; known source (case/space-insensitive) → itself; else `source %q is not valid: …` |
| `splitLines` (tools_insert.go) | `(string) []string` | split `\n`, trim, drop blanks; none left → nil (`null`) |
| `isEmptyJSON` (toolcatalog.go) | `json.RawMessage → bool` | raw text is exactly `null` or `""` |
| `pid` (tools.go) | `json.RawMessage → int64` | JSON integral number, or numeric string (trimmed, base-10); else 0 |
| `pidList` (tools.go) | `[]json.RawMessage → []int64` | `pid` over an array, drop `<=0`; none left → nil (`null`) |
| `bbotQuote` (bbot_ingest.go) | `(data, dataJSON json.RawMessage) string` | pick non-absent (`≠""/"null"`); JSON string → decode + Go-quote, else compact + Go-quote |
| `clipBody` (provider.go) | `(string) string` | trim; empty → `(empty)`; >4096 bytes → first 4096 bytes + `…(truncated, N bytes total)` |
| `nmapWebScheme` (scan_ingest.go) | `(name, tunnel string) string` | `http`/`http-alt`/`http-proxy` → `http` (`https` if `ssl`); `https`/`https-alt` → `https`; else `""` |

All are pure and value-typed (ints, bools, strings, `[]string`, raw-JSON text); constants
from the Go source (`Unlimited = math.MaxInt64`, the related-overview budget `48_000`/`8_000`,
`db.MaxTaskSourceCount = 8`, the `factSources` set, `workerChatMarkerPrefix`) are inlined
with their exact values. Rune-aware truncation matches Go's `[]rune` slicing /
`utf8.RuneCountInString`; error text uses `gocompat::gojson::go_quote_string` (Go `%q`);
trimming/field-splitting/lowering use `gocompat::strings`; a nil `[]string` mirrors Go's
JSON `null`. The JSON helpers take the raw message as its text (the oracle wraps it with
`json.RawMessage`): `pid` parses a JSON integral number or numeric string via `serde_json`
(Go rejects `12.9`/`1e3` into `int64`); `bbotQuote` decodes a JSON string with `serde_json`
or, for other valid JSON, compacts with a hand-written `json.Compact` (strip whitespace
outside strings, preserve escape bytes) — then `strconv.Quote`.

The rest of `agent/`'s unexported surface needs heavier ports (the LLM message model,
`db` graph/node types, `actool.CoreTool`, net/url, regexp) or touches I/O and stays in
Go.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go agent/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`agent/` is not modified; `record.sh` cleans up its oracle.