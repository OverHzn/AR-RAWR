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

## Unit ported without the bridge (manual parity)

| Unit | Cases | Go source | Notes |
|------|------:|----------|-------|
| `server-rs`   | 5910 | `server/` (33 pure helpers) | unexported helpers the bridge can't reach; parity via a Go oracle, not `bridge/diff` |

`server-rs` ports the **unexported, side-effect-free helpers** of `server/`
(`humanBytes`, `firstLine`, `trunc`, `shellQuote`, `validatePassword`,
`skillPathRune`, …). `rust-anything bridge` only extracts exported, pointer-free
functions, and the three it finds in `server/` are all side-effectful
(`ScrubSecretEnv`, `StartLogCapture`, `WithSecretScrub`), so the bridge can't be
used here. Instead `record.sh` runs a temporary Go oracle over a corpus and
writes `cases.jsonl`; `tests/parity.rs` replays it and asserts byte-identical
output. Reproduce with `cd rust/server-rs && ./gate.sh`.

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

## Units not ported (no bridgeable pure functions)

The remaining Go packages are pointer/IO/concurrent-heavy and expose no pure,
value-typed, deterministically-recordable functions the harness can prove parity
for. The pure helpers that *do* exist in `server/` (unexported, so unreachable by
the bridge) are ported separately in [`server-rs`](server-rs) via a Go oracle.

- `server/` (27924 LOC) — 33 pure helpers ported in `server-rs`; the rest
  (stateful HTTP handlers, `*Server` methods over DB/receiver, 1952 pointers) stays
  in Go.
- `traffic/` (4282), `sidequestion/` (707), `llmrec/` (543), `llmpool/` (481),
  `evidence/` (399), `guard/` (330), `enrich/` (281), `cmd/artex/` (154) — no
  exported pure functions; their pure helpers (if any) are unexported.