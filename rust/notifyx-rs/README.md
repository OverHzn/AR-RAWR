# notifyx-rs

Behavioral-parity Rust port of the **unexported, side-effect-free config helpers**
in the Go `notify/` package, verified against the Go original via a recorded
oracle (the bridge can't reach unexported functions).

## Functions (7)

| Go | Signature | Notes |
|----|-----------|-------|
| `cfgString` (channel.go) | `cfgString(map[string]any, key) string` | trimmed string at `key`, or `""` if absent / not a string |
| `cfgInt` (channel.go) | `cfgInt(map[string]any, key) int` | JSON number truncates toward zero; string via `Atoi` (trimmed); else 0 |
| `cfgBool` (channel.go) | `cfgBool(map[string]any, key) bool` | bool, or trimmed+lowered string in {`true`,`1`,`yes`} |
| `cfgStrings` (channel.go) | `cfgStrings(map[string]any, key) []string` | non-empty trimmed strings in the array; single string accepted; nil → `null` |
| `cfgMap` (channel.go) | `cfgMap(map[string]any, key) map[string]string` | string→string, non-blank trimmed keys; non-object → `null` |
| `ValidKind` (channel.go) | `ValidKind(kind) bool` | membership in the channel `registry` |
| `Kinds` (channel.go) | `Kinds() []string` | registry kinds, sorted |

The `cfg*` helpers take a `map[string]any` (the notification config) — a
**composite map parameter** the bridge can't reach. The parity harness passes the
config as a JSON object (Go marshals map keys sorted, so recording is
deterministic) and reads keys from it. Go-stdlib string semantics come from
`gocompat::strings`; nil slices/maps mirror Go's JSON `null`.

Skipped from `notify/`: the senders (`dingTalkSignedURL`, `buildEmailMessage`,
`validateHTTPURL`, SMTP/HTTP delivery) use `time`/`net`/`smtp`, and `render*`
uses `text/template`.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go notify/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`notify/` is not modified; `record.sh` cleans up its oracle.