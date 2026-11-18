//! The Rust port of `traffic/rewrite/`.
//!
//! Ports `Validate(Rule) error` and the `compileRule` path: save-time validation
//! of a rewrite rule (name/phase/matchers/actions/regex/headers/status). The
//! regex compile-check uses the `regex` crate, which shares Go's RE2 syntax for
//! the constructs a rewrite rule uses. `rust-anything diff` checks behavior.

#![forbid(unsafe_code)]

use gocompat::regexerr::go_regex_error;
use gocompat::strings::{go_equal_fold, go_to_lower, go_to_upper, go_trim_space};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;

// ===========================================================================
// Limits (save-time)
// ===========================================================================

const MAX_PATTERN_SIZE: usize = 1 << 10; // 1024
const MAX_VALUE_SIZE: usize = 1 << 20; // 1048576

// ===========================================================================
// Action types
// ===========================================================================

const ACTION_SET_METHOD: &str = "set_method";
const ACTION_SET_PATH_QUERY: &str = "set_path_query";
const ACTION_REPLACE_URL: &str = "replace_url";
const ACTION_DELETE_HEADER: &str = "delete_header";
const ACTION_SET_HEADER: &str = "set_header";
const ACTION_REPLACE_HEADER: &str = "replace_header";
const ACTION_SET_BODY: &str = "set_body";
const ACTION_REPLACE_BODY: &str = "replace_body";
const ACTION_SET_STATUS: &str = "set_status";

// ===========================================================================
// Rule model (JSON-decoded from the wire)
// ===========================================================================

#[derive(Debug, Clone, Default)]
struct Rule {
    #[allow(dead_code)]
    id: i64,
    name: String,
    #[allow(dead_code)]
    enabled: bool,
    phase: String,
    #[allow(dead_code)]
    priority: i64,
    matchers: Matchers,
    actions: Vec<Action>,
    #[allow(dead_code)]
    note: String,
}

#[derive(Debug, Clone, Default)]
struct Matchers {
    methods: Vec<String>,
    host: String,
    url_regex: String,
    headers: Vec<HeaderMatcher>,
    body_regex: String,
    status: Vec<String>,
    resp_headers: Vec<HeaderMatcher>,
    resp_body_regex: String,
}

#[derive(Debug, Clone, Default)]
struct HeaderMatcher {
    name: String,
    value_regex: String,
}

#[derive(Debug, Clone, Default)]
struct Action {
    typ: String,
    name: String,
    regex: String,
    value: String,
    #[allow(dead_code)]
    sensitive: bool,
}

// ===========================================================================
// Execution classes (order)
// ===========================================================================

const CLASS_TARGET: i32 = 0; // method and URL
const CLASS_DELETE_HEADER: i32 = 1;
const CLASS_HEADER: i32 = 2;
const CLASS_BODY: i32 = 3;
const CLASS_STATUS: i32 = 4;

fn action_class(typ: &str) -> i32 {
    match typ {
        ACTION_SET_METHOD | ACTION_SET_PATH_QUERY | ACTION_REPLACE_URL => CLASS_TARGET,
        ACTION_DELETE_HEADER => CLASS_DELETE_HEADER,
        ACTION_SET_HEADER | ACTION_REPLACE_HEADER => CLASS_HEADER,
        ACTION_SET_BODY | ACTION_REPLACE_BODY => CLASS_BODY,
        _ => CLASS_STATUS,
    }
}

// ===========================================================================
// Validate: port of `Validate(r Rule) error` (traffic/rewrite/rule.go:100)
// ===========================================================================

/// Port of `Validate`. Returns `Err(message)` where Go returns an error, `Ok(())`
/// where Go returns nil.
pub fn validate(r: Value) -> Result<(), String> {
    let rule = parse_rule(&r);
    compile_rule(&rule).map(|_| ())
}

fn compile_rule(r: &Rule) -> Result<(), String> {
    if go_trim_space(&r.name).is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if r.phase != "request" && r.phase != "response" {
        return Err("phase must be request or response".to_string());
    }
    let mut methods: BTreeMap<String, bool> = BTreeMap::new();

    // compileMatchers
    let m = &r.matchers;
    if r.phase == "request"
        && (!m.status.is_empty() || !m.resp_headers.is_empty() || !m.resp_body_regex.is_empty())
    {
        return Err(
            "status, resp_headers and resp_body_regex are response-phase matchers".to_string(),
        );
    }
    if !m.methods.is_empty() {
        for method in &m.methods {
            if !valid_token(method) {
                return Err("methods: invalid method".to_string());
            }
            methods.insert(go_to_upper(method), true);
        }
    }
    if !m.host.is_empty() {
        // compile_host validates the host; the returned values are not needed for
        // Validate's error path.
        compile_host(&m.host)?;
    }
    compile_pattern("url_regex", &m.url_regex)?;
    compile_header_matchers("headers", &m.headers)?;
    compile_pattern("body_regex", &m.body_regex)?;
    for s in &m.status {
        if parse_status(s).is_none() {
            return Err(format!(
                "status: {:?} is neither a code (404) nor a class (4xx)",
                s
            ));
        }
    }
    compile_header_matchers("resp_headers", &m.resp_headers)?;
    compile_pattern("resp_body_regex", &m.resp_body_regex)?;

    // actions
    if r.actions.is_empty() {
        return Err("a rule needs at least one action".to_string());
    }
    let mut body_action = false;
    let mut compiled_actions: Vec<(String, i32)> = Vec::new(); // (typ, class)
    for (i, a) in r.actions.iter().enumerate() {
        let (typ, class) = compile_action(&r.phase, a)
            .map_err(|e| format!("action {} ({}): {}", i + 1, a.typ, e))?;
        if class == CLASS_BODY {
            body_action = true;
        }
        compiled_actions.push((typ, class));
    }
    // stable sort by class (only need the class ordering; no execution here)
    compiled_actions.sort_by_key(|(_, c)| *c);

    // reqBody/respBody are not needed for Validate's error path; ruleSnapshot too.
    let _ = body_action;
    Ok(())
}

// ===========================================================================
// compileHost (traffic/rewrite/rule.go:241)
// ===========================================================================

fn compile_host(h: &str) -> Result<(), String> {
    let mut h = go_to_lower(go_trim_space(h));
    if h.starts_with("*.") {
        h = h[1..].to_string(); // keep the leading dot
    }
    let bare = h.strip_prefix('.').unwrap_or(&h);
    if bare.is_empty() || h.contains('*') || h.contains('/') || h.contains(' ') || h.contains('\t')
    {
        return Err(
            "host: use an exact host name or a suffix glob such as *.example.com".to_string(),
        );
    }
    if split_host_port(&h).is_some() {
        return Err("host: a host name without port is expected".to_string());
    }
    Ok(())
}

// ===========================================================================
// compilePattern (traffic/rewrite/rule.go:256)
// ===========================================================================

fn compile_pattern(field: &str, expr: &str) -> Result<Option<Regex>, String> {
    if expr.is_empty() {
        return Ok(None);
    }
    if expr.len() > MAX_PATTERN_SIZE {
        return Err(format!(
            "{}: pattern longer than {} bytes",
            field, MAX_PATTERN_SIZE
        ));
    }
    match Regex::new(expr) {
        Ok(re) => Ok(Some(re)),
        Err(e) => Err(format!("{}: {}", field, go_regex_error(&e, expr))),
    }
}

// ===========================================================================
// compileHeaderMatchers (traffic/rewrite/rule.go:270)
// ===========================================================================

fn compile_header_matchers(
    field: &str,
    in_: &[HeaderMatcher],
) -> Result<Vec<(String, Option<Regex>)>, String> {
    let mut out = Vec::new();
    for h in in_ {
        if !valid_token(&h.name) {
            return Err(format!("{}: invalid header name", field));
        }
        let re = compile_pattern(field, &h.value_regex)?;
        out.push((canonical_header_key(&h.name), re));
    }
    Ok(out)
}

// ===========================================================================
// parseStatus (traffic/rewrite/rule.go:287)
// ===========================================================================

fn parse_status(s: &str) -> Option<(i64, i64)> {
    let s = go_to_lower(go_trim_space(s));
    let b = s.as_bytes();
    if b.len() != 3 || b[0] < b'1' || b[0] > b'5' {
        return None;
    }
    if &s[1..] == "xx" {
        let lo = (b[0] - b'0') as i64 * 100;
        return Some((lo, lo + 100));
    }
    let n: i64 = s.parse().ok()?;
    Some((n, n + 1))
}

// ===========================================================================
// compileAction (traffic/rewrite/rule.go:303)
// ===========================================================================

fn compile_action(phase: &str, a: &Action) -> Result<(String, i32), String> {
    let mut name = String::new();
    let mut re: Option<Regex> = None;
    let mut status_val: i64 = 0;

    if a.value.len() > MAX_VALUE_SIZE {
        return Err(format!("value longer than {} bytes", MAX_VALUE_SIZE));
    }

    // phase gate
    match a.typ.as_str() {
        ACTION_SET_METHOD | ACTION_SET_PATH_QUERY | ACTION_REPLACE_URL => {
            if phase != "request" {
                return Err("request-phase action".to_string());
            }
        }
        ACTION_SET_STATUS if phase != "response" => {
            return Err("response-phase action".to_string());
        }
        _ => {}
    }

    match a.typ.as_str() {
        ACTION_SET_METHOD => {
            if !valid_token(&a.value) || go_equal_fold(&a.value, "CONNECT") {
                return Err("value must be an HTTP method other than CONNECT".to_string());
            }
        }
        ACTION_SET_PATH_QUERY => {
            if parse_path_query(&a.value).is_none() {
                return Err("value must start with / and be a valid request URI".to_string());
            }
        }
        ACTION_REPLACE_URL | ACTION_REPLACE_BODY => {
            re = required_pattern(&a.regex)?;
        }
        ACTION_DELETE_HEADER => {
            name = header_target(&a.name)?;
        }
        ACTION_SET_HEADER => {
            name = header_target(&a.name)?;
            header_value(&a.value)?;
        }
        ACTION_REPLACE_HEADER => {
            name = header_target(&a.name)?;
            re = required_pattern(&a.regex)?;
            header_value(&a.value)?;
        }
        ACTION_SET_BODY => {}
        ACTION_SET_STATUS => {
            let n: i64 = match a.value.parse() {
                Ok(n) => n,
                Err(_) => {
                    return Err(
                        "value must be a status from 200 to 599, except 204 and 304".to_string()
                    );
                }
            };
            if !(200..=599).contains(&n) || !body_allowed_for_status(n) {
                return Err(
                    "value must be a status from 200 to 599, except 204 and 304".to_string()
                );
            }
            status_val = n;
        }
        _ => return Err("unknown action type".to_string()),
    }

    let _ = (name, re, status_val);
    Ok((a.typ.clone(), action_class(&a.typ)))
}

fn required_pattern(expr: &str) -> Result<Option<Regex>, String> {
    if expr.is_empty() {
        return Err("regex cannot be empty".to_string());
    }
    compile_pattern("regex", expr)
}

// ===========================================================================
// headerTarget (traffic/rewrite/rule.go:365)
// ===========================================================================

fn header_target(name: &str) -> Result<String, String> {
    if !valid_token(name) {
        return Err("invalid header name".to_string());
    }
    let c = canonical_header_key(name);
    match c.as_str() {
        "Host" | "Content-Length" | "Transfer-Encoding" | "Connection" | "Keep-Alive" | "Te"
        | "Trailer" | "Expect" | "Upgrade" => {
            return Err(format!("header {} cannot be rewritten", c));
        }
        _ => {}
    }
    if c.starts_with("Proxy-") {
        return Err(format!("header {} cannot be rewritten", c));
    }
    Ok(c)
}

fn header_value(v: &str) -> Result<(), String> {
    if v.contains('\r') || v.contains('\n') || v.contains('\0') {
        return Err("value contains a line break or a NUL byte".to_string());
    }
    Ok(())
}

// ===========================================================================
// validToken (traffic/rewrite/rule.go:388)
// ===========================================================================

fn valid_token(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    for &c in s.as_bytes() {
        let ok = matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9')
            || "!#$%&'*+-.^_`|~".as_bytes().contains(&c);
        if !ok {
            return false;
        }
    }
    true
}

// ===========================================================================
// parsePathQuery (traffic/rewrite/rule.go:406)
// ===========================================================================

fn parse_path_query(s: &str) -> Option<()> {
    if !s.starts_with('/') {
        return None;
    }
    // url.ParseRequestURI: must be an absolute path or absolute URL.
    // We only need the validity check for Validate: reject control chars &
    // malformed percent-encoding; accept the rest (scheme/host are not rewritten).
    if s.contains('\r') || s.contains('\n') || s.contains('\0') || s.contains(' ') {
        return None;
    }
    // Reject "http://..." / "https://..." (absolute URL, not a path).
    let lower = s.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return None;
    }
    // Validate percent-encoding: every '%' must be followed by two hex digits.
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() || !b[i + 1].is_ascii_hexdigit() || !b[i + 2].is_ascii_hexdigit() {
                return None;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Some(())
}

// ===========================================================================
// bodyAllowedForStatus (traffic/rewrite/rule.go:416)
// ===========================================================================

fn body_allowed_for_status(status: i64) -> bool {
    !(100..=199).contains(&status) && status != 204 && status != 304
}

// ===========================================================================
// net.SplitHostPort (subset: does the string look like host:port?)
// ===========================================================================

fn split_host_port(s: &str) -> Option<(String, String)> {
    // A valid host:port has exactly one colon (or [ipv6]:port). For Validate we
    // only need to know if a port is present, matching net.SplitHostPort's
    // "too many colons" / "missing port" errors.
    if s.starts_with('[') {
        // [ipv6]:port
        if let Some(close) = s.find(']') {
            let rest = &s[close + 1..];
            if let Some(port) = rest.strip_prefix(':') {
                return Some((s[1..close].to_string(), port.to_string()));
            }
        }
        return None;
    }
    let colons = s.matches(':').count();
    if colons == 1 {
        let idx = s.find(':').unwrap();
        return Some((s[..idx].to_string(), s[idx + 1..].to_string()));
    }
    None
}

// ===========================================================================
// net/http.CanonicalHeaderKey
// ===========================================================================

fn canonical_header_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = true;
    for c in s.chars() {
        if upper {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        upper = c == '-';
    }
    // Go's CanonicalHeaderKey returns s unchanged if it contains invalid chars;
    // valid_token already gates that, so this is safe.
    out
}

// ===========================================================================
// strconv.Atoi (for status)
// ===========================================================================

// (handled inline via str::parse in compile_action)

// ===========================================================================
// Input parsing (JSON -> Rule)
// ===========================================================================

fn parse_rule(v: &Value) -> Rule {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Rule::default(),
    };
    Rule {
        id: get_i64(obj, "id"),
        name: get_str(obj, "name"),
        enabled: obj
            .get("enabled")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        phase: get_str(obj, "phase"),
        priority: get_i64(obj, "priority"),
        matchers: parse_matchers(obj.get("matchers")),
        actions: parse_actions(obj.get("actions")),
        note: get_str(obj, "note"),
    }
}

fn parse_matchers(v: Option<&Value>) -> Matchers {
    let mut m = Matchers::default();
    let obj = match v.and_then(|x| x.as_object()) {
        Some(o) => o,
        None => return m,
    };
    m.methods = get_str_list(obj, "methods");
    m.host = get_str(obj, "host");
    m.url_regex = get_str(obj, "url_regex");
    m.headers = parse_header_matchers(obj.get("headers"));
    m.body_regex = get_str(obj, "body_regex");
    m.status = get_str_list(obj, "status");
    m.resp_headers = parse_header_matchers(obj.get("resp_headers"));
    m.resp_body_regex = get_str(obj, "resp_body_regex");
    m
}

fn parse_header_matchers(v: Option<&Value>) -> Vec<HeaderMatcher> {
    let mut out = Vec::new();
    if let Some(arr) = v.and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(o) = item.as_object() {
                out.push(HeaderMatcher {
                    name: get_str(o, "name"),
                    value_regex: get_str(o, "value_regex"),
                });
            }
        }
    }
    out
}

fn parse_actions(v: Option<&Value>) -> Vec<Action> {
    let mut out = Vec::new();
    if let Some(arr) = v.and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(o) = item.as_object() {
                out.push(Action {
                    typ: get_str(o, "type"),
                    name: get_str(o, "name"),
                    regex: get_str(o, "regex"),
                    value: get_str(o, "value"),
                    sensitive: o
                        .get("sensitive")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                });
            }
        }
    }
    out
}

fn get_str(obj: &serde_json::Map<String, Value>, k: &str) -> String {
    obj.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn get_i64(obj: &serde_json::Map<String, Value>, k: &str) -> i64 {
    obj.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn get_str_list(obj: &serde_json::Map<String, Value>, k: &str) -> Vec<String> {
    obj.get(k)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|x| x.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}
