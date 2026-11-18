# server-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helpers** in the
Go `server/` package — the pure functions `rust-anything`'s bridge cannot reach
(it only bridges exported, pointer-free functions).

## Why a different verification method

`rust-anything bridge` extracts only exported, pointer-free functions from
`server/`, and the three it does find (`ScrubSecretEnv`, `StartLogCapture`,
`WithSecretScrub`) are all side-effectful (env/IO/closures). The genuinely pure
helpers here (`humanBytes`, `firstLine`, `trunc`, `shellQuote`, …) are
**unexported**, so the bridge can't touch them without editing the Go original —
which would break the 1:1 premise.

Instead, parity is proven directly against the real Go functions:

1. `record.sh` copies `oracle/zz_oracle_test.go.tmpl` into `server/`, runs it with
   `go test`, captures the JSON output (one line per call), then **deletes** it —
   the `server/` package is left untouched.
2. `cases.jsonl` holds 5910 ground-truth cases: `{fn, args, ok|error}` computed by
   the *real* Go functions over a fixed corpus.
3. `tests/parity.rs` replays that corpus against these Rust ports and asserts
   byte-identical output.

## Functions (33)

| File (Go) | Functions |
|-----------|-----------|
| `auth.go` | `validatePassword` |
| `chatupload.go` | `sanitizeFilename`, `humanBytes` |
| `constraints_api.go` | `normalizeConstraintKind` |
| `conversations.go` | `firstLine` |
| `customtool.go` | `proxyURLString`, `identity`, `shellQuote` |
| `dto.go` | `i64s` |
| `engine.go` | `preview` |
| `finding_verification.go` | `normalizeEvidenceText`, `shortQuote` |
| `intent_intervention.go` | `validWorkerMessageRequestID` |
| `logsink.go` | `levelOf` |
| `notifier.go` | `trimTrailingSlash` |
| `platform_tools.go` | `enabledWord` |
| `scheduler.go` | `trunc` |
| `server.go` | `truncateReply`, `portOr`, `defaultPort`, `schemeForPort`, `normFilter`, `inheritedIntentResult`, `atoiDefault` |
| `server_mgmt.go` | `validSkillName`, `maskedSecret`, `isMaskedSecret`, `yamlScalar`, `skillPathRune`, `canonicalTaskID` |
| `skill_zip.go` | `zipMethodName` |
| `task_llm.go` | `llmFailKey` |
| `traffic_ingest.go` | `truncateBody` |

Supporting Go constants (`minPasswordRunes`, `verificationNoteQuoteLen`,
`maxIngestBodySize`, `skillPathBlocked`, `notify.MaskedPrefix`, the ZIP method
names) are copied verbatim. `skillPathRune`'s `unicode.Cf`/`unicode.Co` category
tests use tables generated from Go's own `unicode` package.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go server/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`server/` is not modified; `record.sh` cleans up its temporary oracle file.