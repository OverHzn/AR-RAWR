//! Rewrite rule model + save-time validation — pure slice of
//! `traffic/rewrite/rule.go` (+ `compile.go` ErrorLog).
//!
//! Rules fail open: validation is strict at save time so a bad rule never runs.

use std::sync::Mutex;
use std::time::SystemTime;

use crate::db::error::{Error, Result};

pub const MAX_PATTERN_SIZE: usize = 1 << 10;
pub const MAX_VALUE_SIZE: usize = 1 << 20;

pub const PHASE_REQUEST: &str = "request";
pub const PHASE_RESPONSE: &str = "response";

pub const ACTION_SET_METHOD: &str = "set_method";
pub const ACTION_SET_PATH_QUERY: &str = "set_path_query";
pub const ACTION_REPLACE_URL: &str = "replace_url";
pub const ACTION_DELETE_HEADER: &str = "delete_header";
pub const ACTION_SET_HEADER: &str = "set_header";
pub const ACTION_REPLACE_HEADER: &str = "replace_header";
pub const ACTION_SET_BODY: &str = "set_body";
pub const ACTION_REPLACE_BODY: &str = "replace_body";
pub const ACTION_SET_STATUS: &str = "set_status";

pub const ERR_CLASS_INVALID_URL: &str = "invalid_url";
pub const ERR_CLASS_EXPANSION: &str = "expansion_limit";
pub const ERR_CLASS_PANIC: &str = "panic";
pub const ERR_CLASS_TRANSPORT: &str = "transport_error";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeaderMatcher {
    pub name: String,
    pub value_regex: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Matchers {
    pub methods: Vec<String>,
    pub host: String,
    pub url_regex: String,
    pub headers: Vec<HeaderMatcher>,
    pub body_regex: String,
    pub status: Vec<String>,
    pub resp_headers: Vec<HeaderMatcher>,
    pub resp_body_regex: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Action {
    pub typ: String,
    pub name: String,
    pub regex: String,
    pub value: String,
    pub sensitive: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub phase: String,
    pub priority: i32,
    pub matchers: Matchers,
    pub actions: Vec<Action>,
    pub note: String,
}

pub(crate) fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}

pub(crate) fn canonical_header_key(s: &str) -> String {
    s.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct StatusRange {
    pub(crate) lo: i32,
    pub(crate) hi: i32,
}

pub(crate) fn parse_status(s: &str) -> Option<StatusRange> {
    let s = s.trim().to_lowercase();
    let b = s.as_bytes();
    if b.len() != 3 || !(b'1'..=b'5').contains(&b[0]) {
        return None;
    }
    if &s[1..] == "xx" {
        let lo = (b[0] - b'0') as i32 * 100;
        return Some(StatusRange { lo, hi: lo + 100 });
    }
    let n = s.parse::<i32>().ok()?;
    Some(StatusRange { lo: n, hi: n + 1 })
}

pub(crate) fn body_allowed_for_status(status: i32) -> bool {
    !(100..=199).contains(&status) && status != 204 && status != 304
}

pub(crate) fn compile_host(h: &str) -> Result<(String, bool)> {
    let mut h = h.trim().to_lowercase();
    let mut glob = false;
    if h.starts_with("*.") {
        h = h[1..].to_string();
        glob = true;
    }
    let bare = h.strip_prefix('.').unwrap_or(&h);
    if bare.is_empty() || h.contains(['*', '/', ' ', '\t']) {
        return Err(Error::msg(
            "host: use an exact host name or a suffix glob such as *.example.com",
        ));
    }
    // A host with a port (host:digits) is refused.
    if h.matches(':').count() == 1 {
        if let Some((_, port)) = h.rsplit_once(':') {
            if !port.is_empty() && port.bytes().all(|c| c.is_ascii_digit()) {
                return Err(Error::msg("host: a host name without port is expected"));
            }
        }
    }
    Ok((h, glob))
}

fn compile_pattern(field: &str, expr: &str) -> Result<Option<regex::Regex>> {
    if expr.is_empty() {
        return Ok(None);
    }
    if expr.len() > MAX_PATTERN_SIZE {
        return Err(Error::msg(format!(
            "{field}: pattern longer than {MAX_PATTERN_SIZE} bytes"
        )));
    }
    let re = regex::Regex::new(expr).map_err(|e| Error::msg(format!("{field}: {e}")))?;
    Ok(Some(re))
}

fn required_pattern(expr: &str) -> Result<regex::Regex> {
    if expr.is_empty() {
        return Err(Error::msg("regex cannot be empty"));
    }
    compile_pattern("regex", expr)?.ok_or_else(|| Error::msg("regex cannot be empty"))
}

pub(crate) fn header_value(v: &str) -> Result<()> {
    if v.contains(['\r', '\n', '\0']) {
        return Err(Error::msg("value contains a line break or a NUL byte"));
    }
    Ok(())
}

pub(crate) fn header_target(name: &str) -> Result<String> {
    if !valid_token(name) {
        return Err(Error::msg("invalid header name"));
    }
    let c = canonical_header_key(name);
    if matches!(
        c.as_str(),
        "Host"
            | "Content-Length"
            | "Transfer-Encoding"
            | "Connection"
            | "Keep-Alive"
            | "Te"
            | "Trailer"
            | "Expect"
            | "Upgrade"
    ) || c.starts_with("Proxy-")
    {
        return Err(Error::msg(format!("header {c} cannot be rewritten")));
    }
    Ok(c)
}

pub(crate) fn parse_path_query(s: &str) -> bool {
    // Go uses url.ParseRequestURI: an absolute path (+ optional query), no raw
    // control characters or spaces.
    s.starts_with('/') && !s.chars().any(|c| c.is_control() || c == ' ')
}

pub(crate) fn action_class(typ: &str) -> i32 {
    match typ {
        ACTION_SET_METHOD | ACTION_SET_PATH_QUERY | ACTION_REPLACE_URL => 0,
        ACTION_DELETE_HEADER => 1,
        ACTION_SET_HEADER | ACTION_REPLACE_HEADER => 2,
        ACTION_SET_BODY | ACTION_REPLACE_BODY => 3,
        _ => 4,
    }
}

pub(crate) fn compile_action(phase: &str, a: &Action) -> Result<()> {
    if a.value.len() > MAX_VALUE_SIZE {
        return Err(Error::msg(format!(
            "value longer than {MAX_VALUE_SIZE} bytes"
        )));
    }
    match a.typ.as_str() {
        ACTION_SET_METHOD | ACTION_SET_PATH_QUERY | ACTION_REPLACE_URL => {
            if phase != PHASE_REQUEST {
                return Err(Error::msg("request-phase action"));
            }
        }
        ACTION_SET_STATUS if phase != PHASE_RESPONSE => {
            return Err(Error::msg("response-phase action"));
        }
        _ => {}
    }
    match a.typ.as_str() {
        ACTION_SET_METHOD => {
            if !valid_token(&a.value) || a.value.eq_ignore_ascii_case("CONNECT") {
                return Err(Error::msg(
                    "value must be an HTTP method other than CONNECT",
                ));
            }
        }
        ACTION_SET_PATH_QUERY => {
            if !parse_path_query(&a.value) {
                return Err(Error::msg(
                    "value must start with / and be a valid request URI",
                ));
            }
        }
        ACTION_REPLACE_URL | ACTION_REPLACE_BODY => {
            required_pattern(&a.regex)?;
        }
        ACTION_DELETE_HEADER => {
            header_target(&a.name)?;
        }
        ACTION_SET_HEADER => {
            header_target(&a.name)?;
            header_value(&a.value)?;
        }
        ACTION_REPLACE_HEADER => {
            header_target(&a.name)?;
            required_pattern(&a.regex)?;
            header_value(&a.value)?;
        }
        ACTION_SET_BODY => {}
        ACTION_SET_STATUS => {
            let n = a.value.parse::<i32>().map_err(|_| {
                Error::msg("value must be a status from 200 to 599, except 204 and 304")
            })?;
            if !(200..=599).contains(&n) || !body_allowed_for_status(n) {
                return Err(Error::msg(
                    "value must be a status from 200 to 599, except 204 and 304",
                ));
            }
        }
        _ => return Err(Error::msg("unknown action type")),
    }
    Ok(())
}

/// `Validate`: the first reason a rule cannot be saved. Messages never include
/// an action value (which may be a secret).
pub fn validate(r: &Rule) -> Result<()> {
    if r.name.trim().is_empty() {
        return Err(Error::msg("name cannot be empty"));
    }
    if r.phase != PHASE_REQUEST && r.phase != PHASE_RESPONSE {
        return Err(Error::msg("phase must be request or response"));
    }
    let m = &r.matchers;
    if r.phase == PHASE_REQUEST
        && (!m.status.is_empty() || !m.resp_headers.is_empty() || !m.resp_body_regex.is_empty())
    {
        return Err(Error::msg(
            "status, resp_headers and resp_body_regex are response-phase matchers",
        ));
    }
    for method in &m.methods {
        if !valid_token(method) {
            return Err(Error::msg("methods: invalid method"));
        }
    }
    if !m.host.is_empty() {
        compile_host(&m.host)?;
    }
    compile_pattern("url_regex", &m.url_regex)?;
    for h in &m.headers {
        if !valid_token(&h.name) {
            return Err(Error::msg("headers: invalid header name"));
        }
        compile_pattern("headers", &h.value_regex)?;
    }
    compile_pattern("body_regex", &m.body_regex)?;
    for s in &m.status {
        if parse_status(s).is_none() {
            return Err(Error::msg(format!(
                "status: {s:?} is neither a code (404) nor a class (4xx)"
            )));
        }
    }
    for h in &m.resp_headers {
        if !valid_token(&h.name) {
            return Err(Error::msg("resp_headers: invalid header name"));
        }
        compile_pattern("resp_headers", &h.value_regex)?;
    }
    compile_pattern("resp_body_regex", &m.resp_body_regex)?;
    if r.actions.is_empty() {
        return Err(Error::msg("a rule needs at least one action"));
    }
    for (i, a) in r.actions.iter().enumerate() {
        compile_action(&r.phase, a)
            .map_err(|e| Error::msg(format!("action {} ({}): {}", i + 1, a.typ, e)))?;
    }
    // Silence the unused helper warning — action_class drives execution order in
    // the (deferred) compiled Set.
    let _ = action_class("");
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct LastError {
    pub class: String,
    pub at: SystemTime,
    pub count: i64,
}

impl Default for LastError {
    fn default() -> Self {
        LastError {
            class: String::new(),
            at: SystemTime::UNIX_EPOCH,
            count: 0,
        }
    }
}

/// `ErrorLog`: the last run-time error per rule (zero value ready to use).
#[derive(Debug, Default)]
pub struct ErrorLog {
    m: Mutex<std::collections::HashMap<i64, LastError>>,
}

impl ErrorLog {
    pub fn record(&self, id: i64, class: &str) {
        let mut m = self.m.lock().unwrap();
        let count = m.get(&id).map(|e| e.count).unwrap_or(0);
        m.insert(
            id,
            LastError {
                class: class.to_string(),
                at: SystemTime::now(),
                count: count + 1,
            },
        );
    }
    pub fn get(&self, id: i64) -> Option<LastError> {
        self.m.lock().unwrap().get(&id).cloned()
    }
    pub fn forget(&self, id: i64) {
        self.m.lock().unwrap().remove(&id);
    }
}
