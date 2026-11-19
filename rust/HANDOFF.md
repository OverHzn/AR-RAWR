# HANDOFF.md — session continuity (living document)

Mandatory per PHASE-1 §9. Update at the end of every unit so a fresh session can
resume without re-deriving state.

## Current state (last updated: Unit 8 complete; Unit 9 next)

- Branch: `main`. origin=OverHzn/AR-RAWR, upstream=BaaRai/ARTEX.
- HEAD at Unit 7 (`c013d4f`). Units 1–7 pushed. **Unit 8 ready to commit.**
- Pure-helper layer fully ported; `rust/gate-all.sh` = **PASS 30 / FAIL 0**.
- `rust/artex/gate.sh` (fmt + clippy -D warnings + test) green; artex tests = 80 (68 + 12 sign/ssrf/redact).
- Go baselines for ported packages re-run green (`config`, `llmpool`, `notify` pure specs incl. sign/ssrf/redact).

### Environment
- Go go1.27.3; rustc/cargo 1.100.0. `export PATH="$HOME/.cargo/bin:/usr/local/go/bin:$PATH"`.
- Parity crates build/test `--offline`. Network available for new crates.
- Postgres differential harness: container `artex-pg` on :5433
  (`docker run -d --name artex-pg -e POSTGRES_PASSWORD=artex -e POSTGRES_USER=artex -e POSTGRES_DB=artex -p 5433:5432 postgres:16`);
  `export ARTEX_PG_DSN="postgres://artex:artex@localhost:5433/artex?sslmode=disable"`.
  Go db tests read this DSN and skip when unreachable.
- Push auth: extract `GITHUB_TOKEN` from `/proc/[0-9]*/environ` (never print it);
  `gh` not logged in. User `Hulk <overhorizonnn@gmail.com>`.
- `edit` tool intermittently rejects multi-line oldString ("Missing key"); fall back
  to `python3` heredoc / `write` for file edits.

## Completed units
1. `config` (6d891a5) — full port + tests.
2. async+llm substrate (004208d) — tokio runtime; `llm.rs` re-exports `llm-rs`.
3. llmpool registry (5a8c652) — should_failover/status_of/is_hard_failure/trim_err/backoff.
4. llmpool failover Pool + `llm::Provider` (38c3cc7) — Pool/order/round-robin/failover.
5. notify core (0f0c067) — re-exports + Channel trait + Error/Context/Snapshot/Item/Message/Limits.
6. notify mask (8b1fd17) — mask_config/merge_config/prepare_config_update (destination-swap guard).
7. notify markdown+html render (c013d4f) — markdown_body (pack-whole-items)/html_body etc.

## Active: Unit 9 — notify channel adapters + doJSON/httpClient
- Unit 8 DONE: `src/notify/sign.rs` (dingtalk_signed_url, feishu_sign) + `src/notify/http.rs`
  (allow_local_targets, is_blocked_dial_ip, validate_http_url, redact_request_target,
  redact_transport_error/TransportError, redact_urls_in_text). Specs ported in
  `tests/notify_sign_test.rs` (12 tests); Go baseline for all 11 ported pure specs green.
  Crypto deps added: hmac 0.12, sha2 0.10, base64 0.22 (fetched; offline-buildable now).
  NOTE: Rust `IpAddr` is non-nullable so Go's `isBlockedDialIP(nil)==true` (empty-string
  case) has no analogue — covered by call sites skipping non-literal hosts + the dial guard.
- Unit 9 TODO: build the IO shell (`doJSON`/`httpClient` via reqwest/hyper with a custom
  connect that calls `is_blocked_dial_ip`; 15s timeout, cross-host redirect refusal,
  `respBodyLimit=8<<10`, status classification 5xx/408/429 retryable vs other 4xx permanent,
  `snippet` via notifyx OneLine). Then the 6 adapters (dingtalk/feishu/wecom/webhook/
  telegram/email) with `feishuCard`/`telegramHTML` + the SMTP dial guard.
  Port: `channels_test.go`, `http_test.go` (doJSON/redirect), `pack_test.go` channel cases,
  `email_test.go`, `webhook_template_test.go`, channel cases in `redact_test.go`
  (TestChannelErrorsNeverLeakCredentials*, TestCrossHostRedirectRefused/SameHostRedirectAllowed,
  TestDialGuard*/TestEmailDialGuard*).

## Next
- Unit 9 (above), then sidequestion → mcphttp → db (differential) → intercept → egress →
  traffic → evidence/guard/enrich/report/llmrec → agent → server (see MAP.md).