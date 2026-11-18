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
| `mcphttp-rs`  | 76   | 585   | norma unit, direct |
| `egress-rs`   | 147  | 565   | norma transitif (intercept→db→norma) |
| `intercept-rs`| 361  | 4004  | 5 heavy funcs kept in Go (`--keep`) |
| `db-rs`       | 1525 | 22078 | 17 funcs kept in Go (`--keep`) |
| `agent-rs`    | 2418 | 10181 | 7 funcs kept in Go; `gate.sh` re-patches generated files |

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
| `traffic-rs`      | 91   | `traffic/` | `sanitize`, `ftsQuote`, `clip`, `boolInt`, `validHeaderName` |
| `enrich-rs`       | 47   | `enrich/`  | `id0`, `itoa`, `hostOf` |
| `guard-rs`        | 28   | `guard/`   | `capBlockReason`, `systemBlockMessage` |
| `sidequestion-rs` | 11   | `sidequestion/` | `Parent::key` |

In `server/`, the three functions the bridge *does* extract (`ScrubSecretEnv`,
`StartLogCapture`, `WithSecretScrub`) are all side-effectful (env/IO/closure), so
the bridge is unusable there. `enrich-rs`'s `hostOf` reuses `gocompat::neturl`
(`Url::hostname` / `split_host_port` added for it); `server-rs`'s `parseTaskID`
uses `gocompat::gojson::go_quote_string` (Go `%q`). `traffic-rs` excludes
`runTestHook` (mutates package-global `testRuleHook`); `sidequestion-rs` excludes
`Exchange::Running` (method on a large IO-laden struct).

## Reproducing a unit's gate

```sh
cd rust/<unit>
../ra gate .          # fmt, clippy, tests, unsafe, API, size, error-cases, build, diff
```

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

The remaining Go packages are pointer/IO/concurrent-heavy and expose no further
pure, value-typed, deterministically-recordable functions. Every pure helper that
*does* exist there (unexported, so unreachable by the bridge) is now ported via a
Go oracle: `server-rs`, `traffic-rs`, `enrich-rs`, `guard-rs`, `sidequestion-rs`
(see above). What stays in Go is the stateful / pointer / IO-heavy bulk:

- `server/` (27924 LOC) — 34 pure helpers ported in `server-rs`; the rest
  (stateful HTTP handlers, `*Server` methods over DB/receiver, 1952 pointers) stays
  in Go.
- `traffic/` (4282) — 5 pure helpers ported in `traffic-rs`; the stateful ingest /
  replay / stream machinery (and `runTestHook`, a global hook) stays in Go.
- `enrich/` (281) — 3 pure helpers ported in `enrich-rs`; the `*Engine` DNS / asset
  upsert workers stay in Go.
- `guard/` (330) — 2 pure helpers ported in `guard-rs`; the audit-log / decision
  layer stays in Go.
- `sidequestion/` (707) — `Parent::key` ported in `sidequestion-rs`; `Exchange`
  (large IO struct) and the request/replay flow stay in Go.
- `llmrec/` (543), `llmpool/` (481), `evidence/` (399), `cmd/artex/` (154) — no
  exported and no side-effect-free unexported helpers; fully pointer/IO/concurrent-
  bound. (`llmpool.State::Open` reads `time.Now`; not pure.)