# DECISIONS.md — architecture decision records (living)

## D1 — One Go package → one `rust/artex` module
Each Go package maps to a module under `rust/artex/src/`. Pure, deterministic helpers
are NOT re-ported: they are **re-exported** from the oracle-parity `rust/*-rs` crates
which are already proven against the real Go. The `artex` module adds only what is not
pure data (traits, IO, the Go data model). This satisfies "reuse, not re-port" and keeps
manual parity out of the application crate.

## D2 — Verification = ported Go `_test.go` specs + differential vs Postgres
For each unit, the Go `_test.go` specs are ported to `rust/artex/tests/` and must be green.
For IO/stateful packages (db and friends), tests are additionally run against a live
Postgres (`ARTEX_PG_DSN`) and compared to the Go implementation's behavior; Go tests skip
when the DSN is unreachable. The Go original is never edited — parity is proven against it.

## D3 — Go → Rust type mappings
- `time.Time` → `std::time::SystemTime`; `time.Duration` → `std::time::Duration`.
- `context.Context` sentinels → `llm::Error::{Canceled, DeadlineExceeded}`; `ctx.Err()`
  modelled as a `Context` struct with `err: Option<Error>` and `is_cancelled()`.
- Go `map[string]any` config → `serde_json::Value` / `Map<String, Value>`.
- Go `[]string` secret/destination key sets → a single static `channel_meta()` table in
  `notify/mod.rs` (single source of truth).
- Go `error` sentinels (`Permanent`/`IsPermanent`) → `notify::Error::{Permanent,Transient,Msg}`
  with `is_permanent()`; plain Go `error` (transient) → `Error::Msg`.

## D4 — notify: stateless channels, creds via `cfg`
Channel adapters must be stateless (the same instance is shared concurrently across
configs); credentials always arrive through the `cfg` param, never as struct fields.
`SecretKeys` vs `DestinationKeys` are kept separate so a config edit cannot redirect the
stored real credentials to an attacker-controlled server (`prepare_config_update`).
`Send` returns the delivered-item count (not just error) so a truncated batch marks only
the first `kept` items delivered.

## D5 — notify IP/SSRF guard (Unit 8/9)
Deny-by-default for loopback / link-local (incl. 169.254.169.254 metadata) / unspecified /
multicast at the dial layer. RFC1918 private nets are **deliberately allowed** (self-hosted
relays are common). Escape hatch `ARTEX_NOTIFY_ALLOW_LOCAL=1`. Literal IPs are also checked
at config-save time. IPv4-mapped IPv6 is converted back before the check. `is_link_local_*`
/ `is_interface_local_multicast` are not in std and are implemented manually per Go `net.IP`.
Cross-host redirects are refused (credentials live in the URL); same-host allowed.

## D6 — notify signing algorithms differ (DingTalk vs Feishu)
DingTalk: `key = secret`, `message = ts + "\n" + secret`. Feishu: `key = ts + "\n" + secret`,
`message = ""`. Mirror images; must stay as two separate functions. Reference vectors
pinned from OpenSSL (not self-generated) in `sign_test.go`.

## D7 — email SMTP uses the same SSRF dial guard
The SMTP channel must install the same `is_blocked_dial_ip` Control hook as the HTTP
channels; a bare dialer was the historical SSRF gap (local 169.254/127.0.0.1 reach + the
peer's EHLO error echoed via `last_error`).

## D8 — Never run the ARTEX server
The user runs the Go original and the final server. The rebuild proves parity via tests
only; `cmd/artex`/`server` ports must not auto-launch a server during development.