//! Faithful port of Go's `net/url` (subset): escaping (`shouldEscape` table,
//! `escape`/`unescape`/`validEncoded`), `Userinfo`, `URL` (`Parse`/`String`),
//! and `Values.Encode`. A 1:1 behavioral port — no crate reproduces Go's
//! RFC 3986 relative-URL handling (`url` crate is WHATWG and rejects them).

#![allow(clippy::result_unit_err)]

pub const ENC_PATH: u8 = 1;
pub const ENC_PATH_SEGMENT: u8 = 2;
pub const ENC_HOST: u8 = 4;
pub const ENC_ZONE: u8 = 8;
pub const ENC_USER_PASSWORD: u8 = 16;
pub const ENC_QUERY_COMPONENT: u8 = 32;
pub const ENC_FRAGMENT: u8 = 64;
pub const HEX_CHAR: u8 = 128;

pub const UPPERHEX: &[u8; 16] = b"0123456789ABCDEF";

pub fn escape_table(c: u8) -> u8 {
    match c {
        b'!' => ENC_FRAGMENT | ENC_ZONE | ENC_HOST,
        b'"' => ENC_ZONE | ENC_HOST,
        b'$' => {
            ENC_FRAGMENT | ENC_USER_PASSWORD | ENC_ZONE | ENC_HOST | ENC_PATH_SEGMENT | ENC_PATH
        }
        b'&' => {
            ENC_FRAGMENT | ENC_USER_PASSWORD | ENC_ZONE | ENC_HOST | ENC_PATH_SEGMENT | ENC_PATH
        }
        b'\'' => ENC_ZONE | ENC_HOST,
        b'(' => ENC_FRAGMENT | ENC_ZONE | ENC_HOST,
        b')' => ENC_FRAGMENT | ENC_ZONE | ENC_HOST,
        b'*' => ENC_FRAGMENT | ENC_ZONE | ENC_HOST,
        b'+' => {
            ENC_FRAGMENT | ENC_USER_PASSWORD | ENC_ZONE | ENC_HOST | ENC_PATH_SEGMENT | ENC_PATH
        }
        b',' => ENC_FRAGMENT | ENC_USER_PASSWORD | ENC_ZONE | ENC_HOST | ENC_PATH,
        b'-' => {
            ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        b'.' => {
            ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        b'/' => ENC_FRAGMENT | ENC_PATH,
        b'0'..=b'9' => {
            HEX_CHAR
                | ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        b':' => ENC_FRAGMENT | ENC_ZONE | ENC_HOST | ENC_PATH_SEGMENT | ENC_PATH,
        b';' => ENC_FRAGMENT | ENC_USER_PASSWORD | ENC_ZONE | ENC_HOST | ENC_PATH,
        b'<' => ENC_ZONE | ENC_HOST,
        b'=' => {
            ENC_FRAGMENT | ENC_USER_PASSWORD | ENC_ZONE | ENC_HOST | ENC_PATH_SEGMENT | ENC_PATH
        }
        b'>' => ENC_ZONE | ENC_HOST,
        b'?' => ENC_FRAGMENT,
        b'@' => ENC_FRAGMENT | ENC_PATH_SEGMENT | ENC_PATH,
        b'A'..=b'F' | b'a'..=b'f' => {
            HEX_CHAR
                | ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        b'G'..=b'Z' | b'g'..=b'z' => {
            ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        b'[' => ENC_ZONE | ENC_HOST,
        b']' | b'\\' => ENC_ZONE | ENC_HOST,
        b'^' => ENC_ZONE | ENC_HOST,
        b'_' => {
            ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        b'`' => ENC_ZONE | ENC_HOST,
        b'{' | b'|' | b'}' => ENC_ZONE | ENC_HOST,
        b'~' => {
            ENC_FRAGMENT
                | ENC_QUERY_COMPONENT
                | ENC_USER_PASSWORD
                | ENC_ZONE
                | ENC_HOST
                | ENC_PATH_SEGMENT
                | ENC_PATH
        }
        _ => 0,
    }
}

pub fn should_escape(c: u8, mode: u8) -> bool {
    escape_table(c) & mode == 0
}

pub fn is_hex(c: u8) -> bool {
    escape_table(c) & HEX_CHAR != 0
}

pub fn unhex(c: u8) -> u8 {
    9u8.wrapping_mul(c >> 6).wrapping_add(c & 15)
}

/// Port of Go's `escape(s, mode)`.
pub fn escape(s: &str, mode: u8) -> String {
    let bytes = s.as_bytes();
    let mut space_count = 0usize;
    let mut hex_count = 0usize;
    for &c in bytes {
        if should_escape(c, mode) {
            if c == b' ' && mode == ENC_QUERY_COMPONENT {
                space_count += 1;
            } else {
                hex_count += 1;
            }
        }
    }
    if space_count == 0 && hex_count == 0 {
        return s.to_string();
    }
    let mut t: Vec<u8> = Vec::with_capacity(bytes.len() + 2 * hex_count);
    if hex_count == 0 {
        for &c in bytes {
            if c == b' ' {
                t.push(b'+');
            } else {
                t.push(c);
            }
        }
        return String::from_utf8_lossy(&t).into_owned();
    }
    for &c in bytes {
        if c == b' ' && mode == ENC_QUERY_COMPONENT {
            t.push(b'+');
        } else if should_escape(c, mode) {
            t.push(b'%');
            t.push(UPPERHEX[(c >> 4) as usize]);
            t.push(UPPERHEX[(c & 15) as usize]);
        } else {
            t.push(c);
        }
    }
    String::from_utf8_lossy(&t).into_owned()
}

/// Port of Go's `unescape(s, mode)`. Err(()) on a malformed %-escape.
pub fn unescape(s: &str, mode: u8) -> Result<String, ()> {
    let b = s.as_bytes();
    let mut n = 0usize;
    let mut has_plus = false;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'%' => {
                n += 1;
                if i + 2 >= b.len() || !is_hex(b[i + 1]) || !is_hex(b[i + 2]) {
                    return Err(());
                }
                if mode == ENC_HOST && unhex(b[i + 1]) < 8 && &b[i..i + 3] != b"%25" {
                    return Err(());
                }
                if mode == ENC_ZONE {
                    let v = (unhex(b[i + 1]) << 4) | unhex(b[i + 2]);
                    if &b[i..i + 3] != b"%25" && v != b' ' && should_escape(v, ENC_HOST) {
                        return Err(());
                    }
                }
                i += 3;
            }
            b'+' => {
                has_plus = mode == ENC_QUERY_COMPONENT;
                i += 1;
            }
            _ => {
                if (mode == ENC_HOST || mode == ENC_ZONE)
                    && b[i] < 0x80
                    && should_escape(b[i], mode)
                {
                    return Err(());
                }
                i += 1;
            }
        }
    }
    if n == 0 && !has_plus {
        return Ok(s.to_string());
    }
    let unescaped_plus: u8 = if mode == ENC_QUERY_COMPONENT {
        b' '
    } else {
        b'+'
    };
    let mut t: Vec<u8> = Vec::with_capacity(b.len().saturating_sub(2 * n));
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'%' => {
                t.push((unhex(b[i + 1]) << 4) | unhex(b[i + 2]));
                i += 2;
            }
            b'+' => t.push(unescaped_plus),
            c => t.push(c),
        }
        i += 1;
    }
    Ok(String::from_utf8_lossy(&t).into_owned())
}

/// Port of Go's `validEncoded(s, mode)`.
pub fn valid_encoded(s: &str, mode: u8) -> bool {
    for &c in s.as_bytes() {
        match c {
            b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':'
            | b'@' => {}
            b'[' | b']' => {}
            b'%' => {}
            _ => {
                if should_escape(c, mode) {
                    return false;
                }
            }
        }
    }
    true
}

// ===========================================================================
// net/url: Userinfo, URL, parse, String
// ===========================================================================

#[derive(Clone, Default)]
pub struct Userinfo {
    pub username: String,
    pub password: String,
    pub password_set: bool,
}

impl Userinfo {
    pub fn user(username: &str) -> Self {
        Userinfo {
            username: username.to_string(),
            password: String::new(),
            password_set: false,
        }
    }
    pub fn user_password(username: &str, password: &str) -> Self {
        Userinfo {
            username: username.to_string(),
            password: password.to_string(),
            password_set: true,
        }
    }
    pub fn escaped(&self) -> String {
        let mut s = escape(&self.username, ENC_USER_PASSWORD);
        if self.password_set {
            s.push(':');
            s.push_str(&escape(&self.password, ENC_USER_PASSWORD));
        }
        s
    }
    pub fn has_password(&self) -> bool {
        self.password_set
    }
}

#[derive(Clone, Default)]
pub struct Url {
    pub scheme: String,
    pub opaque: String,
    pub user: Option<Userinfo>,
    pub host: String,
    pub path: String,
    pub raw_path: String,
    pub force_query: bool,
    pub raw_query: String,
    pub fragment: String,
    pub raw_fragment: String,
    pub omit_host: bool,
}

impl Url {
    /// Go `(*URL).Hostname()`: `splitHostPort(u.Host)` then take the host.
    pub fn hostname(&self) -> String {
        split_host_port(&self.host).0
    }

    pub fn set_path(&mut self, p: &str) -> Result<(), ()> {
        let path = unescape(p, ENC_PATH)?;
        self.path = path.clone();
        let escp = escape(&path, ENC_PATH);
        if p == escp {
            self.raw_path = String::new();
        } else {
            self.raw_path = p.to_string();
        }
        Ok(())
    }

    pub fn set_fragment(&mut self, f: &str) -> Result<(), ()> {
        let frag = unescape(f, ENC_FRAGMENT)?;
        self.fragment = frag.clone();
        let escf = escape(&frag, ENC_FRAGMENT);
        if f == escf {
            self.raw_fragment = String::new();
        } else {
            self.raw_fragment = f.to_string();
        }
        Ok(())
    }

    pub fn escaped_path(&self) -> String {
        if !self.raw_path.is_empty()
            && valid_encoded(&self.raw_path, ENC_PATH)
            && let Ok(p) = unescape(&self.raw_path, ENC_PATH)
            && p == self.path
        {
            return self.raw_path.clone();
        }
        if self.path == "*" {
            return "*".to_string();
        }
        escape(&self.path, ENC_PATH)
    }

    pub fn escaped_fragment(&self) -> String {
        if !self.raw_fragment.is_empty()
            && valid_encoded(&self.raw_fragment, ENC_FRAGMENT)
            && let Ok(f) = unescape(&self.raw_fragment, ENC_FRAGMENT)
            && f == self.fragment
        {
            return self.raw_fragment.clone();
        }
        escape(&self.fragment, ENC_FRAGMENT)
    }

    /// Port of Go's `(*URL).String()`.
    pub fn to_url_string(&self) -> String {
        let mut buf = String::new();
        if !self.scheme.is_empty() {
            buf.push_str(&self.scheme);
            buf.push(':');
        }
        if !self.opaque.is_empty() {
            buf.push_str(&self.opaque);
        } else {
            if !self.scheme.is_empty() || !self.host.is_empty() || self.user.is_some() {
                if self.omit_host && self.host.is_empty() && self.user.is_none() {
                    // omit empty host
                } else {
                    if !self.host.is_empty() || !self.path.is_empty() || self.user.is_some() {
                        buf.push_str("//");
                    }
                    if let Some(ui) = &self.user {
                        buf.push_str(&ui.escaped());
                        buf.push('@');
                    }
                    if !self.host.is_empty() {
                        buf.push_str(&escape(&self.host, ENC_HOST));
                    }
                }
            }
            let mut path = self.escaped_path();
            if self.omit_host
                && self.host.is_empty()
                && self.user.is_none()
                && path.starts_with("//")
            {
                // Escape the first / so re-parsing does not read an authority.
                buf.push_str("%2F");
                path = path[1..].to_string();
            }
            if !path.is_empty() && !path.starts_with('/') && !self.host.is_empty() {
                buf.push('/');
            }
            if buf.is_empty() {
                let segment = path.split('/').next().unwrap_or("");
                if segment.contains(':') {
                    buf.push_str("./");
                }
            }
            buf.push_str(&path);
        }
        if self.force_query || !self.raw_query.is_empty() {
            buf.push('?');
            buf.push_str(&self.raw_query);
        }
        if !self.fragment.is_empty() {
            buf.push('#');
            buf.push_str(&self.escaped_fragment());
        }
        buf
    }
}

pub fn valid_optional_port(port: &str) -> bool {
    if port.is_empty() {
        return true;
    }
    let b = port.as_bytes();
    if b[0] != b':' {
        return false;
    }
    b[1..].iter().all(|&c| c.is_ascii_digit())
}

/// Port of Go's `net/url.splitHostPort`: splits `host:port`, then strips the
/// surrounding brackets of an IPv6 literal. Returns `(host, port)`.
pub fn split_host_port(host_port: &str) -> (String, String) {
    let mut host = host_port.to_string();
    let mut port = String::new();
    if let Some(colon) = host.rfind(':') {
        if valid_optional_port(&host[colon..]) {
            port = host[colon + 1..].to_string();
            host = host[..colon].to_string();
        }
    }
    if host.starts_with('[') && host.ends_with(']') && host.len() >= 2 {
        host = host[1..host.len() - 1].to_string();
    }
    (host, port)
}

/// Port of Go's `parseHost`. Returns Err(()) on any parse error.
pub fn parse_host(scheme: &str, host: &str) -> Result<String, ()> {
    let hb = host.as_bytes();
    // Find last '['
    if let Some(open) = host.rfind('[') {
        if open > 0 {
            return Err(());
        }
        // IP-literal form: "[...]"
        let close = host.rfind(']').ok_or(())?;
        let colon_port = &host[close + 1..];
        if !valid_optional_port(colon_port) {
            return Err(());
        }
        unescape(colon_port, ENC_HOST)?;
        let hostname = &host[open + 1..close];
        // zone identifier
        let unescaped_hostname = if let Some(zone_idx) = hostname.find("%25") {
            let host_part = unescape(&hostname[..zone_idx], ENC_HOST)?;
            let zone_part = unescape(&hostname[zone_idx..], ENC_ZONE)?;
            format!("{host_part}{zone_part}")
        } else {
            unescape(hostname, ENC_HOST)?
        };
        // netip.ParseAddr: require a valid IPv6 (or IPv4-mapped) address.
        if unescaped_hostname.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(());
        }
        return Ok(format!("[{unescaped_hostname}]{colon_port}"));
    }
    // Non-IPv6: validate optional port if a colon exists.
    if let Some(i) = host.find(':') {
        let last_colon = host.rfind(':').unwrap();
        // Go 1.27: for non-http(s) schemes, i = lastColon.
        let _ = scheme;
        let idx = last_colon;
        let _ = i;
        let colon_port = &host[idx..];
        if !valid_optional_port(colon_port) {
            return Err(());
        }
    }
    let _ = hb;
    let host = unescape(host, ENC_HOST)?;
    Ok(host)
}

pub fn valid_userinfo(s: &str) -> bool {
    for r in s.chars() {
        if r.is_ascii_alphabetic() || r.is_ascii_digit() {
            continue;
        }
        match r {
            '-' | '.' | '_' | ':' | '~' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ','
            | ';' | '=' | '%' | '@' => {}
            _ => return false,
        }
    }
    true
}

/// Port of Go's `parseAuthority`.
pub fn parse_authority(scheme: &str, authority: &str) -> Result<(Option<Userinfo>, String), ()> {
    let at = authority.rfind('@');
    let (host, user) = match at {
        None => (parse_host(scheme, authority)?, None),
        Some(i) => {
            let h = parse_host(scheme, &authority[i + 1..])?;
            let userinfo = &authority[..i];
            if !valid_userinfo(userinfo) {
                return Err(());
            }
            let u = if !userinfo.contains(':') {
                Userinfo::user(&unescape(userinfo, ENC_USER_PASSWORD)?)
            } else {
                let (username, password) = userinfo.split_once(':').unwrap();
                Userinfo::user_password(
                    &unescape(username, ENC_USER_PASSWORD)?,
                    &unescape(password, ENC_USER_PASSWORD)?,
                )
            };
            (h, Some(u))
        }
    };
    Ok((user, host))
}

/// Port of Go's `getScheme`.
pub fn get_scheme(raw: &str) -> Result<(String, String), String> {
    let b = raw.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() {
            // ok
        } else if c.is_ascii_digit() || c == b'+' || c == b'-' || c == b'.' {
            if i == 0 {
                return Ok((String::new(), raw.to_string()));
            }
        } else if c == b':' {
            if i == 0 {
                return Err("missing protocol scheme".to_string());
            }
            return Ok((raw[..i].to_string(), raw[i + 1..].to_string()));
        } else {
            return Ok((String::new(), raw.to_string()));
        }
        i += 1;
    }
    Ok((String::new(), raw.to_string()))
}

pub fn string_contains_ctl_byte(s: &str) -> bool {
    s.bytes().any(|b| b < b' ' || b == 0x7f)
}

/// Port of Go's internal `parse(rawURL, viaRequest=false)`.
pub fn parse_url(raw_url: &str) -> Result<Url, String> {
    let mut rest: String;
    if string_contains_ctl_byte(raw_url) {
        return Err("net/url: invalid control character in URL".to_string());
    }
    let mut url = Url::default();
    if raw_url == "*" {
        url.path = "*".to_string();
        return Ok(url);
    }
    let (scheme, r) = get_scheme(raw_url)?;
    url.scheme = scheme.to_lowercase();
    rest = r;

    if rest.ends_with('?') && rest.matches('?').count() == 1 {
        url.force_query = true;
        rest = rest[..rest.len() - 1].to_string();
    } else if let Some(idx) = rest.find('?') {
        url.raw_query = rest[idx + 1..].to_string();
        rest = rest[..idx].to_string();
    }

    if !rest.starts_with('/') {
        if !url.scheme.is_empty() {
            // Rootless path → opaque.
            url.opaque = rest;
            return Ok(url);
        }
        // Relative URL: first path segment may not contain a colon.
        let segment = rest.split('/').next().unwrap_or("");
        if segment.contains(':') {
            return Err("first path segment in URL cannot contain colon".to_string());
        }
    }

    if (!url.scheme.is_empty() || !rest.starts_with("///")) && rest.starts_with("//") {
        let after = &rest[2..];
        let (authority, r2) = match after.find('/') {
            Some(i) => (after[..i].to_string(), after[i..].to_string()),
            None => (after.to_string(), String::new()),
        };
        rest = r2;
        let (user, host) = parse_authority(&url.scheme, &authority)
            .map_err(|_| "invalid URL escape".to_string())?;
        url.user = user;
        url.host = host;
    } else if !url.scheme.is_empty() && rest.starts_with('/') {
        url.omit_host = true;
    }

    url.set_path(&rest)
        .map_err(|_| "invalid URL path escape".to_string())?;
    Ok(url)
}

/// Port of Go's `url.Parse`.
pub fn url_parse(raw_url: &str) -> Result<Url, String> {
    let (u, frag) = match raw_url.split_once('#') {
        Some((a, b)) => (a, Some(b)),
        None => (raw_url, None),
    };
    let mut url = parse_url(u)?;
    if let Some(frag) = frag
        && !frag.is_empty()
    {
        url.set_fragment(frag)
            .map_err(|_| "invalid URL escape".to_string())?;
    }
    Ok(url)
}

// ===========================================================================
// net/url: Values.Encode (sslmode)
// ===========================================================================

pub fn query_escape(s: &str) -> String {
    escape(s, ENC_QUERY_COMPONENT)
}

pub fn values_encode(pairs: &[(&str, &str)]) -> String {
    // Go sorts keys; single "sslmode" key here.
    let mut keys: Vec<&str> = pairs.iter().map(|(k, _)| *k).collect();
    keys.sort_unstable();
    let mut buf = String::new();
    for k in keys {
        let key_escaped = query_escape(k);
        for (pk, pv) in pairs {
            if *pk == k {
                if !buf.is_empty() {
                    buf.push('&');
                }
                buf.push_str(&key_escaped);
                buf.push('=');
                buf.push_str(&query_escape(pv));
            }
        }
    }
    buf
}

/// Go `net.JoinHostPort`: `host:port`, or `[host]:port` when `host` contains a
/// colon (an IPv6 literal).
pub fn join_host_port(host: &str, port: &str) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
