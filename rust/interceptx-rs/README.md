# interceptx-rs

Behavioral-parity Rust port of the **unexported, side-effect-free** helpers in the
Go `intercept/` package that the `intercept-rs` bridge leaves behind, verified
against the Go original via a recorded oracle (the bridge can't reach unexported
functions).

## Functions (11)

| Go | Signature | Notes |
|----|-----------|-------|
| `argvPrefixLen` | `argvPrefixLen(tokens []string) int` | longest token prefix that is a known command in `argvArity` (25-entry table), arity clamped to `len`, else `min(1,len)` |
| `argvProgramName` | `argvProgramName(tok string) string` | lower-cased `path.Base`; `mkfs.*`→`mkfs`; trailing digits/dots (after a letter) stripped |
| `isWord` | `isWord(c byte) bool` | `[A-Za-z0-9_]` |
| `endsToken` | `endsToken(b []byte, i int) bool` | end-of-input / non-word / `.` not followed by word |
| `bounded` | `bounded(s string, limit int) (string, bool)` | UTF-8-boundary truncation, with a `truncated` flag |
| `stripCodeFence` | `stripCodeFence(text string) string` | strip ```` ``` ```` fence + opening language-tag line |
| `validPort` | `validPort(s string) int` | 1–65535, else 0 |
| `riskLevel` | `riskLevel(score int) string` | `high`≥70, `medium`≥30, else `low` |
| `judgeActionLabel` | `judgeActionLabel(action string) string` | allow/block/`escalated to manual approval`/passthrough |
| `defaultMessage` | `defaultMessage(action, name string) string` | deny/ask rule message, else "" |
| `subset` | `subset(resolved, pinned []string) bool` | non-empty `resolved` ⊆ `pinned` |

Go string semantics (`path.Base`, `strings.ToLower`, `strings.TrimSpace`) come from
`gocompat`. The `argvArity` table is transcribed verbatim as a `match`.

The rest of `intercept/`'s unexported surface needs heavier ports or stays in Go:
`ArgvPrefix`/`AlwaysAllowPrefix`/`ArgvRuleMatches` (the `ParseSimpleCommand` shell
parser), `ruleMatches`/`digestInput` (`compiledRule` model + SHA-256),
`canonicalIPs`/`urlHosts`/`voidsExemption`/`exemptionHolds` (`netip`/target
models), `hasRequest`/`targetName`/`notCovered` (`TargetRequest`/`TargetScope`).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go intercept/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`intercept/` is not modified; `record.sh` cleans up its oracle.