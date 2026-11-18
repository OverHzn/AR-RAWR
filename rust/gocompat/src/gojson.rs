//! Faithful reproductions of the Go `os`/`encoding/json` error-message shapes
//! (`*PathError` from `os.ReadFile`, `json.Unmarshal` scanner phrases) so a port
//! matches the original byte-for-byte when a config file is missing/unreadable/
//! malformed.

#![allow(clippy::result_unit_err)]

pub fn go_io_error(e: &std::io::Error, path: &str) -> String {
    // Mirror Go's os.ReadFile *PathError formatting.
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("open {}: no such file or directory", path),
        std::io::ErrorKind::PermissionDenied => format!("open {}: permission denied", path),
        _ => {
            // On Linux, reading a directory yields EISDIR (raw 21) → Go reports
            // it from the read step: `read <path>: is a directory`.
            if e.raw_os_error() == Some(21) {
                format!("read {}: is a directory", path)
            } else {
                format!("read {}: {}", path, e)
            }
        }
    }
}

/// Go `encoding/json` UnmarshalTypeError text (best-effort for the diff harness).
pub fn go_json_error(text: &str, e: &serde_json::Error) -> String {
    // Emulate Go encoding/json's `json.Unmarshal` error messages so the port
    // matches the original byte-for-byte when a config file is malformed.
    use serde_json::error::Category;
    let msg = e.to_string();

    // EOF (unexpected end of input) carries no offending character in Go.
    if e.classify() == Category::Eof || msg.contains("EOF") {
        return "unexpected end of JSON input".to_string();
    }

    // Map serde's syntax message to the Go scanner's context phrase.
    let context = if msg.contains("expected value") {
        "looking for beginning of value"
    } else if msg.contains("key must be a string") {
        "looking for beginning of object key string"
    } else if msg.contains("expected `:`") {
        "after object key"
    } else if msg.contains("expected `,` or `}`") {
        "after object key:value pair"
    } else if msg.contains("expected `,` or `]`") {
        "after array element"
    } else if msg.contains("trailing characters") {
        "after top-level value"
    } else if msg.contains("control character") || msg.contains("invalid escape") {
        "in string literal"
    } else if msg.contains("invalid number") {
        "in numeric literal"
    } else {
        "looking for beginning of value"
    };

    let ch = offending_byte(text, e);
    format!("invalid character {} {}", go_quote_byte(ch), context)
}

/// The byte serde points at via its 1-based (line, column), clamped to the input.
pub fn offending_byte(text: &str, e: &serde_json::Error) -> u8 {
    let bytes = text.as_bytes();
    let (line, column) = (e.line(), e.column());
    let mut cur_line = 1usize;
    let mut cur_col = 1usize;
    for (i, &b) in bytes.iter().enumerate() {
        if cur_line == line && cur_col == column {
            return b;
        }
        if b == b'\n' {
            cur_line += 1;
            cur_col = 1;
        } else {
            cur_col += 1;
        }
        let _ = i;
    }
    bytes.last().copied().unwrap_or(b' ')
}

/// Go's `%q` formatting for a single byte/rune in an error message.
pub fn go_quote_byte(b: u8) -> String {
    match b {
        b'\'' => "'\\''".to_string(),
        b'\\' => "'\\\\'".to_string(),
        b'\n' => "'\\n'".to_string(),
        b'\t' => "'\\t'".to_string(),
        b'\r' => "'\\r'".to_string(),
        0x20..=0x7e => format!("'{}'", b as char),
        _ => format!("'\\x{:02x}'", b),
    }
}

// ===========================================================================
// Go `encoding/json`-compatible syntax validation + quoting.
//
// `serde_json` reports different error text than Go's `encoding/json` (and it
// short-circuits on a type mismatch before validating the syntax). To reproduce
// Go's byte-for-byte error strings we run our own rune-based scanner that emits
// exactly the phrases Go's `scanner.go` does, then quote the offending rune the
// way `quoteChar`/`strconv.Quote` do.

/// Go `strconv.IsPrint` approximation (enough for error-quoting): letters,
/// marks, numbers, punctuation, symbols and the ASCII space are printable;
/// control, format (e.g. BOM), and non-ASCII spaces are not.
fn go_is_print(r: char) -> bool {
    let u = r as u32;
    if u == 0x20 {
        return true; // ASCII space only
    }
    if u < 0x20 || u == 0x7f {
        return false;
    }
    if (0x80..=0x9f).contains(&u) {
        return false; // C1 controls
    }
    if r.is_control() {
        return false;
    }
    // Format (Cf) characters, notably the BOM/ZWNBSP (U+FEFF) and bidi marks,
    // are not IsPrint in Go and are escaped as \u....
    if matches!(u,
        0xad | 0x600..=0x605 | 0x200b..=0x200f | 0x202a..=0x202e |
        0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb)
    {
        return false;
    }
    // Non-ASCII Unicode spaces (Zs/Zl/Zp) are not IsPrint in Go.
    if matches!(u,
        0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000)
    {
        return false;
    }
    true
}

/// Append the body of Go `strconv.Quote` for a single rune (no surrounding quotes).
fn append_escaped_rune(out: &mut String, r: char) {
    match r {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\u{7}' => out.push_str("\\a"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{b}' => out.push_str("\\v"),
        _ => {
            let u = r as u32;
            if go_is_print(r) {
                out.push(r);
            } else if u < 0x20 || u == 0x7f {
                out.push_str(&format!("\\x{:02x}", u));
            } else if u < 0x1_0000 {
                out.push_str(&format!("\\u{:04x}", u));
            } else {
                out.push_str(&format!("\\U{:08x}", u));
            }
        }
    }
}

/// Go `%q` for a string (`strconv.Quote`): a double-quoted Go string literal.
pub fn go_quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for r in s.chars() {
        append_escaped_rune(&mut out, r);
    }
    out.push('"');
    out
}

/// Go `encoding/json`'s `quoteChar`: single-quoted, used inside scanner errors.
pub fn go_quote_char(c: char) -> String {
    if c == '\'' {
        return "'\\''".to_string();
    }
    if c == '"' {
        return "'\"'".to_string();
    }
    let mut inner = String::new();
    append_escaped_rune(&mut inner, c);
    format!("'{}'", inner)
}

struct GoJsonScan<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> GoJsonScan<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }
    fn eof_err<T>() -> Result<T, String> {
        Err("unexpected end of JSON input".to_string())
    }

    fn value(&mut self) -> Result<(), String> {
        self.skip_ws();
        match self.peek() {
            None => Self::eof_err(),
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => self.string(),
            Some('t') => self.lit("true"),
            Some('f') => self.lit("false"),
            Some('n') => self.lit("null"),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!(
                "invalid character {} looking for beginning of value",
                go_quote_char(c)
            )),
        }
    }

    fn lit(&mut self, word: &str) -> Result<(), String> {
        for expected in word.chars() {
            match self.bump() {
                None => return Self::eof_err(),
                Some(c) if c == expected => {}
                Some(c) => {
                    return Err(format!(
                        "invalid character {} in literal {} (expecting {})",
                        go_quote_char(c),
                        word,
                        go_quote_char(expected)
                    ))
                }
            }
        }
        Ok(())
    }

    fn string(&mut self) -> Result<(), String> {
        self.bump(); // opening quote
        loop {
            match self.bump() {
                None => return Self::eof_err(),
                Some('"') => return Ok(()),
                Some('\\') => match self.bump() {
                    None => return Self::eof_err(),
                    Some('u') => {
                        for _ in 0..4 {
                            match self.bump() {
                                Some(c) if c.is_ascii_hexdigit() => {}
                                Some(c) => {
                                    return Err(format!(
                                        "invalid character {} in string literal",
                                        go_quote_char(c)
                                    ))
                                }
                                None => return Self::eof_err(),
                            }
                        }
                    }
                    Some(c) if "\"\\/bfnrt".contains(c) => {}
                    Some(c) => {
                        return Err(format!(
                            "invalid character {} in string literal",
                            go_quote_char(c)
                        ))
                    }
                },
                Some(c) if (c as u32) < 0x20 => {
                    return Err(format!(
                        "invalid character {} in string literal",
                        go_quote_char(c)
                    ))
                }
                Some(_) => {}
            }
        }
    }

    fn number(&mut self) -> Result<(), String> {
        if self.peek() == Some('-') {
            self.bump();
        }
        match self.peek() {
            Some('0') => {
                self.bump();
            }
            Some(c) if ('1'..='9').contains(&c) => {
                self.bump();
                while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    self.bump();
                }
            }
            Some(c) => {
                return Err(format!(
                    "invalid character {} in numeric literal",
                    go_quote_char(c)
                ))
            }
            None => return Self::eof_err(),
        }
        if self.peek() == Some('.') {
            self.bump();
            if !matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                return match self.peek() {
                    Some(c) => Err(format!(
                        "invalid character {} in numeric literal",
                        go_quote_char(c)
                    )),
                    None => Self::eof_err(),
                };
            }
            while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.bump();
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                return match self.peek() {
                    Some(c) => Err(format!(
                        "invalid character {} in numeric literal",
                        go_quote_char(c)
                    )),
                    None => Self::eof_err(),
                };
            }
            while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                self.bump();
            }
        }
        Ok(())
    }

    fn array(&mut self) -> Result<(), String> {
        self.bump(); // '['
        self.skip_ws();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(());
        }
        loop {
            self.value()?;
            self.skip_ws();
            match self.bump() {
                Some(',') => self.skip_ws(),
                Some(']') => return Ok(()),
                Some(c) => {
                    return Err(format!(
                        "invalid character {} after array element",
                        go_quote_char(c)
                    ))
                }
                None => return Self::eof_err(),
            }
        }
    }

    fn object(&mut self) -> Result<(), String> {
        self.bump(); // '{'
        self.skip_ws();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(());
        }
        loop {
            self.skip_ws();
            match self.peek() {
                Some('"') => self.string()?,
                Some(c) => {
                    return Err(format!(
                        "invalid character {} looking for beginning of object key string",
                        go_quote_char(c)
                    ))
                }
                None => return Self::eof_err(),
            }
            self.skip_ws();
            match self.bump() {
                Some(':') => {}
                Some(c) => {
                    return Err(format!(
                        "invalid character {} after object key",
                        go_quote_char(c)
                    ))
                }
                None => return Self::eof_err(),
            }
            self.value()?;
            self.skip_ws();
            match self.bump() {
                Some(',') => {}
                Some('}') => return Ok(()),
                Some(c) => {
                    return Err(format!(
                        "invalid character {} after object key:value pair",
                        go_quote_char(c)
                    ))
                }
                None => return Self::eof_err(),
            }
        }
    }
}

/// Validate `text` as JSON the way Go's `encoding/json` check does. Returns
/// `None` when valid, or `Some(go_error)` with the exact Go `*SyntaxError` text.
pub fn go_json_check_valid(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut s = GoJsonScan {
        chars: &chars,
        pos: 0,
    };
    match s.value() {
        Err(e) => Some(e),
        Ok(()) => {
            s.skip_ws();
            s.peek()
                .map(|c| format!("invalid character {} after top-level value", go_quote_char(c)))
        }
    }
}
