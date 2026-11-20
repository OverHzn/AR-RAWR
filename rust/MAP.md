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
| mcphttp-rs / mcphttpx-rs | mcphttp pure helpers | mcphttp.rs | VERIFIED (gate), wired |
| egress-rs, db-rs, intercept-rs, report-rs, agent-rs, … | their Go oracles | (future units) | built (gate), not yet wired |

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
| mcphttp | 619 | `src/mcphttp.rs` | DONE (`72ccc22`) | `tests/mcphttp_test.rs` (4) | leaf; Streamable + legacy SSE |
| db | 23639 | `src/db/` | IN_PROGRESS | `tests/db_secrets_test.rs` (4), `tests/db_migrate_test.rs` (8), `tests/db_settings_test.rs` (4), `tests/db_seed_test.rs` (3), `tests/db_task_templates_test.rs` (2), `tests/db_llmhealth_skillgaps_test.rs` (3), `tests/db_skill_tool_usage_test.rs` (3), `tests/db_llm_usage_test.rs` (6), `tests/db_logs_test.rs` (1), `tests/db_llmretry_test.rs` (3), `tests/db_asset_intercept_test.rs` (1), `tests/db_task_credentials_test.rs` (2), `tests/db_asset_intercept_match_test.rs` (4), `tests/db_chat_mentions_test.rs` (3), `tests/db_task_outcome_test.rs` (2), `tests/db_task_categories_test.rs` (2), `tests/db_exploration_test.rs` (1), `tests/db_exploration_nodes_test.rs` (2), `tests/db_exploration_intent_state_test.rs` (2), `tests/db_exploration_intent_softdelete_test.rs` (1), `tests/db_exploration_intent_cancel_test.rs` (1), `tests/db_exploration_reads_test.rs` (1), `tests/db_exploration_frontier_test.rs` (1), `tests/db_exploration_finding_test.rs` (1), `tests/db_exploration_intent_claim_test.rs` (1), `tests/db_exploration_stats_test.rs` (1), `tests/db_exploration_db_aggregates_test.rs` (1) | D-1a crypto + D-1b boot/migrate + D-1c settings/secrets + D-1d seed + D-1e task_templates + D-1f llmhealth/skill_gaps/constants + D-1g metering ledger (skill_usage/tool_usage/archive-aggregate + RFC3339 codec) + D-1h llm_usage ledger (usage/profile/daily/judge/cost) + D-1i server logs (oldest-first paging) + D-1j llm retry policy (clamped settings round-trip) + D-1k asset intercept rule CRUD (global blocklist) + D-1l task credentials (sealed-at-rest upsert/list/delete) + D-1m asset intercept matching (pure match/gate/CIDR) + D-1n chat mentions (catalog search + base64 cursor pagination) + D-1o task outcome card (aggregate baseline) + D-1p task categories (CRUD + transactional batch move) + D-1q exploration store skeleton (handle + root) + D-1r exploration nodes (AddNode/GetNode/state/anchor/link) + D-1s exploration intent state transitions (set/CAS/reopen/reset/count + claim TTL) + D-1t SoftDeleteIntent (state=deleted + reason + summary, conflict guard, side-session cleanup) + D-1u CancelIntent (hard-delete exclusive downward cascade + per-intent token rollup ledger) + D-1v exploration node/edge reads (Nodes/ListByKind/ListByKindPage/NodesPage+NodeFilter/NodesByIDs/Edges/EdgesTouching/FactsYielded) + D-1w exploration frontier/goals (Frontier/HasActiveIntent/HasOpenGoal/OpenGoalFingerprint/UpdateGoalPayload/DeleteGoal) + D-1x exploration finding reads (FindingLineage/FindingIntents/FindingIntentsTerminal) + D-1y exploration intent claim lifecycle (ClaimIntent/ClaimIntentToken/ClaimPausedIntentToken/HeartbeatIntentClaim/SettleIntentClaim/SettleIntentClaimBlocked) + D-1z exploration stats/token aggregation (Stats/TokenTotal/TokenStatsByWorker/TokenStatsBySession) + D-2a DB-level exploration aggregates (ExplorationDiag/TokenTotalsAll/LastActivityAll/GoalCountsAll) VERIFIED (differential vs Postgres, Go oracle green); next db resource groups |
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