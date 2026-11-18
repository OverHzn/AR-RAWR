//! Rust port of the unexported, side-effect-free helpers in `server/`.
//!
//! These functions are *not* reachable through `rust-anything`'s bridge (it only
//! bridges exported, pointer-free functions), so parity is proven differently: a
//! temporary Go oracle (`server/zz_oracle_test.go`, deleted after recording) ran
//! the real Go functions over a corpus and wrote `cases.jsonl`. The
//! `tests/parity.rs` harness replays that corpus here and asserts byte-identical
//! output. The Go original is untouched.

#![forbid(unsafe_code)]

use gocompat::gojson::go_quote_string;
use gocompat::strings::{go_to_lower, go_trim_space};

// ---- supporting constants copied from the Go source ----
const MIN_PASSWORD_RUNES: usize = 12; // server/auth.go minPasswordRunes
const MAX_PASSWORD_BYTES: usize = 72; // server/auth.go maxPasswordBytes
const VERIFICATION_NOTE_QUOTE_LEN: usize = 60; // server/finding_verification.go
const MAX_INGEST_BODY_SIZE: usize = 256 << 10; // server/traffic_ingest.go
const SKILL_PATH_BLOCKED: &str = "\\%#?*:|\"<>"; // server/server_mgmt.go skillPathBlocked
const MASKED_PREFIX: &str = "__masked__"; // notify/mask.go MaskedPrefix

/// `strconv.FormatInt(v, 10)` for the int64 helpers.
fn format_i64(v: i64) -> String {
    v.to_string()
}

// ---- auth.go ----

/// `validatePassword` (server/auth.go).
pub fn validate_password(pw: &str) -> String {
    if pw.chars().count() < MIN_PASSWORD_RUNES {
        return format!(
            "Password must be at least {} characters",
            MIN_PASSWORD_RUNES
        );
    }
    if pw.len() > MAX_PASSWORD_BYTES {
        return format!("Password must not exceed {} bytes", MAX_PASSWORD_BYTES);
    }
    String::new()
}

// ---- customtool.go ----

/// `proxyURLString` (server/customtool.go).
pub fn proxy_url_string(s: &str) -> String {
    let s = go_trim_space(s);
    if s.is_empty() || s.contains("://") {
        return s.to_string();
    }
    format!("http://{}", s)
}

/// `identity` (server/customtool.go).
pub fn identity(s: &str) -> String {
    s.to_string()
}

/// `shellQuote` (server/customtool.go).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---- engine.go ----

/// `preview` (server/engine.go).
pub fn preview(s: &str, n: i64) -> String {
    let s = s.replace(['\r', '\n'], " ");
    let r: Vec<char> = s.chars().collect();
    if n >= 0 && (r.len() as i64) > n {
        let k = n as usize;
        let mut out: String = r[..k].iter().collect();
        out.push('…');
        return out;
    }
    r.into_iter().collect()
}

// ---- finding_verification.go ----

/// `normalizeEvidenceText` (server/finding_verification.go).
pub fn normalize_evidence_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `shortQuote` (server/finding_verification.go).
pub fn short_quote(q: &str) -> String {
    let r: Vec<char> = go_trim_space(q).chars().collect();
    let take = if r.len() > VERIFICATION_NOTE_QUOTE_LEN {
        VERIFICATION_NOTE_QUOTE_LEN
    } else {
        r.len()
    };
    r[..take].iter().collect()
}

// ---- intent_intervention.go ----

/// `validWorkerMessageRequestID` (server/intent_intervention.go).
pub fn valid_worker_message_request_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    for r in id.chars() {
        let ok = r.is_ascii_lowercase()
            || r.is_ascii_uppercase()
            || r.is_ascii_digit()
            || r == '-'
            || r == '_'
            || r == '.'
            || r == ':';
        if !ok {
            return false;
        }
    }
    true
}

// ---- logsink.go ----

/// `levelOf` (server/logsink.go).
pub fn level_of(msg: &str) -> String {
    let low = go_to_lower(msg);
    const ERR: &[&str] = &[
        "fatal",
        "panic",
        "error",
        "err:",
        "failed",
        "dropped",
        "refused",
        "✕",
        "unreachable",
    ];
    for k in ERR {
        if low.contains(k) {
            return "error".to_string();
        }
    }
    const WARN: &[&str] = &[
        "warn", "disabled", "disable", "skip", "stopped", "⚠", "retry",
    ];
    for k in WARN {
        if low.contains(k) {
            return "warn".to_string();
        }
    }
    "info".to_string()
}

// ---- notifier.go ----

/// `trimTrailingSlash` (server/notifier.go).
pub fn trim_trailing_slash(s: &str) -> String {
    let mut end = s.len();
    let b = s.as_bytes();
    while end > 0 && b[end - 1] == b'/' {
        end -= 1;
    }
    s[..end].to_string()
}

// ---- platform_tools.go ----

/// `enabledWord` (server/platform_tools.go).
pub fn enabled_word(b: bool) -> String {
    if b {
        "enabled".to_string()
    } else {
        "disabled".to_string()
    }
}

// ---- scheduler.go ----

/// `trunc` (server/scheduler.go).
pub fn trunc(s: &str, max: i64) -> String {
    let r: Vec<char> = s.chars().collect();
    if max >= 0 && (r.len() as i64) <= max {
        return s.to_string();
    }
    let k = if max < 0 { 0 } else { max as usize };
    let mut out: String = r[..k.min(r.len())].iter().collect();
    out.push_str(&format!("…(truncated, {} chars in total)", r.len()));
    out
}

// ---- server.go ----

/// `truncateReply` (server/server.go).
pub fn truncate_reply(s: &str) -> String {
    const MAX: usize = 200;
    let r: Vec<char> = s.chars().collect();
    if r.len() <= MAX {
        return s.to_string();
    }
    let mut out: String = r[..MAX].iter().collect();
    out.push('…');
    out
}

/// `portOr` (server/server.go).
pub fn port_or(s: &str, d: i64) -> i64 {
    match go_atoi(s) {
        Some(n) if n > 0 => n,
        _ => d,
    }
}

/// `defaultPort` (server/server.go).
pub fn default_port(scheme: &str) -> i64 {
    if scheme == "http" { 80 } else { 443 }
}

/// `schemeForPort` (server/server.go).
pub fn scheme_for_port(p: i64) -> String {
    if p == 443 || p == 8443 {
        "https".to_string()
    } else {
        "http".to_string()
    }
}

/// `normFilter` (server/server.go).
pub fn norm_filter(v: &str) -> String {
    if v == "all" {
        String::new()
    } else {
        v.to_string()
    }
}

/// `inheritedIntentResult` (server/server.go).
pub fn inherited_intent_result(state: &str) -> bool {
    matches!(state, "done" | "blocked" | "exhausted" | "stopped")
}

/// `atoiDefault` (server/server.go).
pub fn atoi_default(s: &str, d: i64) -> i64 {
    go_atoi(s).unwrap_or(d)
}

/// Go `strconv.Atoi`: parse an optionally-signed base-10 integer, whole string,
/// no surrounding whitespace tolerated (returns `None` on any trailing/leading
/// space, empty string, or non-digit).
fn go_atoi(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

// ---- server_mgmt.go ----

/// `validSkillName` (server/server_mgmt.go).
pub fn valid_skill_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // utf8.ValidString is always true for a Rust &str.
    let rs: Vec<char> = name.chars().collect();
    if rs.len() > 64 {
        return false;
    }
    let is_letter =
        |r: char| r.is_ascii_lowercase() || (r as u32 > unicode_max_ascii() && r.is_alphabetic());
    if !is_letter(rs[0]) || rs[rs.len() - 1] == '-' {
        return false;
    }
    for &r in &rs {
        let ok = is_letter(r)
            || r.is_ascii_digit()
            || r == '-'
            || (r as u32 > unicode_max_ascii() && r.is_numeric());
        if !ok {
            return false;
        }
    }
    !name.contains("--")
}

/// `unicode.MaxASCII` = 0x7F; chars strictly greater are "non-ASCII".
fn unicode_max_ascii() -> u32 {
    0x7F
}

/// `maskedSecret` (server/server_mgmt.go): notify.MaskedPrefix when non-empty.
pub fn masked_secret(v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        MASKED_PREFIX.to_string()
    }
}

/// `isMaskedSecret` (server/server_mgmt.go): notify.IsMasked(TrimSpace(v)).
pub fn is_masked_secret(v: &str) -> bool {
    go_trim_space(v).starts_with(MASKED_PREFIX)
}

/// `yamlScalar` (server/server_mgmt.go).
pub fn yaml_scalar(s: &str) -> String {
    const SPECIAL: &str = ":#{}[]&*!|>'\"%@`,\n";
    if !s.is_empty()
        && !s.chars().any(|c| SPECIAL.contains(c))
        && !s.starts_with(' ')
        && !s.ends_with(' ')
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "''"))
}

/// `skillPathRune` (server/server_mgmt.go).
pub fn skill_path_rune(r: char) -> bool {
    let u = r as u32;
    if u < 0x20 || u == 0x7f || u == 0xfffd {
        return false; // control / NUL / invalid-UTF8 replacement char
    }
    if SKILL_PATH_BLOCKED.contains(r) {
        return false;
    }
    // unicode.Is(unicode.Cf/Co/Cs, r): format chars (ZWJ, bidi overrides / RLO
    // spoofing) and private-use. Cs (surrogates) cannot be a Rust `char`.
    if is_unicode_cf(u) || is_unicode_co(u) {
        return false;
    }
    if r != ' ' && r.is_whitespace() {
        return false;
    }
    true
}

/// `unicode.Cf` (format) ranges, generated from Go's `unicode` package.
fn is_unicode_cf(u: u32) -> bool {
    matches!(u,
        0x00AD | 0x0600 | 0x061C | 0x06DD | 0x070F | 0x0890 | 0x0891 | 0x08E2
        | 0x180E | 0x200B | 0xFEFF | 0xFFF9 | 0x110BD | 0x110CD | 0xE0001 | 0xE0020
        | 0x0601..=0x0605 | 0x200C..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064
        | 0x2066..=0x206F | 0xFFFA..=0xFFFB | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0021..=0xE007F)
}

/// `unicode.Co` (private use) ranges, generated from Go's `unicode` package.
fn is_unicode_co(u: u32) -> bool {
    matches!(u, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

// ---- skill_zip.go ----

/// `zipMethodName` (server/skill_zip.go).
pub fn zip_method_name(m: u16) -> String {
    let name = match m {
        0 => "Store",
        8 => "Deflate",
        9 => "Deflate64",
        12 => "bzip2",
        14 => "LZMA",
        20 => "Zstandard", // zipMethodZstdPKW
        93 => "Zstandard", // zipMethodZstd
        95 => "XZ",
        96 => "JPEG",
        97 => "WavPack",
        98 => "PPMd",
        99 => "AES encrypted",
        _ => return "unknown".to_string(),
    };
    name.to_string()
}

// ---- task_llm.go ----

/// `llmFailKey` (server/task_llm.go).
pub fn llm_fail_key(task_id: &str, profile_id: i64) -> String {
    format!("{}:{}", task_id, format_i64(profile_id))
}

/// `parseTaskID` (server/task_llm.go): `strconv.ParseInt(id, 10, 64)`, then
/// require a positive id; otherwise `invalid task id "<id>"` (Go `%q` quoting).
pub fn parse_task_id(id: &str) -> Result<i64, String> {
    match id.parse::<i64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("invalid task id {}", go_quote_string(id))),
    }
}

// ---- traffic_ingest.go ----

/// `truncateBody` (server/traffic_ingest.go).
pub fn truncate_body(s: &str) -> String {
    if s.len() <= MAX_INGEST_BODY_SIZE {
        return s.to_string();
    }
    // Go slices bytes (s[:max]); preserve that on a char boundary if possible.
    let mut end = MAX_INGEST_BODY_SIZE;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... [truncated by ingest]", &s[..end])
}

// ---- chatupload.go ----

/// `sanitizeFilename` (server/chatupload.go): strips `"`, `\`, `\n`, `\r`.
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '"' | '\\' | '\n' | '\r'))
        .collect()
}

/// `humanBytes` (server/chatupload.go).
pub fn human_bytes(n: i64) -> String {
    const K: i64 = 1 << 10;
    const M: i64 = 1 << 20;
    if n >= M {
        format!("{:.1} MB", n as f64 / M as f64)
    } else if n >= K {
        format!("{:.1} KB", n as f64 / K as f64)
    } else {
        format!("{} B", n)
    }
}

// ---- dto.go ----

/// `i64s` (server/dto.go): strconv.FormatInt(v, 10).
pub fn i64s(v: i64) -> String {
    format_i64(v)
}

// ---- conversations.go ----

/// `firstLine` (server/conversations.go).
pub fn first_line(s: &str, max: i64) -> String {
    let s = go_trim_space(s);
    let s = match s.find(['\r', '\n']) {
        Some(i) => &s[..i],
        None => s,
    };
    let r: Vec<char> = s.chars().collect();
    if max >= 0 && (r.len() as i64) > max {
        let k = max as usize;
        let mut out: String = r[..k].iter().collect();
        out.push('…');
        return out;
    }
    r.into_iter().collect()
}

// ---- constraints_api.go ----

/// `normalizeConstraintKind` (server/constraints_api.go).
pub fn normalize_constraint_kind(k: &str) -> String {
    let k = go_to_lower(go_trim_space(k));
    if k == "allow" || k == "deny" {
        k
    } else {
        String::new()
    }
}

// ---- server_mgmt.go canonicalTaskID ----

/// `canonicalTaskID` (server/server_mgmt.go): returns (canonical, ok).
pub fn canonical_task_id(raw: &str) -> (String, bool) {
    match go_parse_int(raw) {
        Some(n) if n > 0 => (format_i64(n), true),
        _ => (String::new(), false),
    }
}

/// Go `strconv.ParseInt(raw, 10, 64)`: whole-string, optional sign.
fn go_parse_int(raw: &str) -> Option<i64> {
    raw.parse::<i64>().ok()
}
