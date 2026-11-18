# dbx-rs

Behavioral-parity Rust port of the **unexported, side-effect-free, dependency-free**
helpers in the Go `db/` package that the `db-rs` bridge and the primitive sweep both
leave behind, verified against the Go original via a recorded oracle (the bridge
can't reach unexported functions).

## Functions (5)

| Go | Signature | Notes |
|----|-----------|-------|
| `nullIfEmpty` | `nullIfEmpty(s string) any` | `""` → nil (`null`), else `s` |
| `isIdentStart` | `isIdentStart(c byte) bool` | `_`, `a`–`z`, `A`–`Z` |
| `isIdentChar` | `isIdentChar(c byte) bool` | identifier start, or `0`–`9` |
| `marshalStringArray` | `marshalStringArray([]string) string` | Postgres text[] literal; empty → `{}`, only `"` escaped to `\"` |
| `nullableInt` | `nullableInt(v int) any` | `0` → nil (`null`), else `v` |

These are pure and value-typed (no `net/netip`, `net/url`, publicsuffix, or the DSL
parser), so the whole dependency-free subset of `db/`'s unexported helpers fits in
one small crate. The byte predicates take a `byte`, which the harness records as its
numeric value; nil slices/scalars mirror Go's JSON `null`.

The rest of `db/`'s unexported surface needs heavier ports or stays in Go:
`calcCSegment`/`ValidateAssetIP`/`matchOne` (cidr) need `net.ParseIP`/`ParseCIDR`;
`parseURL`/`normalizeURL` need a faithful `net/url` round-trip; `tokenize`/
`readToken`/`ParseDSL` are the full asset-DSL parser.

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go db/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

`db/` is not modified; `record.sh` cleans up its oracle.