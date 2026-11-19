# MAP.md — ARTEX Go → Rust migration map

Per-module migration status. One Go package → one `rust/artex` module (or reused
`rust/*-rs` oracle-parity crate). Status legend:

`NOT_STARTED` · `ANALYZED` · `IN_PROGRESS` · `IMPLEMENTED` · `TESTED` · `VERIFIED`

`VERIFIED` = ported Go tests green in Rust **and** (for IO/stateful pkgs) the Go
baseline re-run green. The final application is **not** certified until every row
is `VERIFIED` and no Go fallback remains (PHASE-1 §11, PHASE-02 §H).

## Reused oracle-parity crates (`rust/*-rs`)

These are NOT part of `rust/artex`; they are the behavioral oracle each wraps a
frozen Go source snapshot under `<crate>/go/` or `<crate>/oracle/`. `artex`
**re-exports** their proven logic instead of re-porting it.

| crate | wraps | artex consumer | status |
|-------|-------|----------------|--------|
| config-rs | config/ | config.rs | VERIFIED (gate) |
| llm-rs | llm oracle | llm.rs | VERIFIED (gate) |
| llmpoolx-rs | llmpool/ | llmpool.rs | VERIFIED (gate) |
| notify-rs | notify/ (pure leaves) | notify/mod.rs | VERIFIED (gate) |
| notifyx-rs | notify/ (pure config/escape) | notify/mod.rs, notify/mask.rs | VERIFIED (gate) |
| egress-rs, db-rs, intercept-rs, mcphttp-rs, report-rs, agent-rs, … | their Go oracles | (future units) | built (gate), not yet wired |

## Go source packages → `rust/artex` modules

| Go pkg | LOC | Rust destination | Status | Test | Notes |
|--------|-----|------------------|--------|------|-------|
| config | 224 | `src/config.rs` | VERIFIED | `tests/config_test.rs` | Unit 1 |
| (async+llm substrate) | — | `src/llm.rs` (+tokio) | VERIFIED | llmpool_test | Unit 2 |
| llmpool (registry) | 526 | `src/llmpool.rs` | VERIFIED | `tests/llmpool_test.rs` | Unit 3 |
| llmpool (failover Pool) | 526 | `src/llmpool.rs` + `llm::Provider` | VERIFIED | `tests/llmpool_test.rs` | Unit 4 |
| notify (core+Channel+mask) | 2925 | `src/notify/{mod,mask}.rs` | VERIFIED | `tests/notify_mask_test.rs`, `notify_render_test.rs` | Units 5–6 |
| notify (markdown/html render) | — | `src/notify/{markdown,html}.rs` | VERIFIED | `tests/notify_pack_test.rs` | Unit 7 |
| notify (sign+http+channels) | — | `src/notify/{sign,http}.rs` + adapters | IN_PROGRESS | `tests/notify_sign_test.rs` (sign+ssrf+redact) | Unit 8: sign+SSRF+redact substrate VERIFIED; channel adapters next |
| sidequestion | 761 | `src/sidequestion.rs` | NOT_STARTED | — | leaf (←llmpool) |
| mcphttp | 619 | `src/mcphttp.rs` | NOT_STARTED | — | leaf |
| db | 23639 | `src/db/` | NOT_STARTED | — | differential vs Postgres |
| intercept | 4328 | `src/intercept.rs` | NOT_STARTED | — | ←db,enrich |
| egress | 616 | `src/egress.rs` | NOT_STARTED | — | ←intercept |
| traffic (+rewrite) | 6814 | `src/traffic/` | NOT_STARTED | — | ←db,egress |
| evidence | 419 | `src/evidence.rs` | NOT_STARTED | — | ←db,traffic |
| guard | 357 | `src/guard.rs` | NOT_STARTED | — | ←db,intercept |
| enrich | 309 | `src/enrich.rs` | NOT_STARTED | — | ←db,egress,intercept |
| report | 605 | `src/report.rs` | NOT_STARTED | — | ←db |
| llmrec | 590 | `src/llmrec.rs` | NOT_STARTED | — | ←db |
| agent | 11002 | `src/agent/` | NOT_STARTED | — | ←many |
| server | 29580 | `src/server/` | NOT_STARTED | — | ←all |
| cmd/artex | 172 | `src/main.rs` | NOT_STARTED | — | entry point; do NOT run server |

## Dependency order (from Go import graph)

Leaf: `config`, `notify`, `llmpool`, `sidequestion`(←llmpool), `mcphttp`.
Then: `db`(←config,notify,sidequestion) → `intercept`(←db,enrich) → `egress`(←intercept)
→ `traffic`(←db,egress) → `evidence`,`guard`,`enrich`,`report`,`llmrec` → `agent` → `server`.

Cycle note: db↔intercept and db↔sidequestion are Go interface-broken cycles; resolve
in Rust with traits + `Arc<dyn>` injection (decide when db unit lands).