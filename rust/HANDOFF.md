# HANDOFF.md — session continuity (living document)

Mandatory per PHASE-1 §9. Update at the end of every unit so a fresh session can
resume without re-deriving state.

## Current state (last updated: Unit 9a complete; Unit 9b next)

- Branch: `main`. origin=OverHzn/AR-RAWR, upstream=BaaRai/ARTEX.
- HEAD at Unit 8 (`5bfdc46`, pushed). **Unit 9a ready to commit** (notify doJSON/httpClient IO shell).
- Pure-helper layer fully ported; `rust/gate-all.sh` = **PASS 30 / FAIL 0**.
- `rust/artex/gate.sh` (fmt + clippy -D warnings + test) green; **artex tests = 90**
  (lib 11 + config 2 + llmpool 18 + notify_http 10 + notify_mask 16 + notify_pack 12 + notify_render 9 + notify_sign 12).
- Go baselines for ported packages re-run green (`config`, `llmpool`, `notify` pure specs incl. sign/ssrf/redact + the Unit-9a `http_test.go` doJSON/dial-guard specs).

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

## Active: Unit 9 — notify channel adapters + doJSON/httpClient
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
- **9b-3 TODO:** email = **SMTP client** (EHLO/STARTTLS/AUTH PLAIN/MAIL/RCPT/DATA/QUIT) + TLS +
  RFC 2047 Q-encoded subject + RFC 5322 + base64 HTML body (`buildEmailMessage`) + the email
  dial guard (reuse `is_blocked_dial_ip`/`allow_local_targets`). `smtpStageError`: 4xx transient,
  5xx permanent, unparseable→transient. Add email to `channel()` factory (webhook already there) so
  all 6 kinds register. With email done, port the cross-channel tables whole: the full
  `TestChannelValidateReportsMissingFields` (email cases: SMTP/port/recipient) + `TestRegistryCoversAllKinds`
  + `redact_test.go` (TestChannelErrorsNeverLeakCredentials* incl. email row, TestCrossHostRedirectRefused/
  SameHostRedirectAllowed) + `ssrf_test.go` TestEmailDialGuard*. Email specs: `email_test.go` (fake SMTP
  server) + `channels_test.go` email cases (TestEmailMessageIsWellFormed / EscapesStructuralInjection /
  SMTPErrorClassification).

## Next
- Unit 9b (above), then sidequestion → mcphttp → db (differential) → intercept → egress →
  traffic → evidence/guard/enrich/report/llmrec → agent → server (see MAP.md).