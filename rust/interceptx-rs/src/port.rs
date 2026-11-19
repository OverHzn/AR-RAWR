//! Rust port of the unexported, pure helpers in the Go `intercept/` package.
//!
//! Everything here is deterministic and value-typed: the argv-prefix arity table
//! lookup, program-name normalisation, the code-fence stripper, the byte/word
//! predicates, the UTF-8-boundary truncation, the port/risk/action/message/label
//! helpers and the resolved-subset-of-pinned check. Go string semantics
//! (`path.Base`, `strings.ToLower`, `strings.TrimSpace`) come from `gocompat`.

use gocompat::filepath::path_base;
use gocompat::strings::{go_to_lower, go_trim_space};

// ---------------------------------------------------------------------------
// Batch 9ae: pure helpers in `intercept/`.
// ---------------------------------------------------------------------------

/// `argvArity` (`intercept/argv.go`): the command-prefix -> arity table, as a
/// `match`. Keys are joined with single spaces (see [`argv_prefix_len`]).
fn argv_arity(key: &str) -> Option<i64> {
    match key {
        "cat" => Some(1),
        "cd" => Some(1),
        "ls" => Some(1),
        "rm" => Some(1),
        "grep" => Some(1),
        "head" => Some(1),
        "tail" => Some(1),
        "echo" => Some(1),
        "pwd" => Some(1),
        "find" => Some(1),
        "curl" => Some(1),
        "wget" => Some(1),
        "nmap" => Some(1),
        "git" => Some(2),
        "npm" => Some(2),
        "docker" => Some(2),
        "kubectl" => Some(2),
        "terraform" => Some(2),
        "npm run" => Some(3),
        "docker compose" => Some(3),
        "kubectl rollout" => Some(3),
        "git config" => Some(3),
        "gcloud" => Some(3),
        "gh" => Some(3),
        "aws" => Some(3),
        _ => None,
    }
}

/// `argvPrefixLen` (`intercept/argv.go`): the longest token prefix that is a
/// known command in [`argv_arity`]; its arity, clamped to `len(tokens)`, or
/// `min(1, len)` when nothing matches.
pub fn argv_prefix_len(tokens: &[String]) -> i64 {
    for n in (1..=tokens.len()).rev() {
        let key = tokens[..n].join(" ");
        if let Some(a) = argv_arity(&key) {
            return a.min(tokens.len() as i64);
        }
    }
    1.min(tokens.len() as i64)
}

/// `argvProgramName` (`intercept/argv.go`): the lower-cased `path.Base` of `tok`,
/// with `mkfs.*` collapsed to `mkfs` and a trailing run of digits/dots (after a
/// letter) stripped.
pub fn argv_program_name(tok: &str) -> String {
    let name = go_to_lower(&path_base(tok));
    if name.starts_with("mkfs.") {
        return "mkfs".to_string();
    }
    let bytes = name.as_bytes();
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b'.' || bytes[end - 1].is_ascii_digit()) {
        end -= 1;
    }
    if end > 0 && end < bytes.len() {
        return name[..end].to_string();
    }
    name
}

/// `isWord` (`intercept/...`): `[A-Za-z0-9_]`.
pub fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `endsToken` (`intercept/...`): whether position `i` in `b` ends a word token
/// (end of input, a non-word byte, or a `.` not followed by a word byte).
pub fn ends_token(b: &[u8], i: usize) -> bool {
    if i >= b.len() {
        return true;
    }
    if is_word(b[i]) {
        return false;
    }
    b[i] != b'.' || i + 1 >= b.len() || !is_word(b[i + 1])
}

/// `bounded` (`intercept/...`): `s` truncated to at most `limit` bytes on a UTF-8
/// boundary, with a flag reporting whether truncation happened.
pub fn bounded(s: &str, limit: i64) -> (String, bool) {
    if (s.len() as i64) <= limit {
        return (s.to_string(), false);
    }
    let bytes = s.as_bytes();
    let mut end = limit;
    while end > 0 && (bytes[end as usize] & 0xC0) == 0x80 {
        end -= 1;
    }
    (s[..end as usize].to_string(), true)
}

/// `stripCodeFence` (`intercept/...`): strip a surrounding triple-backtick fence
/// (and an opening language-tag line) from `text`.
pub fn strip_code_fence(text: &str) -> String {
    let t = go_trim_space(text);
    if t.len() <= 6 || !t.starts_with("```") || !t.ends_with("```") {
        return t.to_string();
    }
    let inner = go_trim_space(&t[3..t.len() - 3]);
    if !inner.starts_with('{')
        && let Some((_, rest)) = inner.split_once('\n')
    {
        return go_trim_space(rest).to_string();
    }
    inner.to_string()
}

/// `validPort` (`intercept/...`): `s` as a TCP/UDP port (1-65535), or 0.
pub fn valid_port(s: &str) -> i64 {
    match s.parse::<i64>() {
        Ok(n) if (1..=65535).contains(&n) => n,
        _ => 0,
    }
}

/// `riskLevel` (`intercept/risk.go`): `high` at >= `riskHighAt` (70), `medium` at
/// >= `riskMediumAt` (30), else `low`.
pub fn risk_level(score: i64) -> String {
    if score >= 70 {
        "high"
    } else if score >= 30 {
        "medium"
    } else {
        "low"
    }
    .to_string()
}

/// `judgeActionLabel` (`intercept/...`): the human label for a judge action.
pub fn judge_action_label(action: &str) -> String {
    match action {
        "allow" => "allow",
        "deny" => "block",
        "ask" => "escalated to manual approval",
        _ => action,
    }
    .to_string()
}

/// `defaultMessage` (`intercept/...`): the default operator message for a `deny`
/// or `ask` action, or "".
pub fn default_message(action: &str, name: &str) -> String {
    match action {
        "deny" => format!("intercept rule [{name}] forbids executing this tool"),
        "ask" => format!("intercept rule [{name}] requires user approval, please wait"),
        _ => String::new(),
    }
}

/// `subset` (`intercept/...`): whether `resolved` is non-empty and every element
/// is in `pinned`.
pub fn subset(resolved: &[String], pinned: &[String]) -> bool {
    if resolved.is_empty() {
        return false;
    }
    resolved.iter().all(|ip| pinned.contains(ip))
}

// ---------------------------------------------------------------------------
// Batch 9ai: hostname label validation in `intercept/` (pure byte-wise check).
// ---------------------------------------------------------------------------

/// `validHostname` (`intercept/...`): whether `h` is a syntactically valid
/// hostname — at most 253 bytes, each dot-separated label 1-63 bytes, no leading
/// or trailing `-` per label, and only `[a-z0-9_-]` (lower-case only) per byte.
pub fn valid_hostname(h: &str) -> bool {
    if h.len() > 253 {
        return false;
    }
    for label in h.split('.') {
        let b = label.as_bytes();
        if b.is_empty() || b.len() > 63 || b[0] == b'-' || b[b.len() - 1] == b'-' {
            return false;
        }
        for &c in b {
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_') {
                return false;
            }
        }
    }
    true
}
