# rewritex-rs

Behavioral-parity Rust port of the **unexported, side-effect-free** helpers in the
Go `traffic/rewrite/` package that the `rewrite-rs` bridge leaves behind, verified
against the Go original via a recorded oracle (the bridge can't reach unexported
functions).

## Functions (5)

| Go | Signature | Notes |
|----|-----------|-------|
| `validToken` | `validToken(s string) bool` | non-empty RFC-7230 token (ASCII alnum + ``!#$%&'*+-.^_`|~``); byte-wise, so any non-ASCII fails |
| `nulInHead` | `nulInHead(_ string, body []byte) bool` | NUL byte within the first `min(len, 512)` bytes of `body` |
| `actionClass` | `actionClass(typ string) int` | execution class (`classTarget` 0 … `classStatus` 4) for an action type |
| `bodyAllowedForStatus` | `bodyAllowedForStatus(status int) bool` | false for 1xx, 204, 304 |
| `headerTarget` | `headerTarget(name string) (string, error)` | token-check → `http.CanonicalHeaderKey`; reject hop-by-hop / `Proxy-` headers |

These are pure and value-typed. `headerTarget` reproduces `net/http.CanonicalHeaderKey`
— which delegates to `net/textproto.CanonicalMIMEHeaderKey` (the ASCII header-byte
table + first-letter / post-dash upper-casing) — byte-for-byte, so the canonical
form and the error messages match the Go original exactly. The harness records the
`(string, error)` return as `[canonical, errString]` with `errString` empty on
success.

The rest of `traffic/rewrite/`'s unexported surface needs heavier ports or stays in
Go: `ruleSnapshot`/`sha256Hex` (JSON `Rule`/`Matchers` model + SHA-256, no `sha2`
crate offline), `expandReplace` (regexp), `headerLines`/`streamToolPreview` (in the
parent `traffic` package).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go traffic/rewrite/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`traffic/rewrite/` is not modified; `record.sh` cleans up its oracle.