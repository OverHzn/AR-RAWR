//! Faithful ports of the Go `encoding/*` helpers the ARTEX units rely on.
//!
//! - [`base64_std_decode`] = `base64.StdEncoding.DecodeString` (standard alphabet,
//!   `=` padding). Returns `Err` on the same malformed inputs Go rejects (bad
//!   length, stray/invalid characters, misplaced padding).

const STD_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Go `base64.StdEncoding.DecodeString`.
///
/// Accepts a standard-alphabet, `=`-padded string whose length is a multiple of 4,
/// decoded one 4-character quantum at a time (1–2 trailing `=` in the final quantum).
/// Returns the decoded bytes, or `Err(())` for anything Go's `StdEncoding` rejects.
#[allow(clippy::result_unit_err)]
pub fn base64_std_decode(input: &str) -> Result<Vec<u8>, ()> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(());
    }

    let mut rev = [255u8; 256];
    for (i, &c) in STD_ALPHABET.iter().enumerate() {
        rev[c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i < bytes.len() {
        let q = &bytes[i..i + 4];
        i += 4;

        // Trailing '=' padding in this quantum (0, 1, or 2).
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i != bytes.len()) {
            return Err(()); // too much padding, or padding before the last quantum
        }
        // No data character may sit among the padding.
        if q[..4 - pad].contains(&b'=') {
            return Err(());
        }

        // Accumulate the 6-bit groups of the non-padding characters.
        let mut buf: u32 = 0;
        for &c in &q[..4 - pad] {
            let v = rev[c as usize];
            if v == 255 {
                return Err(()); // invalid character (also rejects CR/LF/space)
            }
            buf = (buf << 6) | u32::from(v);
        }

        // Left-align into a 24-bit window and emit the 3, 2, or 1 whole bytes.
        let aligned = buf << (24 - (4 - pad) * 6);
        for k in 0..3 - pad {
            out.push((aligned >> (16 - 8 * k)) as u8);
        }
    }
    Ok(out)
}

/// Go `base64.StdEncoding.EncodeToString`.
///
/// Standard alphabet, `=`-padded, encoding each 3-byte group to 4 characters
/// (the final group to 2 or 3 characters plus `=` padding). The inverse of
/// [`base64_std_decode`].
pub fn base64_std_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for c in &mut chunks {
        let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        out.push(STD_ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(STD_ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(STD_ALPHABET[((n >> 6) & 63) as usize] as char);
        out.push(STD_ALPHABET[(n & 63) as usize] as char);
    }
    match chunks.remainder() {
        [a] => {
            let n = u32::from(*a) << 16;
            out.push(STD_ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(STD_ALPHABET[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            out.push(STD_ALPHABET[((n >> 18) & 63) as usize] as char);
            out.push(STD_ALPHABET[((n >> 12) & 63) as usize] as char);
            out.push(STD_ALPHABET[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}