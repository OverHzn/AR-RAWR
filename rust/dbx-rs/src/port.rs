//! Rust port of the unexported, pure helper functions in the Go `db/` package
//! that are dependency-free (no `net/netip`, `net/url`, publicsuffix, or the DSL
//! parser). Shared Go-stdlib pieces would come from `gocompat`, but none are
//! needed here.

/// `nullIfEmpty` (`db/...`): `None` (JSON `null`) for the empty string, otherwise
/// the string itself — used to store a nullable text column.
pub fn null_if_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// `isIdentStart` (`db/asset_dsl.go`): whether byte `c` may start a DSL identifier
/// (`_`, `a`–`z`, `A`–`Z`).
pub fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

/// `isIdentChar` (`db/asset_dsl.go`): whether byte `c` may continue a DSL
/// identifier (an identifier start, or a digit `0`–`9`).
pub fn is_ident_char(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// `marshalStringArray` (`db/...`): render a Go `[]string` as a Postgres text[]
/// literal — empty → `{}`, otherwise `{"a","b"}`, with only `"` escaped to
/// `\"` (exactly Go's `strings.ReplaceAll(s, "\"", "\\\"")`).
pub fn marshal_string_array(items: &[String]) -> String {
    if items.is_empty() {
        return "{}".to_string();
    }
    let body: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.replace('"', "\\\"")))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// `nullableInt` (`db/...`): `None` (JSON `null`) for 0, otherwise the integer —
/// used to store a nullable integer column.
pub fn nullable_int(v: i64) -> Option<i64> {
    if v == 0 { None } else { Some(v) }
}
