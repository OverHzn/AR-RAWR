//! JSONB sanitizer — pure port of `db/exploration.go:jsonbClean`.
//!
//! `json.Marshal` encodes a NUL as the six-byte escape `\u0000`; PostgreSQL's
//! `jsonb` rejects that escape (SQLSTATE 22P05). Captured tool/HTTP bytes can
//! carry NULs, so drop the escape. A *literal* `\\u0000` (doubled backslash)
//! must be left intact: a `\u0000` is genuine only when preceded by an even
//! number of backslashes.

/// Strip genuine `\u0000` escape sequences from marshaled JSON bytes.
pub fn jsonb_clean(b: &[u8]) -> Vec<u8> {
    if !b.windows(6).any(|w| w == b"\\u0000") {
        return b.to_vec();
    }
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    let mut bs = 0usize; // consecutive backslashes already emitted before i
    while i < b.len() {
        if b[i] == b'\\'
            && bs.is_multiple_of(2)
            && i + 5 < b.len()
            && b[i + 1] == b'u'
            && b[i + 2] == b'0'
            && b[i + 3] == b'0'
            && b[i + 4] == b'0'
            && b[i + 5] == b'0'
        {
            i += 6; // skip the whole \u0000
            bs = 0;
            continue;
        }
        if b[i] == b'\\' {
            bs += 1;
        } else {
            bs = 0;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}
