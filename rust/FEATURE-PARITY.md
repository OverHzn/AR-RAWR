# FEATURE-PARITY.md — feature → Rust destination → test → status

Per PHASE-1 §3 / PHASE-02. Every discovered feature gets a row; none may disappear.
Status: `NOT_ANALYZED` · `ANALYZED` · `PLANNED` · `IN_PROGRESS` · `IMPLEMENTED` ·
`TESTED` · `VERIFIED` · `BLOCKED`. `VERIFIED` requires ported-Go-test green + (IO/stateful)
Go-baseline green.

The exhaustive A–O feature inventory (autonomous engine, dual graph, LLM infra, tool
exec, security controls, MITM proxy, browser extension, findings/evidence, retesting,
asset enrichment, MCP/skills/memory, reporting/export, notifications, web UI, deployment)
is catalogued in the unit rows below as each package is migrated. The packages not yet
touched remain `NOT_ANALYZED` and are scheduled in MAP.md dependency order.

## config (Wave 0/1 — foundation)
| id | feature | Rust dest | test | status |
|----|---------|-----------|------|--------|
| CFG-1 | config load/save, DSN, defaults, env | `src/config.rs` | `tests/config_test.rs` | VERIFIED |

## llmpool (Wave 3 — LLM infra)
| id | feature | Rust dest | test | status |
|----|---------|-----------|------|--------|
| LLM-1 | health registry: failover classification, status_of, trim_err, backoff ladder | `src/llmpool.rs` | `tests/llmpool_test.rs` | VERIFIED |
| LLM-2 | failover Pool: window filter, equal-rank round-robin, cancel/exhaustion barriers | `src/llmpool.rs` + `llm::Provider` | `tests/llmpool_test.rs` | VERIFIED |
| LLM-3 | provider profiles/rotation/streaming/token accounting (full) | `src/llm.rs` + future | — | ANALYZED |

## notify (Wave 7 — notifications)
| id | feature | Rust dest | test | status |
|----|---------|-----------|------|--------|
| NTF-1 | core types, Channel trait, Error/Context, Snapshot/Item/Message/Limits | `src/notify/mod.rs` | inline + `notify_render_test.rs` | VERIFIED |
| NTF-2 | severity/status labels, truncate bytes/runes/html, one_line, asset_line | (re-export notify-rs/notifyx-rs) | `notify_render_test.rs` | VERIFIED |
| NTF-3 | filter parse/match, masked_value/is_masked | (re-export) | `notify_render_test.rs`, `notify_mask_test.rs` | VERIFIED |
| NTF-4 | config masking + destination-swap guard (anti-exfiltration) | `src/notify/mask.rs` | `tests/notify_mask_test.rs` | VERIFIED |
| NTF-5 | markdown + html renderers, pack-whole-items digest (honest `kept`) | `src/notify/{markdown,html}.rs` | `tests/notify_pack_test.rs` | VERIFIED |
| NTF-6 | untrusted-content escaping (md links/beacons/emphasis; HTML &<> + href quotes) | `src/notify/{markdown,html}.rs` | `notify_pack_test.rs` | VERIFIED |
| NTF-7 | signing (DingTalk + Feishu HMAC variants) | `src/notify/sign.rs` | `notify_sign_test.rs` | VERIFIED |
| NTF-8 | SSRF dial guard (loopback/link-local/metadata deny; RFC1918 allow; ALLOW_LOCAL) | `src/notify/http.rs` | `notify_sign_test.rs` | VERIFIED |
| NTF-9 | credential redaction in error text (redact_request_target/transport/urls_in_text) | `src/notify/http.rs` | `notify_sign_test.rs` | VERIFIED |
| NTF-10 | doJSON + httpClient (status classification, cross-host redirect refusal, body cap) | `src/notify/http.rs` | (porting `http_test.go`) | PLANNED |
| NTF-11 | channel adapters: dingtalk/feishu/wecom/webhook/telegram/email (+SMTP guard) | `src/notify/channels/` | `channels_test.go` etc. | PLANNED |

## Remaining packages — NOT_ANALYZED (scheduled)
| Go pkg | feature area | Rust dest | status |
|--------|--------------|-----------|--------|
| sidequestion | side questions (←llmpool) | `src/sidequestion.rs` | NOT_ANALYZED |
| mcphttp | MCP Streamable HTTP + SSE | `src/mcphttp.rs` | NOT_ANALYZED |
| db | repository/transactions/migrations/FGM graph | `src/db/` | NOT_ANALYZED |
| intercept | approval/interception rules | `src/intercept.rs` | NOT_ANALYZED |
| egress | egress proxy/profile | `src/egress.rs` | NOT_ANALYZED |
| traffic (+rewrite) | MITM proxy, capture, replay, rewrite | `src/traffic/` | NOT_ANALYZED |
| evidence | evidence quotes/integrity | `src/evidence.rs` | NOT_ANALYZED |
| guard | scope/deny/budget/noise | `src/guard.rs` | NOT_ANALYZED |
| enrich | asset enrichment / ScopeSentry | `src/enrich.rs` | NOT_ANALYZED |
| report | reports/export/proof-strength | `src/report.rs` | NOT_ANALYZED |
| llmrec | LLM usage records/cache metrics | `src/llmrec.rs` | NOT_ANALYZED |
| agent | autonomous engine, planner, workers, graph | `src/agent/` | NOT_ANALYZED |
| server | HTTP API, auth, SSE, UI embed, extension API | `src/server/` | NOT_ANALYZED |
| cmd/artex | entry point, setup wizard, daemon | `src/main.rs` | NOT_ANALYZED |

## Cross-cutting (PHASE-02 §B) — tracked for later waves
first-run setup token, admin claim, login/session lifecycle, password reset, login rate
limit, trusted proxy, company/user ownership, task categories/templates/relations/archives,
main-agent conversations, goal decomposition, judge/compactor roles, usage/cache accounting,
worker rotation/failover/exclusion, todolists, cross-worker trace, graph debounce, frontier
claiming/dispatch, scope-coverage denominator, target gate, interception attribution, task/
intent budgets, forced wrap-up/resume, noise profiles, scanner low-and-slow, prompt-injection
boundaries, MITM CA lifecycle, decompression + evidence-quote verification, extension auth/
capture, replay + binary handling, encrypted snapshots, reporting proof-strength, MCP HTTP/SSE,
web-search providers + encrypted config, ScopeSentry sync, scanner versions, skills persistence,
install/upgrade, daemon restart, reverse-proxy/SSE, cross-platform builds, release checksums/
SBOM/provenance. Each maps to its owning package row above as that package is analysed.