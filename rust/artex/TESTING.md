# Testing the `artex` rewrite

The pure-helper parity crates (`rust/*-rs`) verify byte-for-byte against recorded
Go outputs. The application crate (`rust/artex`) is different: most of it is
IO/stateful (Postgres, HTTP, crypto), so it is verified by **porting the Go
`_test.go` specs into `tests/`** and, for the `db`/`server` layers, by running the
Go and Rust tests against the **same live Postgres** (differential testing) —
without running the ARTEX server itself.

## Unit tests (no services)

```sh
rust/artex/gate.sh      # cargo fmt --check + clippy -D warnings + cargo test
```

## Postgres-backed differential tests (db, …)

The Go tests read the DSN from `config.DSN()` (env `ARTEX_PG_DSN`), and skip when
no database is reachable. To exercise them (and later the Rust `db` port) against a
throwaway Postgres:

```sh
docker run -d --name artex-pg \
  -e POSTGRES_PASSWORD=artex -e POSTGRES_USER=artex -e POSTGRES_DB=artex \
  -p 5433:5432 postgres:16

export ARTEX_PG_DSN="postgres://artex:artex@localhost:5433/artex?sslmode=disable"

# Go side (baseline behaviour):
GOWORK=off go test ./db/ -count=1

# Rust side (port), once the db module lands:
# (cd rust/artex && cargo test --offline --features db)
```

This exercises the real schema/migrations/queries against a live database while
leaving the ARTEX application process itself untouched.

## Porting order (dependency-driven)

`config` → `llm` model (reuse `llm-rs`) → `db` → `intercept` → `egress` →
`traffic` → `guard`/`enrich`/`report`/`llmrec`/`evidence` → `agent` →
`notify`/`llmpool`/`mcphttp`/`sidequestion` → `server` → `cmd/artex`.