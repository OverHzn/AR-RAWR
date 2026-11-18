# reportex-rs

Behavioral-parity Rust port of the **unexported, side-effect-free string helpers**
in the Go `report/` package, verified against the Go original via a recorded
oracle (the bridge can't reach unexported functions).

## Functions (7)

| Go | Signature | Notes |
|----|-----------|-------|
| `nz` (report.go) | `nz(s, d string) string` | return `d` when `s` is all whitespace, else `s` |
| `oneLine` (quality.go) | `oneLine(s string) string` | `strings.Join(strings.Fields(s), " ")` |
| `unbalancedFence` (quality.go) | `unbalancedFence(s string) bool` | odd number of lines starting (after ` \t`) with ``` ``` ``` |
| `staleReportNotice` (quality.go) | `staleReportNotice(stale bool) string` | fixed advisory paragraph, or `""` |
| `reportStale` (quality.go) | `reportStale(report string, evidenceVersion, reportEvidenceVersion int64) bool` | non-empty report whose evidence version differs |
| `verificationLabel` (quality.go) | `verificationLabel(s string) string` | label for the `db.Verification*` constants; unknown passes through |
| `detailedReportLimit` (quality.go) | `detailedReportLimit(body, heading string, limit int) string` | body under `heading`, cut to `limit` bytes on a rune boundary, open fence closed, KiB note |

All seven are deterministic and value-typed (strings/ints/bools) — no net, regex,
time, or IO. Go-stdlib string semantics (Unicode whitespace, tab/space trimming,
Fields/Join) come from `gocompat::strings` so edge cases match byte-for-byte.

Skipped from `report/`: the markdown/CSV builders (`FindingsMarkdown`,
`SingleFindingMarkdown`, `FindingsCSV`, `Markdown`) take `*db.DBFinding` / a
report `Input` struct and use `text/template`, `time`, and `regexp`; the
`qualityBullets`/`qualityNarrative` builders take the `Quality` struct (a
composite-param tier of their own).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go report/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`report/` is not modified; `record.sh` cleans up its oracle.