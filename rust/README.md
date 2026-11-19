# ARTEX → Rust port (`rust/`)

Behavioral-parity Rust ports of the ARTEX Go modules, one unit at a time, proven
against the Go original with [`rust-anything`](https://github.com/erkamyaman/rust-anything).

Each unit is a standalone crate (`crate-type = ["cdylib", "rlib"]`) that exposes
the same API as its Go package, plus a JSON dispatcher (`call_json`) and typed/wire
C entry points. `rust-anything` drives the loop:

```
bridge → record → (port.rs) → build → diff → gate
```

`diff`/`gate` compare the Rust output against cases recorded from the Go original;
`diff` must reach 100% before a unit is considered done.

## Units (all gate-green)

| Unit | Cases | Go LOC | Notes |
|------|------:|-------:|-------|
| `config-rs`   | 213  | 212   | `gate.sh` sets `ARTEX_CONFIG` (error-path cases) |
| `notify-rs`   | 254  | 2733  | |
| `report-rs`   | 12   | 540   | |
| `rewrite-rs`  | 91   | 1034  | (`traffic/rewrite/`) |
| `mcphttp-rs`  | 76   | 585   | norma unit, direct; unexported pure helpers → `mcphttpx-rs` |
| `egress-rs`   | 147  | 565   | norma transitif (intercept→db→norma) |
| `intercept-rs`| 361  | 4004  | 5 heavy funcs kept in Go (`--keep`) |
| `db-rs`       | 1525 | 22078 | 17 funcs kept in Go (`--keep`); unexported dep-free helpers → `dbx-rs` |
| `agent-rs`    | 2418 | 10181 | 7 funcs kept in Go; `gate.sh` re-patches generated files; unexported pure helpers → `agentx-rs` |

`gocompat/` is a shared crate holding faithful Rust ports of the Go stdlib pieces
the units rely on (`net/url`, `path/filepath`, `encoding/json` error shapes,
`strings` incl. Go's *simple* Unicode case mapping, `time`, `encoding/base64`,
`regexp` error text). It is a separate crate, so its size does not count against
any unit's size gate.

## Units ported without the bridge (manual parity)

These units port the **unexported, side-effect-free helpers** of their Go package —
`rust-anything bridge` only extracts exported, pointer-free functions, so it can't
reach them. Parity is proven against the *real* Go functions instead: `record.sh`
copies a temporary oracle into the package, runs it under `go test`, writes
`sorted cases.jsonl`, then deletes it (the package is left untouched);
`tests/parity.rs` replays the corpus and asserts byte-identical output. Each crate
gates with `./gate.sh` (fmt, clippy `-D warnings`, parity tests).

| Unit | Cases | Go source | Functions |
|------|------:|----------|-----------|
| `server-rs`       | 5936 | `server/` | 34 helpers (`humanBytes`, `firstLine`, `trunc`, `validatePassword`, `skillPathRune`, `parseTaskID`, …) |
| `traffic-rs`      | 810  | `traffic/` | `sanitize`, `ftsQuote`, `clip`, `boolInt`, `validHeaderName`, `uniqueArchiveHosts`, `clipBytes`, `truncateUTF8`, `isBinaryBody`, `binaryTag`, `appliedRules` |
| `enrich-rs`       | 76   | `enrich/`  | `id0`, `itoa`, `hostOf`, `uniq`, `isBlank` |
| `guard-rs`        | 42   | `guard/`   | `capBlockReason`, `systemBlockMessage`, `TargetBlockMessage` |
| `sidequestion-rs` | 149  | `sidequestion/` | `Parent::key`, `outputBudget`, `inputBudget`, `messageGroups`, `EstimateInputTokens` (`messageGroups`/`EstimateInputTokens` over the `llm-rs` + `compaction-rs` models; `BuildRequest` needs `time.Time` via `assemble` and stays in Go) |
| `reportex-rs`     | 62   | `report/`  | `nz`, `oneLine`, `unbalancedFence`, `staleReportNotice`, `reportStale`, `verificationLabel`, `detailedReportLimit` |
| `notifyx-rs`      | 970  | `notify/`  | `cfgString`, `cfgInt`, `cfgBool`, `cfgStrings`, `cfgMap`, `ValidKind`, `Kinds` (composite `map[string]any` param) |
| `dbx-rs`          | 51   | `db/`      | `nullIfEmpty`, `isIdentStart`, `isIdentChar`, `marshalStringArray`, `nullableInt` (dep-free unexported helpers) |
| `mcphttpx-rs`     | 21   | `mcphttp/` | `normalizeHeaders` (composite `map[string]string`, `(map, error)`), `isUnsupportedVersion` (`error` → message string) |
| `agentx-rs`       | 169  | `agent/`   | `budgetCap`, `left`, `relatedOverviewBudgetForSources`, `foldableKind`, `isFalsy`, `firstLine`, `truncOutput`, `workerChatMarker`, `normalizeDirection`, `hintAuthor`, `normalizeFactSource`, `splitLines`, `isEmptyJSON`, `pid`, `pidList`, `bbotQuote`, `clipBody`, `nmapWebScheme` (pure int/string/JSON helpers) |
| `llmpoolx-rs`     | 23   | `llmpool/` | `trimErr`, `blocksLen`, `estimateTokens` (`blocksLen`/`estimateTokens` over the `norma/llm` model ported in `llm-rs`; other helpers need regexp/`[]*Member` pointers/context and stay in Go) |
| `llmrecx-rs`      | 23   | `llmrec/`  | `parseSession`, `thinkingText` (`thinkingText` over the `norma/llm` model ported in `llm-rs`; other helpers need `context` and stay in Go) |
| `llm-rs`          | 24   | `norma/llm` (external) | `Message.Text`, `Message.ToolUses`, `Usage.Add`, `Usage.MergeEcho`, the `boundary.go` functions (`IsBoundaryMarker`/`LastBoundaryIndex`/`MessagesAfterBoundary`/`PairToolBlocks`/`MessagesForAPI`/`BoundaryMessage`/`ParseBoundaryMeta`) + the value model (`ContentBlock`/`Message`/`Usage`/`ToolSchema`/`CompletionRequest`/`BoundaryMeta`); the oracle is a **standalone Go module** because `norma/llm` lives in the read-only module cache (can't copy an oracle into it) |
| `compaction-rs`   | 21   | `norma/compaction` (external) | `EstimateTokens` (Σ `blockChars` `/4` over the `llm-rs` message model), `IsOverflow` (`error`→`bool`); oracle is a **standalone Go module** (only exported funcs reachable) |
| `tool-rs`         | 23   | `norma/tool` (external) | `Text`/`Errorf` (→ `Result`), `RenderDeferredToolsBlock`, `RenderTodoReminder` (pure helpers of a mostly-IO package); oracle is a **standalone Go module** whose go.mod/go.sum mirror ARTEX's deps (tool pulls `creack/pty`, `x/net`, …) |
| `transcript-rs`   | 28   | `norma/transcript` (external) | `Messages` (`[]Record` → `(messages, usage)` over the `llm-rs` model), `LastUUID` (pure folds; `Record.Timestamp` is a Go `string`, not `time.Time`, so the record model ports faithfully); oracle is a **standalone Go module** (only exported funcs reachable; `Store` writer/loader + `crypto/rand` id helpers stay in Go) |
| `permission-rs`   | 36   | `norma/permission` (external) | `Allowed`/`Denied`/`AskUser` constructors, `Evaluate` (four-stage deny-priority pipeline: deny rules → mode short-circuit → allow rules/auto-allows → interactive; the only impure input `Request.Ask` host callback is modeled by its resolved `Option<Decision>` return value, so the pipeline is a pure function of value-typed inputs), plus `matchRule`/`editTools` transitively; stdlib-only package → lightweight standalone oracle, no `llm-rs` dep |
| `noa-rs`   | 206   | `norma/noa` (external, *beyond ARTEX scope*) | batch 9a+9b+9c atomic pure helpers of the context-compression engine: `FormatTokens` (`…K` / `%.1fK`), `IndexToRef`/`RefToIndex` (`mNNNNN` codec), `ParseBlockID` (`bN`, tier-suffix strip), `SummaryMessageID`/`BlockIDFromSummaryID`, `DefaultCountTokens`/`EstimateTokensFast` (rune-based, CJK-aware), `ArchiveFileName`/`ArchiveTierDir`, `PanelBlockCount` (`\bb\d+\(T\d+\)=` word-boundary), `DefaultConfig`/`ValidateConfig` (full `Config` value model, `%g`-faithful warnings, Go nil-slice→`null`), `CoreMessage` model + `ComputeTurnGroups`/`WithdrawSplitTurns` (turn-integrity: reasoning/act/result partitioning, split-turn withdrawal), `AdjustBoundariesForToolPairs`/`AdjustBoundariesForReasoningPairs`/`ApplyPairBoundaryAdjustments` (pair-boundary widening to a fixed point, Compress tool excluded), `ParseCompressArgs` (lenient multi-layer Compress-call parser: string-unwrap, markdown-fence, trailing-comma, raw-newline, tail-repair, bracket-salvage; all 8 `CompressParseKind`s; `Keys` map-order normalised sorted for determinism); stdlib-only → lightweight standalone oracle; Go `regexp` reproduced by hand (no `regex` crate offline) |

In `server/`, the three functions the bridge *does* extract (`ScrubSecretEnv`,
`StartLogCapture`, `WithSecretScrub`) are all side-effectful (env/IO/closure), so
the bridge is unusable there. `enrich-rs`'s `hostOf` reuses `gocompat::neturl`
(`Url::hostname` / `split_host_port` added for it); `server-rs`'s `parseTaskID`
uses `gocompat::gojson::go_quote_string` (Go `%q`); `traffic-rs`'s
`uniqueArchiveHosts` uses `gocompat::strings` (`go_to_lower`/`go_trim_space`).
Excluded as impure / non-value: `traffic.runTestHook` (mutates package-global
`testRuleHook`) and `sidequestion.Exchange::Running` (method on a large IO struct).

## Reproducing a unit's gate

```sh
cd rust/<unit>
../ra gate .          # fmt, clippy, tests, unsafe, API, size, error-cases, build, diff
```

To verify **every** unit at once (gocompat build + all 28 crates, each via its own
gate — `gate.sh` if present, else `ra gate .`):

```sh
cd rust
./gate-all.sh          # incremental (reuses target/)
./gate-all.sh --clean  # wipe every target/ first (full from-scratch rebuild)
```

It puts `tools/go-shim` first on `PATH` (so norma-importing units build) and prints
a PASS/FAIL line per unit with a final tally; exit code is non-zero if any fail.

`ra` (this directory) runs `rust-anything` with the go-shim first on `PATH` so
units that transitively import `github.com/Autumn-27/norma` build against the fork
`replace github.com/Autumn-27/norma => github.com/BaaRai/Norma v0.4.11`.

Two units need their wrapper instead of a bare `ra gate`:

- `config-rs`: `./gate.sh` — exports `ARTEX_CONFIG=fixtures/cfgdir` so `Load`/
  `SkillDir`/`PostgresDSN` exercise their directory-read error paths (recorded with
  the same env). `--record` re-records from the Go original.
- `agent-rs`: `./gate.sh` — re-applies two bridge-template-gap fixes
  (`go/agent.go` needs `RetryConfig`/`llm.Format`; the zero-arg `BuiltinPromptSeeds`
  wire shim leaves an unused reader) via `patch-generated.py`, then gates.
  `--bridge` regenerates from scratch and re-patches.

## Kept-in-Go functions

Some exported functions are not portable through the parity harness and stay in Go
via `rust-anything bridge --keep "<Name>: <reason>"`:

- **`agent-rs`**: `SetToolAugment`/`ToolAugment`/`SetToolResolve`/`ToolResolve`
  (mutable global fn-pointer hooks), `BudgetsFromEnv`/`FromEnv` (env I/O),
  `BuiltinToolSeeds` (constructs heavy `actool.CoreTool` SDK values).
- **`db-rs`**: struct/IO/nondeterministic/parser cases (e.g. `RootDomain`,
  `SplitURL`, `ValidateDSL`, `DSN`, `ParseScopeLine`, `RenderLessons`,
  `EvaluateAssetGate`, …).
- **`intercept-rs`**: 5 heavy-dependency functions.

## Units not ported (no portable pure functions)

The remaining Go packages are pointer/IO/concurrent-heavy. Their pure helpers that
take **primitive parameters** are all ported via a Go oracle (`server-rs`,
`traffic-rs`, `enrich-rs`, `guard-rs`, `sidequestion-rs`, `reportex-rs`,
`notifyx-rs`, `dbx-rs`, `mcphttpx-rs`, `agentx-rs`, `llmpoolx-rs`, `llmrecx-rs`,
`llm-rs`, `compaction-rs`, `tool-rs`, `transcript-rs`, `permission-rs`). A further tier of pure
functions takes **composite parameters** (slices, maps, `[]byte`, or structs from
other packages) — the bridge can't reach those either, and they are only
*partially* portable; see "Composite-parameter tier" below. What stays in Go
regardless is the stateful / pointer / IO-heavy bulk.

### Composite-parameter pure functions (partially portable)

Pure functions the primitive-param sweep missed. Portability depends on the
parameter type:

| Function | Params → return | Status |
|----------|-----------------|--------|
| `enrich.uniq`, `traffic.uniqueArchiveHosts` | `[]string` → `[]string` | **ported** (`enrich-rs`, `traffic-rs`); JSON arrays, deterministic |
| `enrich.isBlank` | `any` → `bool` | **ported** (`enrich-rs`); harness passes a JSON value; `null` / empty string / numeric 0 → true, bool/array/object → false (Go type switch on nil/string/float64/int) |
| `traffic.clipBytes`, `truncateUTF8`, `isBinaryBody`, `binaryTag` | `[]byte` / `string, []byte` | **ported** (`traffic-rs`); base64 in the harness via `gocompat::encoding`, hex helper for the magic tag |
| `traffic.toHeaderMap` | `map[string]string` | returns a map (Go JSON sorts keys — deterministic but trivial) |
| `notify.cfgString`/`cfgInt`/`cfgBool`/`cfgStrings`/`cfgMap` | `map[string]any` | **ported** (`notifyx-rs`); the harness passes the config as a JSON object (Go marshals keys sorted), reads `key`; nil slice/map → `null` |
| `db.marshalStringArray` | `[]string` | **ported** (`dbx-rs`); Postgres text[] literal, nil slice → `{}`; `db.nullIfEmpty`/`nullableInt`/`isIdentStart`/`isIdentChar` are primitive-param and ported alongside |
| `mcphttp.normalizeHeaders` | `map[string]string` | **ported** (`mcphttpx-rs`); harness passes a JSON object (Go marshals keys sorted); `(map, error)` — error message uses Go `%q`; corpus keeps ≤1 bad key so Go's random map order stays deterministic. `mcphttp.isUnsupportedVersion` (`error` → `err.Error()` string) ported alongside |
| `traffic.headerLines` | `map[string][]string` | **not recordable** — Go map iteration order is random |
| `traffic.appliedRules` | `[]rewrite.TraceLine` | **ported** (`traffic-rs`); reads each line's `RuleID`/`Outcome`, returns `(applied?, ordered unique non-zero ids)` — nil `[]int64` → `null` mirrored |
| `guard.TargetBlockMessage` | `intercept.TargetResult` | **ported** (`guard-rs`); the harness passes `Code`/`Host`/`Port`/`Reason` and reuses `gocompat::neturl::join_host_port` (Go `net.JoinHostPort`) |
| `sidequestion.outputBudget`, `inputBudget` | `llm.CompletionRequest` / `Snapshot` | **ported** (`sidequestion-rs`); each reads one field (`MaxTokens`, `Model.WindowTokens`) — the harness passes the scalar |
| `sidequestion.messageGroups`/`BuildRequest`/`assemble`/… | `llm.Message`/`Snapshot`/`Exchange` | `messageGroups` + `EstimateInputTokens` **ported** (`sidequestion-rs`, over `llm-rs` + `compaction-rs`); `BuildRequest`'s `llm.MessagesForAPI` + `CloneRequest` are pure/available — the **only** remaining blocker is `assemble`/`exchangeMessages`, which format `time.Time` RFC3339 (no offline time crate) |
| `llmpool.rotateGroups`/`blocksLen` | `[]*Member`/`[]llm.ContentBlock` | `blocksLen` (+ `estimateTokens`, over `llm-rs`'s `CompletionRequest`) **ported** (`llmpoolx-rs`); `rotateGroups` still needs `[]*Member` pointers |
| `enrich.extractTitle` | `[]byte` | regex (`reTitle`) + `html.UnescapeString`; no offline regex crate |

The `[]string`, `[]byte`, and struct subsets (`guard.TargetBlockMessage`,
`traffic.appliedRules`) were clean enough to port into the existing crates the
same way as the primitive helpers (the `[]byte` ones via a base64 helper in the
harness; the struct ones by reading the relevant struct fields through the
harness and reusing `join_host_port` where needed). The rest need a fuller struct
port or are blocked (regex, map non-determinism).

What stays in Go regardless is the stateful / pointer / IO-heavy bulk.
"Src"/"Test" below are **code-LOC** (non-blank, non-comment lines; comments and
blanks excluded, measured with a small script — so the numbers differ from a raw
`wc -l`):

| Package | Src | Test (LOC / files) | Ported | Stays in Go |
|---------|----:|-------------------:|--------|-------------|
| `server/` | 24480 | 17926 / 104 files | 34 helpers (`server-rs`) | stateful HTTP handlers, `*Server` over DB/receiver |
| `traffic/` | 3572 | 3757 / 12 files | 5 helpers (`traffic-rs`) | stateful ingest / replay / stream; `runTestHook` global |
| `sidequestion/` | 674 | 602 / 3 files | `Parent::key`, `outputBudget`, `inputBudget`, `messageGroups` (`sidequestion-rs`) | `Exchange` (large IO struct), `assemble`/`BuildRequest` (time / `llm.MessagesForAPI`), request/replay flow |
| `report/` | 438 | 253 / 1 file | 7 string helpers (`reportex-rs`) | markdown/CSV builders (`*db.DBFinding`/`Input`, `text/template`, `time`, `regexp`), `qualityBullets`/`qualityNarrative` (`Quality` struct) |
| `notify/` | 1768 | 2272 / 12 files | 7 config helpers (`notifyx-rs`) | senders (`dingTalkSignedURL`, `buildEmailMessage`, SMTP/HTTP), `render*` (`text/template`) |
| `llmrec/` | 423 | 369 / 4 files | `parseSession`, `thinkingText` (`llmrecx-rs`) | LLM recommendation (SQL + JSON aggregation); `workerFrom`/`reportStop` need `context` |
| `evidence/` | 379 | 357 / 2 files | — | fingerprint / trust-scoring over DB |
| `llmpool/` | 348 | 518 / 4 files | `trimErr`, `blocksLen`, `estimateTokens` (`llmpoolx-rs`) | LLM connection pool; `State::Open` reads `time.Now`; `rotateGroups` (`[]*Member`) + regex/context helpers stay in Go |
| `enrich/` | 248 | 0 / 0 files | 3 helpers (`enrich-rs`) | `*Engine` DNS / asset-upsert workers (no tests) |
| `guard/` | 243 | 1184 / 5 files | 2 helpers (`guard-rs`) | audit-log / decision layer (`*Guard`, DB audit, env) |
| `cmd/artex/` | 126 | 16 / 1 file | — | process entrypoint |

The packages **with Go tests** (`traffic`, `sidequestion`, `llmrec`, `evidence`,
`llmpool`, `guard`) are the candidates for a *functional rewrite* — porting the
stateful code with std Rust and verifying it by translating the Go unit tests
(there is no bridge/diff parity for IO code, so the Go tests become the spec).
`guard/` is the ideal first target: the smallest source (243 LOC) with the
richest test suite (1184 LOC / 5 files — a ~5:1 test-to-source ratio).
`enrich/` has no tests at all. These are **not** parity ports — a different
methodology and effort tier from the table above.