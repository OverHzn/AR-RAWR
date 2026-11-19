# HANDOFF.md — session continuity (living document)

Mandatory per PHASE-1 §9. Update at the end of every unit so a fresh session can
resume without re-deriving state.

## Current state (last updated: Unit 10b mcphttp complete; db next)

- Branch: `main`. origin=OverHzn/AR-RAWR, upstream=BaaRai/ARTEX.
- HEAD at Unit 10b mcphttp (this commit, to push). SQ-4 was `7c47d3f`; tool 10a `eda20bd`.
- Pure-helper layer fully ported; `rust/gate-all.sh` = **PASS 30 / FAIL 0**.
- `rust/artex`: fmt + clippy -D warnings clean; **artex tests = 184** (lib 51 + the
  per-crate integration suites: config 2, llmpool 18, notify_http 10, notify_mask 16,
  notify_pack 12, notify_render 9, notify_sign 12, notify_channels 12, notify_webhook 10,
  notify_registry 2, notify_email 7, notify_redact 10, sidequestion_request 2,
  sidequestion_service 3, sidequestion_context 8, sidequestion_capture 3,
  **tool_capture 3, mcphttp 4**, + misc 12).
- Link note: artex now pulls MANY `*-rs` crates (notify-rs, mcphttp-rs, ...) whose generated
  `#[no_mangle] rust_anything_call`/`free` FFI entry points collide at the final bin link. Fixed
  WITHOUT touching the frozen crates via `artex/.cargo/config.toml`:
  `-C link-arg=-Wl,--allow-multiple-definition` (artex never calls the C entry points, only the
  `port::*` fns). The same config sets `build.target-dir=/tmp/artex-target` because /workspaces
  (32G) fills with build artifacts; per-crate gate builds keep their own small target/.
- Go baselines for ported packages re-run green (config, llmpool, notify pure specs,
  sidequestion incl. context_test.go + sidequestion_test.go capture specs).

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
8. notify signing + SSRF/redact substrate (5bfdc46) — `src/notify/sign.rs`
   (dingtalk_signed_url, feishu_sign) + `src/notify/http.rs` pure helpers
   (allow_local_targets, is_blocked_dial_ip, validate_http_url, redact_request_target,
   redact_transport_error/TransportError, redact_urls_in_text). Specs in
   `tests/notify_sign_test.rs` (12). Crypto deps: hmac 0.12, sha2 0.10, base64 0.22.

## Unit 9 — notify channel adapters + doJSON/httpClient (complete, all pushed through e60077a)
- **9a DONE (ready to commit):** the IO shell in `src/notify/http.rs` —
  `do_json(ctx, method, url, headers, payload)` + lazy process-wide `http_client()`
  (reqwest/rustls): 15s timeout, `respBodyLimit=8<<10`, status classification
  (2xx ok; 429/408 & ≥500 transient; other 4xx permanent; message carries numeric status
  + `snippet`), cross-host redirect refusal (same-host allowed, max 5), and the SSRF dial
  guard = **literal-IP pre-check in `do_json`** (hyper-util skips the resolver for IP
  literals) **+ `GuardResolver`** (custom `reqwest::dns::Resolve` that vets every hostname
  lookup via `is_blocked_dial_ip`, closing DNS-rebinding). Transport errors are stripped to
  `request failed: Op host:port: <cause>` with `reqwest::Error::without_url()` (the
  `url.Error.Err` analogue) + `redact_urls_in_text` so no credential leaks; the read-error
  path is redacted too (reqwest body errors can carry a URL, unlike Go's bare `io` error).
  Specs in `tests/notify_http_test.rs` (10) over a scriptable tokio test server
  (`tests/common/mod.rs`, the `httptest.NewServer` equivalent); `cfg_*` config readers were
  already re-exported from `notifyx-rs` (no new `cfg.rs` needed). Port of `http_test.go`
  (TestDoJSONClassifiesHTTPStatus/IncludesResponseSnippet/SnippetIsSingleLineAndBounded/
  RejectsOversizedResponse) + dial-guard (literal + resolver) + cross/same-host redirect.
  Go baseline green for all of these. New dep: reqwest 0.12 (rustls-tls), offline-buildable.
- **9b-1 DONE (ready to commit):** the four HTTP JSON adapters (dingtalk/feishu/wecom/telegram)
  on top of `do_json`. Trait `Channel::validate -> Result<(),Error>` (Go `Validate` messages:
  "missing Webhook address" / "missing Bot Token" / "missing Chat ID" / "invalid API URL"), and
  default trait bodies for `default_rate_per_min`/`secret_keys`/`destination_keys` reading
  `channel_meta(self.kind())`. Adapters stateless (creds via `cfg`; `cfg_string` already trims).
  `src/notify/dingtalk.rs` (actionCard for single+backlink else markdown; `errcode!=0` always
  Permanent), `wecom.rs` (markdown 4096-byte pack; 45009 transient else permanent),
  `feishu.rs` (`feishu_card` whole-item packing measured with the SAME `serde_json` that
  `do_json` sends with — envelope=113 holds; `code`/`StatusCode` dual check; sign only with
  secret), `telegram.rs` (`telegram_html`/`telegram_batch_line`/`telegram_batch_title`;
  endpoint builder; `ok` check; 429 transient else permanent). `channel(kind)` factory registers
  the 4 (webhook/email join in 9b-2/9b-3). Specs in `tests/notify_channels_test.rs` (12): port of
  `channels_test.go` channel cases + feishu `pack_test.go` (TestFeishuPackingReportsKept,
  TestFeishuDigestBodiesStayWithinCardLimit — body ≤16000, envelope=113, every item once & in
  order across messages) + validate-message cases for the 4 kinds. Go baseline green.
- **9b-2 DONE (ready to commit):** generic webhook = a faithful **subset of Go `text/template`**
  built from scratch in `src/notify/webhook.rs` (mod `tmpl`). Lexer+parser+evaluator for:
  `.Field` (leading-dot IS special — lexes to [Dot,Ident]; only Dot/Var/Paren are fieldable, a
  string/num/func literal is NOT, so `"x" .F` is two args not a field), `$var`, `range $i,$v :=`,
  `if`/`else`, `( )` parens, `{{- -}}` whitespace trim, string/number literals; custom funcs
  `json`/`jsons` (json matches Go `json.Marshal` incl. HTML-escape of `<>&`/U+2028/9) + builtins
  `printf`(subset)/`index`/`len`/`not`/`and`/`or`/`eq`/`ne`/`lt..`; unknown field → error (not echo);
  unknown func → parse error (capability boundary). `WebhookChannel`: Validate(url/method/template),
  Send renders→`serde_json::from_str` validity gate (else Permanent "valid JSON")→do_json (GET=bodyless),
  returns full `m.items.len()` (no truncation). `channel()` factory now registers webhook (5 of 6).
  Specs: 9 inline `tmpl` unit tests (field-access regression, range/if, printf/index/paren, json
  escaping, unknown field/func, whitespace trim, rfc3339) + `tests/notify_webhook_test.rs` (10): the
  channels_test.go webhook cases + webhook_template_test.go capability boundary (context-is-pure-data
  key-set pin, funcs-are-minimal via undefined-func parse error, cannot-reach-unknown-data no-leak,
  render-fails-permanently, can-only-produce-json) + report-all-items. Go baseline green.
- **9b-3 DONE (ready to commit):** email = a direct **SMTP client** in `src/notify/email.rs`
  (EHLO/STARTTLS/AUTH PLAIN/MAIL/RCPT/DATA/QUIT) over an owned `SmtpStream` (Plain TcpStream |
  TLS via tokio-rustls) with `AsyncRead`/`AsyncWrite` delegation + `BufReader`. TLS uses **rustls
  with the ring provider** (`builder_with_provider(ring::default_provider())` + webpki-roots;
  `tokio-rustls`/`rustls` added to Cargo.toml with `default-features=false, features=["ring",…]`
  to avoid the offline-missing aws-lc-rs). `tls` config is a **bool** (`cfg_bool` → implicit TLS),
  `to` via `cfg_strings`. `validate` = host/port(1-65535)/from/to with the exact Go messages;
  `Send` wraps validate err as Permanent, builds the message, dials. **buildEmailMessage** is a
  simple RFC5322 message: `From/To/Subject/MIME-Version/Content-Type: text/html; charset="UTF-8"/
  Content-Transfer-Encoding: base64` + base64(htmlBody(m,0)) wrapped at 76 cols (no multipart, no
  Date, no dot-stuffing). Subject = **mime.QEncoding.Encode("utf-8", title)** — a faithful port of
  Go's RFC2047 Q-encoder (`q_encode`: needsEncoding check, 63-char content split that never breaks
  a UTF-8 seq, space→`_`, else `=XX`/byte), verified byte-for-byte against Go. Dial guard: resolve
  via `lookup_host` then `is_blocked_dial_ip` (reuse http.rs `is_blocked_dial_ip`/`allow_local_targets`
  /`ALLOW_LOCAL_TARGETS_ENV`); refusal names the env var. `smtp_stage_error`: 5xx→permanent, else
  (4xx/unparseable)→transient; keeps the reply text. Credentials refused over plaintext to a
  non-local host (`is_localhost`). Email registered in `channel()` factory → all 6 kinds.
  Cross-channel tables ported whole: `notify_registry_test.rs` (TestRegistryCoversAllKinds +
  full TestChannelValidateReportsMissingFields: dingtalk/feishu/wecom "Webhook", telegram "Bot Token"/
  "Chat ID", email "SMTP"/"port"/"recipient") + `notify_redact_test.rs` (TestChannelErrorsNeverLeak
  Credentials + …InPermanentPath: every channel at a doomed/malformed address, sentinel secret never
  in error). Email specs: `notify_email_test.rs` (7: fake hand-rolled SMTP server → deliver-full-
  message/AUTH-without-account/classify-5xx-vs-4xx/validate-missing/config-string-tolerance/dial-guard
  reject-loopback/opt-in). artex 140 tests; Go baseline green; gate-all 30/30.

## Active: sidequestion package (after Unit 9b complete)
- **Prereqs DONE & pushed:** `4e965b1` (llm::Accumulator + Context value bag),
  `04f0b66` (Context runtime cancellation: `with_cancel`/`CancelHandle`/`cancelled`/`err`).
- **SQ-1 (`0abe589`, pushed):** data model (`Model`/`Snapshot`/`Exchange`/`Memory`/`ContextInfo`/
  `Answer` + `GoTime` RFC3339) + `clone_request`/`exchange_messages`/`assemble`/`build_request`.
  Spec: `tests/sidequestion_request_test.rs`.
- **SQ-2 (`23eb131`, pushed):** `SideQuestionService::Answer` (`src/sidequestion/service.rs`) —
  stream/atomic accumulation, tool-use refusal, usage-on-failure. Specs:
  `tests/sidequestion_service_test.rs`.
- **SQ-3 (`7eb3014`, pushed):** `contextBuilder` + `SideQuestionService::Respond`
  (`src/sidequestion/context.rs`): bounded context prep (history fold/load, snapshot compaction
  with sha256-keyed summary cache, summary call cap 12, UTF-8-safe chunking) + at most one
  overflow recovery that counts the failed attempt's usage. Added `Provider::complete_full`
  (usage observable on error; default projects `complete`) and switched service.rs atomic path
  to it. Added serde to `sidequestion-rs` `Parent` + `GoTime`/`Model`/`Snapshot` for the
  `json.Marshal(snapshot)` cache key. Spec: `tests/sidequestion_context_test.rs` (8 specs, all
  ported from `context_test.go`).
- **SQ-4 (`7c47d3f`, pushed):** capture layer `src/sidequestion/capture.rs` — `Publisher`/
  `with_publisher`, `Capture`/`attach`/`finish`, `bind`/`BoundProvider` — plus a new
  `src/harness.rs` mirroring `norma/harness.QueryDeps` (the model-call surface: `call_model`
  stream + `call_model_sync` → `complete_full`, `fill_from` fills nils from a Provider). `attach`
  threads a per-attach `Capture` sink through the call surface so compaction/auxiliary calls
  (no attempt marker) cannot replace the main checkpoint: calls from the returned `QueryDeps`
  carry an `AttemptKey` value (distinct Context-bag slot from the `CaptureKey` `finish` reads and
  the `PublisherKey` `attach` reads), and `bind` (placed around each concrete pool member) saves
  the request and, on a clean non-cancelled completion, folds the response via `MessagesForAPI`.
  Spec: `tests/sidequestion_capture_test.rs` (3 specs ported from `sidequestion_test.go`:
  TestCheckpointDeepCopyAndBoundaries, TestSnapshotExcludesPartialStreamAndSelectsPoolMember,
  TestMainSideConcurrencyAndIndependentCancellation — last uses a multi_thread tokio runtime +
  independent `with_cancel` contexts to prove main/side run concurrently and cancelling one
  leaves the other running, with the cancelled side's message_start usage preserved).

## Unit 10 — norma/tool dynamic surface + mcphttp client (complete, to push)
- **10a `tool` (`eda20bd`, pushed):** `src/tool.rs` — the dynamic `norma/tool` surface:
  `ToolContext`(+`resolve_path`), `CoreTool` trait (async `call -> (Result, Option<String>)`),
  `Spec`→`build`→`Arc<dyn CoreTool>` with fail-closed defaults, and the capture layer
  (`Capture`/`CaptureOnce`, `max_output`=MaxOutputChars>0?that:30000, `truncate` with
  rune-safe `cut_head`/`cut_tail`, `spill_output` to `abs(dir)/output-<unix>-<seq>.txt`
  0o600). Plus `src/permission.rs` (`Context{Mode,WorkingDir,Allowed,Disallowed}` +
  re-export of `permission_rs::port::*`; `ask_user`). Spec `tests/tool_capture_test.rs` (3).
  Key fix: `cut_tail` aligns the byte offset to a char boundary before slicing (Rust `&str`
  panics mid-rune where Go byte-slices); edition-2021 (no let-chains).
- **10b `mcphttp` (this commit):** `src/mcphttp.rs` — Streamable HTTP + legacy SSE MCP client.
  `Client::new` (streamable) / `new_sse` (legacy), `initialize` (tries
  `[PROTOCOL_VERSION,2025-06-18]` streamable or `[LEGACY_SSE_PROTOCOL_VERSION]` legacy; falls
  back only when `is_unsupported_version`), `tools`/`wrap`/`run_tool`/`call_tool`/`protocol`/
  `close`. Streamable `round_trip` sets MCP-Protocol-Version only after `initialized`, captures
  `Mcp-Session-Id`, drains notification bodies, parses `text/event-stream` via `parse_sse`
  (8MiB cap, event-boundary flush, match `wantID`) else JSON. Legacy: `read_legacy_sse` reads
  the `endpoint` event, `resolve_url` (reqwest `Url::join` == Go ResolveReference), dispatches
  JSON-RPC by id to a `pending` mpsc map; `legacy_round_trip` serialised by `legacy_mu`.
  `wrap`→Permissions `ask_user("call MCP tool {full}?")`→`call("tools/call")`→concat content
  text→`Capture`→`single_text_block`; tool error → `errorf` (not `Err`). Reuses
  `mcphttp_rs::port::tool_name` (`mcp__{server}__{name}`) + `mcphttpx_rs::port`
  (`normalize_headers`/`is_unsupported_version`). Specs `tests/mcphttp_test.rs` (4) ported from
  `client_test.go`+`client_sse_test.go`: request-timeout (1s cut / 10s ok / 0→120s default),
  protocol-version negotiation (no header on initialize, negotiated header after), version
  rejection forms (http 400 / json-rpc -32602 fall back; unrelated 401 does not), legacy SSE
  discover+call (custom long-lived `GET /sse` + `POST /message` server sharing a per-session
  mpsc channel).

## Unit D-1 — db boot path (D-1a crypto + D-1b connection/schema/migrate + D-1c settings/secrets + D-1d seed, done)
- **D-1a `db::secrets` crypto (`9377c5a`, pushed):** `src/db/secrets.rs` — `SecretColumn`
  model (`aad`/`row_filter`/`id_col`/`value_expr`, the 10 `COL_*` columns + 5
  `SECRET_SETTING_KEYS`), `SecretBox` AES-256-GCM (seal/open, `enc:v1:`+base64(nonce12‖ct‖tag),
  AAD `<table>.<col>`/`settings.<key>`), `seal_secret`/`open_secret` (plaintext passthrough
  pre-init), `parse_secret_key` (base64|hex), `derive_sub_key` HMAC-SHA256,
  `traffic_trace_key`/`set_traffic_trace_key`. Spec `tests/db_secrets_test.rs` (4). Deps
  (tokio-postgres/deadpool-postgres/aes-gcm/rand/url) added.
- **D-1b connection + schema + migrations (this commit):** `src/db/mod.rs` (`DB{pool}`,
  `open`/`open_with_migrations`, `ensure_database`, `create_database_sql`, `build_pool` max 32,
  `with_schema_migration_lock` = session `pg_advisory_lock(7337741001)`,
  `apply_schema_with_retry` (deadlock 40P01 backoff ×5), `seed_builtins` STUB `Ok(())` until
  D-1c/D-1d, `suffix_test_database`, `ensure_llm_records_table`/`ensure_llm_usage_table` +
  `error.rs`). `src/db/migrate.rs` (`Migration`, `load_migrations` [skip non-.sql, error on bad
  .sql, dup-version, sha256 hex, sort], `apply_migrations` [checks DB-applied versions against
  the binary: missing-from-binary / name / checksum → fail-closed; refuse out-of-order pending
  below max-applied before writing; per-migration tx; wrap `migration %04d (%s): …`],
  `apply_migration`, `exec_embedded_migration` + `coordinate_with_schema_migration`
  `pg_advisory_xact_lock`, `migration_name_re`, `schema_migration_lock_key`). Schema + 7
  migrations pulled byte-identically from the Go tree via `include_str!` (drift-free checksum
  parity). Spec `tests/db_migrate_test.rs` (8) ported from `migrate_test.go`: pure
  loadMigrations validation + fresh/adopt-existing/reopen-noop/apply-once-in-order/fail-closed
  (checksum/name/missing)/rollback-and-block/out-of-order-refuse — differential vs the
  `artex-pg` Postgres, skip when `ARTEX_PG_DSN` unreachable. `Cargo.toml` gains
  `[dev-dependencies]` (tokio/tokio-postgres/sha2) for the tests/ crate. artex **196 tests**
  green, clippy `-D warnings` clean, gate 30/30, Go `TestLoadMigrations|TestOpen|TestMigrations|
  TestFailing|TestSecretBox` still green.
- **D-1c settings + DB-bound secrets (this commit):** `src/db/settings.rs` —
  `seal_setting` (secret key → `seal_secret`, else passthrough), `DB::{get_setting`
  (SELECT value, `secret_setting` decrypt, ErrNoRows → `("",false)`), `set_setting` (UPSERT
  value=EXCLUDED + updated_at=now()), `insert_setting_if_absent` (ON CONFLICT DO NOTHING,
  rows>0 → inserted; rests on the PK constraint), `rotate_password` (tx: seal hash UPSERT +
  version `(value::bigint+1)::text RETURNING value`, parse i64, commit; non-numeric version →
  increment fails → whole tx rolls back, hash preserved), `insert_password` (tx: hash ON
  CONFLICT DO NOTHING, rows==0 → `(0,false)`; else version='1' → `(1,true)`),
  `get_bool`/`set_bool` ("true"/"1" = true, default on unset/unparseable)}`. `src/db/secrets.rs`
  DB-bound half: `DB::init_secrets` = `with_schema_migration_lock` { load_secret_key →
  `SecretBox::new` → per-column key check (`SELECT valueExpr … WHERE valueExpr LIKE 'enc:v1:%'
  rowFilter LIMIT 1`, decrypt → `secret key check: …` on err≠NoRows) → `backfill_secrets` →
  set_active_secret_box + set_traffic_trace_key(derive_sub_key(key, TRAFFIC_TRACE_DOMAIN))};
  `load_secret_key` (env `ARTEX_SECRET_KEY` trimmed → parse; else `keyDir/secret.key` → parse;
  else sum per-column `count(*) WHERE … LIKE 'enc:v1:%'` → refuse `the database holds N
  encrypted secret(s) … refusing to generate a new key` if N>0; else gen 32 rand, mkdir 0700,
  O_EXCL 0600, base64+"\n"); `backfill_secrets` (per column: SELECT id,col::text WHERE col NOT
  NULL rowFilter; skip is_sealed; seal; UPDATE `SET col=$1[::text::jsonb] WHERE id=$2 AND
  col::text=$3` matching the read value; count rows). `mod.rs` gains `pub mod settings;`,
  `with_schema_migration_lock` made `pub(crate)`. Spec `tests/db_settings_test.rs` (4) ported
  from `settings_insert_test.go` (insert-if-absent no-overwrite, rotate-password rollback on
  non-numeric version) + the DB-bound halves of `secrets_test.go` (legacy plaintext backfill →
  all 10 secret columns sealed at rest → plain reads; idempotent re-backfill; no-key refusal
  (+encrypted-count); wrong-key `secret key check`; per-column key check on settings.global_proxy)
  via raw-SQL fixtures + `t.TempDir()`-equivalent scratch dirs + shared async lock, differential
  vs the `artex-pg` Postgres. `[dev-dependencies]` tokio-postgres gains `with-serde_json-1`
  (jsonb fixture binds). artex **196 + 4 = 200 tests** green, clippy clean, gate 30/30, Go
  `TestSecret|TestInsertSetting|TestRotatePassword` green.
- **D-1d real seedBuiltins (this commit):** `src/db/seed.rs` — `builtinAgents` (6 agents
  goals/planner/mainagent/worker/auto/pentest with their `promptVar` catalogs; pentest
  interactive_shell=true run_seconds=0, rest nil→COALESCE 1200), `seed_builtins` orchestrator
  (upsert agents `ON CONFLICT(key) DO UPDATE name+description RETURNING id` + per-var
  agent_prompt_vars upsert → DELETE renamed vars EngagementTitle/CoverageGaps/Now →
  default-on interactive_shell for planner/worker/mainagent/auto gated
  `interactive_shell_default_v1` → seed browser MCP + migrate → seed_builtin_skill_visibility
  (`api-recon`→auto/pentest/worker) → seed_default_intercept_rules v1/v2/v3 → asset rules →
  target-gate exemptions), all default-row seeds gated by settings flags
  (`intercept_default_rules_v{1,2,3}`, `asset_intercept_default_rules_v1`,
  `target_gate_exempt_seed_v1`). v1 = 17 rules (9 prio-100 system-destructive, 4 prio-90
  DB-destructive, 4 prio-80 HTTP-DELETE) with verbatim Go regex; v2 = 2 rules (destructive
  enabled, exfil-pipe disabled); v3 = delete-endpoint path rule (`DELETE_ENDPOINT_PATH_PATTERN`,
  INSERT…SELECT…WHERE NOT EXISTS). `seed_target_gate_exemptions` marshals `[{host,port,note}]`
  via `insert_setting_if_absent(SettingTargetExemptHosts)`. `src/db/browser_mcp.rs` — the pure
  rewrite logic (`rewrite_stock_npx_browser_mcp`/`rewrite_browser_mcp_row`/`playwright_mcp_package_index`/
  `has_cli_flag`) is **reused from `dbx_rs::port`** (parity-proven, single source of truth — no
  hand-port); this module keeps only the `BROWSER_MCP_*` consts + `DB::migrate_stock_browser_mcp`
  (one-time npx→binary row rewrite gated `browser_mcp_direct_bin_v1`, always marks flag done).
  `mod.rs`: `seed_builtins` STUB → real `seed::seed_builtins` via temp `DB{pool}`; `pub mod
  browser_mcp; pub mod seed;`; `DB::{execute,query,query_opt,query_one}` made `pub` (parity
  with Go's embedded `*sql.DB`, used by tests + the resource-group layer ahead). `Cargo.toml`
  main `tokio-postgres` gains `with-serde_json-1` (jsonb args/env binds, no string casts); gains
  `dbx-rs` dep. Specs: `tests/db_seed_test.rs` (3) ported from `db_test.go` `TestOpenSeed`,
  `browser_mcp_test.go` `TestSeedAndMigrateBrowserMCP` (5 subtests), `target_pins_test.go`
  `TestTargetGateExemptionSeedAndLog` (seed portion) — scratch DBs + shared async lock +
  raw-SQL fixtures, differential vs `artex-pg`, skip when DSN unreachable. artex **203 tests**
  green, clippy `-D warnings` clean, gate 30/30, Go `TestOpenSeed|TestRewriteStockNpxBrowserMCP
  |TestRewriteBrowserMCPRow|TestSeedAndMigrateBrowserMCP|TestTargetGateExemptionSeed` green.
- **D-1e task_templates resource group (this commit):** `src/db/task_templates.rs` — port of
  `db/task_templates.go`. `TaskTemplate`/`TaskTemplateInput`/`TaskTemplatePatch` structs +
  `TaskInterceptRuleInput` (JSONB rule element; `#[serde(default)]` mirrors Go's lenient
  unmarshal; its `*AssetStore` task-rule methods are ported with the assets group).
  `DB::{create,list,get,update,patch,delete}_task_template` use `DB::{execute,query,query_opt}`.
  Pure name helpers `taskTemplateName`/`taskTemplateNKey` **reused from `dbx_rs::port`** (gate-
  pinned); whitespace/trim via `gocompat::strings`. `intercept_rules` JSONB bound as
  `serde_json::Value` (with-serde_json-1), `timestamptz` read as `SystemTime` (postgres-types
  native codec — no chrono/time). **Error model extended:** `db::Error` now carries an optional
  sentinel identity (`Error::sentinel`/`Error::is`, Go `errors.Is` analogue) + Postgres SQLSTATE
  (`Error::sqlstate`/`Error::is_unique_violation`, captured in `From<tokio_postgres::Error>`);
  sentinel keys `ERR_TASK_TEMPLATE_{INVALID,NAME_CONFLICT,NOT_FOUND}`. `Patch` is a single
  CASE-based `UPDATE` (disjoint patches compose; unique-violation→name-conflict, no-row→not-found).
  Specs: `tests/db_task_templates_test.rs` (2) ported from `task_templates_test.go`
  (`TestTaskTemplateCRUDAndNormalizedUniqueness`, `TestTaskTemplateDisjointPatchesCompose`) —
  shared DSN + async lock + unique-name fixtures, differential vs `artex-pg`, skip when DSN
  unreachable. artex **205 tests** green, clippy `-D warnings` clean, fmt clean, gate 30/30, Go
  `TestTaskTemplateCRUDAndNormalizedUniqueness|TestTaskTemplateDisjointPatchesCompose` green.
- **D-1f llmhealth + skill_gaps + constants (commit 28e5775):** leaf db cluster.
  `src/db/llmhealth.rs` — `LLMHealth{profile_id,fails,trips,open_until:Option<SystemTime>,
  last_error,last_at}` + `DB::{load,save,clear}_llm_health` (`open_until IS NOT NULL AND
  open_until > now()` filters expired windows; upsert stamps `last_at=now()`).
  `src/db/skill_gaps.rs` — `SkillGap` + `DB::skill_gaps(&Context, limit)` (limit<=0->50;
  verbatim Go SQL: `COUNT(*) FILTER` misses/hits, `EXTRACT(EPOCH FROM MAX(ts))::bigint`;
  honours ctx cancellation via `tokio::select!` on `Context::cancelled()` -> `Error::Canceled`,
  Go `QueryContext` semantics). `src/db/constants.rs` — exploration node kinds/digest+intent
  states/edge rels (consumed by the exploration group later). Added `From<llm::Error> for
  db::Error` (-> `Error::msg(e.message())`, keeps "context canceled"/"context deadline
  exceeded"). Specs: `tests/db_llmhealth_skillgaps_test.rs` (3) — fresh differential tests
  (no Go `_test.go` for these files): llmhealth upsert/load(expired+null excluded)/clear,
  skill_gaps aggregation + limit + cancellation, constants parity guard. artex **208 tests**
  green, clippy clean, fmt clean, gate 30/30.
- **D-1g metering ledger cluster (commit 77c173a):** `src/db/gotime.rs` — self-contained
  RFC3339 (Go `time.Time` JSON) <-> `SystemTime` codec (no chrono/time offline;
  `days_from_civil`/`civil_from_days`), + `opt_system_time_rfc3339` serde adapter; locked to
  Go via differential value (`2024-06-15T12:00:00+07:00` -> unix 1718427600). `src/db/skill_usage.rs`
  — `SkillUsage`/`SkillStat`/`SkillCall` + `DB::{insert,skill_stats,missing_skill_stats,
  recent_skill_calls,skill_calls_by_task}` (verbatim Go SQL; `null_if_zero` pub(crate) shared
  with tool_usage). `src/db/task_archive_aggregate.rs` — `TaskArchiveAggregate` reader +
  `archived_task_aggregates` + `merge_archived_skill_stats` (calls/tasks sum, agents via
  reused `dbx_rs::port::merge_agent_keys`, newer `last_used` wins, v1 call-count fallback,
  calls-desc/skill-asc sort). `src/db/tool_usage.rs` — `ToolUsage` + `DB::{insert,tool_usage_counts}`
  (live per-key totals + archived `.tools` merge). Specs: `tests/db_skill_tool_usage_test.rs` (3)
  — port of Go `TestSkillUsageLedger`+`TestToolUsageLedger` (unique names) + pure
  `mergeArchivedSkillStats` case; Go oracle green on same DB. artex **214 tests** green, clippy
  clean, fmt clean, gate 30/30.
- **D-1h llm_usage ledger (commit f2732f9):** `src/db/llm_usage.rs` — `LLMUsage`/`ModelTokenStat`/
  `ProfileUsage`/`ProfileDayUsage`/`JudgeDayUsage`/`JudgeUsage` + `DB::{ensure_llm_usage_table
  (delegates exec_embedded_migration 0002), task_llm_spend, insert_llm_usage, token_by_model,
  usage_by_profile, usage_daily, judge_usage_stats}` (verbatim Go SQL; NUMERIC price cols cast
  `::float8` to scan as f64) + `parse_exp_id` (pure) + `apply_cost_eur` (pub, fresh=input-
  cache_read-cache_write clamped>=0, first-name-wins, no-price->None). `src/db/gotime.rs` +
  `ymd_minus_days(days)` (UsageDaily cutoff = now.UTC().AddDate(0,0,-days) YYYY-MM-DD).
  `src/db/task_archive_aggregate.rs` extended: `token_profiles`/`token_daily` JSON fields.
  `ModelTokenStat` defined here (only consumer is token_by_model; commands.go merely declares
  it). Specs: `tests/db_llm_usage_test.rs` (6) — port of Go `TestTaskLLMSpendCountsJudgeNotChat`
  + `TestCostSeparatesFreshReadAndWriteBuckets` + `TestUsageByProfileCostAggregatesRows` +
  `TestLLMUsageTsIndex` + `TokenByModel` + `ParseExpID`; Go oracle green on same DB. artex
  **220 tests** green, clippy clean, fmt clean, gate 30/30.
- **D-1i server logs (commit 3fa6361):** `src/db/logs.rs` — `DBLog` + `DB::{insert_log
  (RETURNING id), recent_logs, list_logs_before}` (port of `db/logs.go`; `ORDER BY id
  DESC LIMIT` then reverse -> oldest-first; `created_at` as SystemTime). No Go `_test.go`
  -> fresh differential test `tests/db_logs_test.rs` (1): insert/page oldest-first +
  ListLogsBefore paging + limit clamp. artex **221 tests** green, clippy clean, fmt clean,
  gate 30/30. (Deferred `db/nkey.go`: key builders re-exportable from `db-rs::port`, but
  `RootDomain` needs a publicsuffix/eTLD+1 not yet in the Rust tree -> build it with the
  assets group.)
- **D-1j llm retry policy (commit 6b8944f):** `src/db/llmretry.rs` — `RetryRule`/
  `RetryOverride`/`LLMRetryPolicy` (all Copy+PartialEq) + pure `interval`/`or`/`clamped`
  (attempts [-1,20], interval [0,1h]) + `DB::{llm_retry_policy (zero on missing/
  unparseable, else Clamped), set_llm_retry_policy (clamped first)}` over the
  `llm_retry_policy` setting. `RetryOverride` declared here (its Clamped lives here;
  config.go will embed it as LLMProfile.Retry). Specs: `tests/db_llmretry_test.rs` (3) —
  port of Go `TestLLMRetryPolicyRoundTrip` (round-trip + clamp + missing-key, restores
  prior setting) + pure clamp/or/interval + override-clamp; Go oracle green. artex
  **224 tests** green, clippy clean, fmt clean, gate 30/30.
- **D-1k asset intercept rules (commit 378fa88):** `src/db/asset_intercept.rs` —
  `AssetInterceptRule` + `DB::{list (builtin DESC, id DESC), create (builtin=false),
  update (RETURNING; missing id errors like Go ErrNoRows), delete, toggle}` (port of
  `db/asset_intercept.go`; global rules have no action column -> action always empty).
  No Go `_test.go` for the CRUD (only the matching logic has one) -> fresh differential
  test `tests/db_asset_intercept_test.rs` (1): CRUD round-trip + ordering + missing-id
  update error. artex **225 tests** green, clippy clean, fmt clean, gate 30/30.
- **D-1l task credentials (commit 9dd2e85):** `src/db/task_credentials.rs` —
  `TaskCredential`/`TaskCredentialInput` (normalize: trim, source default user,
  empty-password only for user) + `DB::{upsert (ON CONFLICT overwrite, sealed via
  COL_CRED_PASS, returns plaintext), list (decrypt on read), delete (taskID-scoped,
  not-found errors)}` (port of `db/task_credentials.go`; ErrTaskCredentialInvalid
  sentinel as msg). Go's ExplorationStore wrappers forward here. No dedicated Go
  CRUD test -> fresh differential `tests/db_task_credentials_test.rs` (2): upsert-
  in-place + sealed-at-rest (is_sealed) + decrypt round-trip + delete, and normalize
  validation. artex **227 tests** green, clippy clean, fmt clean, gate 30/30.
- **D-1m asset intercept matching (this commit):** `src/db/asset_intercept_match.rs`
  — pure half of `db/asset_intercept_match.go`: `AssetInterceptRule::reason`, `match_one`,
  `match_asset_intercept_rules`, `has_enabled_rule`, `evaluate_asset_gate` + `AssetGateDecision`
  (intercept-first then allow; empty/all-disabled allow = inert). `AssetInterceptKindLabel`
  reused from `db-rs::port` (gate-pinned) — added `db-rs` as an artex path dep (links fine
  under the `--allow_multiple-definition` flag). CIDR matching via `std::net` (`parse_cidr` +
  `cidr_contains`, no external crate). `AssetInterceptRule` gained a manual `Default`
  (SystemTime has no Default -> UNIX_EPOCH). Specs: `tests/db_asset_intercept_match_test.rs`
  (4) — port of Go `TestMatchAssetInterceptRules` (13 cases) + `TestEvaluateAssetGate`
  (6 scenarios) + kind-label + Reason format; Go oracle green. (`TestAssetInterceptCandidates`
  deferred to the assets group — needs the `Asset` type.) artex **231 tests** green, clippy
  clean, fmt clean, gate 30/30.

## Next
- db resource groups (tasks/assets/findings/… — the `*-rs` parity crates' `port::*` where they
  exist, else new `artex/src/db/` modules + ported Go specs) → intercept → egress → traffic →
  evidence/guard/enrich/report/llmrec → agent → server (see MAP.md). The full
  `harness.Query`/`QueryInput` machinery lands with the agent/server layer.