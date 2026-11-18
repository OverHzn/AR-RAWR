# enrich-rs

Behavioral-parity Rust port of the **unexported, side-effect-free helpers** in the
Go `enrich/` package, verified against the Go original via a recorded oracle
(the bridge can't reach unexported functions).

## Functions (4)

| Go | Signature | Notes |
|----|-----------|-------|
| `id0` (enrich.go) | `id0(int64) bool` | `id <= 0` |
| `itoa` (enrich.go) | `itoa(int64) string` | `strconv.FormatInt(n, 10)` |
| `hostOf` (enrich.go) | `hostOf(string) string` | `url.Parse` + `u.Hostname()`; `""` on parse error |
| `uniq` (enrich.go) | `uniq([]string) []string` | order-preserving dedup |

`hostOf` reuses `gocompat::neturl` (`parse_url` + the `Url::hostname` /
`split_host_port` ports of Go's `net/url`).

## Loop

```sh
./record.sh   # regenerate cases.jsonl from the real Go enrich/ (deterministic)
./gate.sh     # fmt, clippy (-D warnings), parity tests
```

60 recorded cases. `enrich/` is not modified; `record.sh` cleans up its oracle.