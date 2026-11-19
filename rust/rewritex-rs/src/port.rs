//! Rust port of the unexported, pure helpers in the Go `traffic/rewrite/` package.
//!
//! Everything here is deterministic and value-typed: header-name token validation
//! and canonicalisation (a faithful reproduction of `net/http.CanonicalHeaderKey`
//! over `net/textproto.CanonicalMIMEHeaderKey`, including the ASCII-only header
//! byte table), the NUL-in-head scan, the action execution-class table, and the
//! body-allowed-for-status predicate.

// ---------------------------------------------------------------------------
// Batch 9ae: pure helpers in `traffic/rewrite/`.
// ---------------------------------------------------------------------------

/// `validToken` (`traffic/rewrite/...`): whether `s` is a non-empty RFC-7230 token
/// — ASCII alphanumerics plus ``!#$%&'*+-.^_`|~``. Iterates bytes, so any non-ASCII
/// byte fails (matching Go's `s[i]` loop).
pub fn valid_token(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.bytes().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    })
}

/// `nulInHead` (`traffic/rewrite/...`): whether the first `min(len, 512)` bytes of
/// `body` contain a NUL byte. (The ignored leading string argument of the Go
/// function is dropped — the result depends only on `body`.)
pub fn nul_in_head(body: &[u8]) -> bool {
    let n = body.len().min(512);
    body[..n].contains(&0)
}

/// `classTarget` (`traffic/rewrite/rule.go`): method and URL.
pub const CLASS_TARGET: i64 = 0;
/// `classDeleteHeader`.
pub const CLASS_DELETE_HEADER: i64 = 1;
/// `classHeader`.
pub const CLASS_HEADER: i64 = 2;
/// `classBody`.
pub const CLASS_BODY: i64 = 3;
/// `classStatus` (the default class).
pub const CLASS_STATUS: i64 = 4;

/// `actionClass` (`traffic/rewrite/rule.go`): the execution class of an action
/// type. Classes, in Go `iota` order: [`CLASS_TARGET`] 0, [`CLASS_DELETE_HEADER`]
/// 1, [`CLASS_HEADER`] 2, [`CLASS_BODY`] 3, [`CLASS_STATUS`] 4 (default).
pub fn action_class(typ: &str) -> i64 {
    match typ {
        "set_method" | "set_path_query" | "replace_url" => CLASS_TARGET,
        "delete_header" => CLASS_DELETE_HEADER,
        "set_header" | "replace_header" => CLASS_HEADER,
        "set_body" | "replace_body" => CLASS_BODY,
        _ => CLASS_STATUS,
    }
}

/// `bodyAllowedForStatus` (`traffic/rewrite/...`): whether a response body is
/// allowed for `status` — false for 1xx, 204 and 304.
pub fn body_allowed_for_status(status: i64) -> bool {
    !(100..=199).contains(&status) && status != 204 && status != 304
}

/// `textproto.validHeaderFieldByte` (`net/textproto/reader.go`): the ASCII header
/// field byte table (alphanumerics plus ``!#$%&'*+-.^_`|~``). Identical to
/// [`valid_token`]'s set, but on a single byte.
fn valid_header_field_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// `textproto.CanonicalMIMEHeaderKey` (`net/textproto/reader.go`), which
/// `http.CanonicalHeaderKey` delegates to: the quick canonical-encoding check,
/// then per-byte canonicalisation.
fn canonical_mime_header_key(s: &str) -> String {
    let bytes = s.as_bytes();
    // Quick check for canonical encoding.
    let mut upper = true;
    for &c in bytes {
        if !valid_header_field_byte(c) {
            return s.to_string();
        }
        if upper && c.is_ascii_lowercase() {
            return canonicalize(bytes);
        }
        if !upper && c.is_ascii_uppercase() {
            return canonicalize(bytes);
        }
        upper = c == b'-';
    }
    s.to_string()
}

/// `textproto.canonicalMIMEHeaderKey` (`net/textproto/reader.go`): upper-case the
/// first letter and every letter after a dash, lower-case the rest. (The Go
/// `commonHeader` lookup is a pure optimisation — its values equal the canonical
/// forms this loop already produces — so it is omitted.)
fn canonicalize(a: &[u8]) -> String {
    if a.is_empty() {
        return String::new();
    }
    // See if `a` looks like a header key. If not, return it unchanged.
    let mut no_canon = false;
    for &c in a {
        if valid_header_field_byte(c) {
            continue;
        }
        // Accept invalid headers with a space before the colon, but don't
        // canonicalize them (https://go.dev/issue/34540).
        if c == b' ' {
            no_canon = true;
            continue;
        }
        return String::from_utf8_lossy(a).into_owned();
    }
    if no_canon {
        return String::from_utf8_lossy(a).into_owned();
    }
    let mut out = a.to_vec();
    let mut upper = true;
    for b in out.iter_mut() {
        let mut c = *b;
        if upper && c.is_ascii_lowercase() {
            c -= b'a' - b'A';
        } else if !upper && c.is_ascii_uppercase() {
            c += b'a' - b'A';
        }
        *b = c;
        upper = c == b'-';
    }
    String::from_utf8(out).expect("header bytes are ASCII")
}

/// `headerTarget` (`traffic/rewrite/...`): validate `name` as a header token and
/// return its canonical form, unless it is a forbidden (hop-by-hop or proxy)
/// header — in which case an error message is returned. Success yields
/// `Ok(canonical)`; failure yields `Err(message)`.
pub fn header_target(name: &str) -> Result<String, String> {
    if !valid_token(name) {
        return Err("invalid header name".to_string());
    }
    let c = canonical_mime_header_key(name);
    match c.as_str() {
        "Host" | "Content-Length" | "Transfer-Encoding" | "Connection" | "Keep-Alive" | "Te"
        | "Trailer" | "Expect" | "Upgrade" => Err(format!("header {c} cannot be rewritten")),
        _ if c.starts_with("Proxy-") => Err(format!("header {c} cannot be rewritten")),
        _ => Ok(c),
    }
}
