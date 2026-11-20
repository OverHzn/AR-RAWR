//! Rewrite engine — port of `traffic/rewrite/{compile,engine,trace}.go`.
//!
//! Rules form an immutable, sorted snapshot. `Apply` runs one phase as a
//! pipeline: each rule sees the previous rule's output. A rule is atomic — a
//! failed action (or panic) leaves the message as that rule found it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use hmac::{Hmac, Mac};
use regex::bytes::Regex as BRegex;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::rewrite_rule::{
    action_class, body_allowed_for_status, canonical_header_key, compile_host, header_target,
    header_value, parse_path_query, parse_status, valid_token, Action, HeaderMatcher, Rule,
    ERR_CLASS_EXPANSION, ERR_CLASS_INVALID_URL, ERR_CLASS_PANIC, MAX_PATTERN_SIZE, MAX_VALUE_SIZE,
    PHASE_REQUEST, PHASE_RESPONSE,
};

pub const MAX_BODY_SIZE: usize = 1 << 20;
pub const MAX_EXPANSION: usize = 1 << 20;
const SNAPSHOT_VALUE_MAX: usize = 4 << 10;

pub const OUTCOME_APPLIED: &str = "applied";
pub const OUTCOME_SKIPPED: &str = "skipped";
pub const OUTCOME_ERROR: &str = "error";

pub const REASON_STREAM: &str = "not rewritten: stream";
pub const REASON_BINARY: &str = "not rewritten: binary";
pub const REASON_BODY_TOO_LARGE: &str = "not rewritten: body too large";
pub const REASON_CONTENT_ENCODING: &str = "not rewritten: content-encoding";
pub const REASON_NO_BODY_ALLOWED: &str = "not rewritten: no body allowed";
pub const REASON_CONNECTION_HEADER: &str = "header named in Connection";

// ---------------------------------------------------------------------------
// Header (Go http.Header with canonical keys)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header(pub BTreeMap<String, Vec<String>>);

impl Header {
    pub fn from_values(entries: &[(&str, &[&str])]) -> Header {
        let mut m = BTreeMap::new();
        for (name, vals) in entries {
            m.insert(
                canonical_header_key(name),
                vals.iter().map(|s| s.to_string()).collect(),
            );
        }
        Header(m)
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .get(&canonical_header_key(name))
            .and_then(|v| v.first())
            .map(|s| s.as_str())
    }
    pub fn values(&self, name: &str) -> Vec<String> {
        self.0
            .get(&canonical_header_key(name))
            .cloned()
            .unwrap_or_default()
    }
    pub fn set(&mut self, name: &str, value: &str) {
        self.0
            .insert(canonical_header_key(name), vec![value.to_string()]);
    }
    pub fn set_values(&mut self, name: &str, vals: Vec<String>) {
        self.0.insert(canonical_header_key(name), vals);
    }
    pub fn del(&mut self, name: &str) {
        self.0.remove(&canonical_header_key(name));
    }
}

// ---------------------------------------------------------------------------
// A small Go-net/url-like URL: only the parts the engine reads or writes.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GUrl {
    pub scheme: String,
    pub userinfo: String,
    pub host: String,
    pub path: String,
    pub raw_query: String,
    pub force_query: bool,
}

fn split_tail(tail: &str) -> (String, String, bool) {
    let tail = tail.split('#').next().unwrap_or("");
    match tail.find('?') {
        Some(i) => {
            let after = &tail[i + 1..];
            (tail[..i].to_string(), after.to_string(), after.is_empty())
        }
        None => (tail.to_string(), String::new(), false),
    }
}

impl GUrl {
    pub fn parse(raw: &str) -> Option<GUrl> {
        let i = raw.find("://")?;
        let scheme = &raw[..i];
        if scheme.is_empty() || !scheme.chars().next()?.is_ascii_alphabetic() {
            return None;
        }
        let rest = &raw[i + 3..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let (path, raw_query, force_query) = split_tail(&rest[end..]);
        let (userinfo, host) = match authority.rsplit_once('@') {
            Some((u, h)) => (u.to_string(), h.to_string()),
            None => (String::new(), authority.to_string()),
        };
        if host.is_empty() {
            return None;
        }
        Some(GUrl {
            scheme: scheme.to_string(),
            userinfo,
            host,
            path,
            raw_query,
            force_query,
        })
    }

    pub fn request_uri(&self) -> String {
        let mut r = if self.path.is_empty() {
            "/".to_string()
        } else {
            self.path.clone()
        };
        if self.force_query && !r.ends_with('?') {
            r.push('?');
        } else if !self.raw_query.is_empty() {
            r.push('?');
            r.push_str(&self.raw_query);
        }
        r
    }

    pub fn hostname(&self) -> String {
        let h = self
            .host
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(&self.host);
        h.trim_start_matches('[')
            .trim_end_matches(']')
            .to_lowercase()
    }

    pub fn port(&self) -> Option<String> {
        if self.host.starts_with('[') {
            let (_, tail) = self.host.rsplit_once(']')?;
            return tail
                .strip_prefix(':')
                .map(|p| p.to_string())
                .filter(|p| !p.is_empty());
        }
        match self.host.rsplit_once(':') {
            Some((_, p)) if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) => {
                Some(p.to_string())
            }
            _ => None,
        }
    }

    pub fn set_path_query(&mut self, value: &str) {
        let (path, query, force) = split_tail(value);
        self.path = path;
        self.raw_query = query;
        self.force_query = force;
    }
}

impl fmt::Display for GUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme)?;
        if !self.userinfo.is_empty() {
            write!(f, "{}@", self.userinfo)?;
        }
        write!(f, "{}{}", self.host, self.path)?;
        if !self.raw_query.is_empty() || self.force_query {
            write!(f, "?{}", self.raw_query)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: GUrl,
    pub header: Header,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, raw_url: &str, header: Header, body: &[u8]) -> Request {
        Request {
            method: method.to_string(),
            url: GUrl::parse(raw_url).unwrap_or_else(|| GUrl {
                scheme: String::new(),
                userinfo: String::new(),
                host: String::new(),
                path: String::new(),
                raw_query: String::new(),
                force_query: false,
            }),
            header,
            body: body.to_vec(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: i32,
    pub header: Header,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: i32, header: Header, body: &[u8]) -> Response {
        Response {
            status,
            header,
            body: body.to_vec(),
        }
    }
}

pub type BinaryFn = dyn Fn(&str, &[u8]) -> bool;

pub struct Input {
    pub request: Request,
    pub response: Response,
    pub stream: bool,
    pub is_binary: Option<Box<BinaryFn>>,
}

impl Input {
    pub fn new(req: Request) -> Input {
        Input {
            request: req,
            response: Response::new(200, Header::default(), &[]),
            stream: false,
            is_binary: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TraceLine {
    pub phase: String,
    pub rule_id: i64,
    pub rule_snapshot: String,
    pub action: String,
    pub field: String,
    pub outcome: String,
    pub reason: String,
    pub before_hash: String,
    pub after_hash: String,
    pub after: Option<Vec<u8>>,
}

impl TraceLine {
    pub fn outcome_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.rule_id, self.action, self.field, self.outcome, self.reason
        )
    }
}

#[derive(Debug)]
pub struct ApplyResult {
    pub request: Request,
    pub response: Response,
    pub changed: bool,
    pub lines: Vec<TraceLine>,
}

// ---------------------------------------------------------------------------
// Snapshot JSON (must match Go ruleSnapshot field names)
// ---------------------------------------------------------------------------

fn is_zero_i(v: &usize) -> bool {
    *v == 0
}
fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Serialize)]
struct SnapHeader {
    name: String,
    #[serde(rename = "value_regex", skip_serializing_if = "String::is_empty")]
    value_regex: String,
}

#[derive(Serialize)]
struct SnapMatchers {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    methods: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    host: String,
    #[serde(rename = "url_regex", skip_serializing_if = "String::is_empty")]
    url_regex: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    headers: Vec<SnapHeader>,
    #[serde(rename = "body_regex", skip_serializing_if = "String::is_empty")]
    body_regex: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    status: Vec<String>,
    #[serde(rename = "resp_headers", skip_serializing_if = "Vec::is_empty")]
    resp_headers: Vec<SnapHeader>,
    #[serde(rename = "resp_body_regex", skip_serializing_if = "String::is_empty")]
    resp_body_regex: String,
}

#[derive(Serialize)]
struct SnapAction {
    #[serde(rename = "type")]
    typ: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    regex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    sensitive: bool,
    #[serde(rename = "value_len", skip_serializing_if = "is_zero_i")]
    value_len: usize,
    #[serde(rename = "value_sha256", skip_serializing_if = "String::is_empty")]
    value_sha256: String,
}

#[derive(Serialize)]
struct RuleSnapshot {
    id: i64,
    name: String,
    phase: String,
    priority: i32,
    matchers: SnapMatchers,
    actions: Vec<SnapAction>,
}

fn sha256_hex(b: &[u8]) -> String {
    let sum = Sha256::digest(b);
    let mut out = String::with_capacity(32 * 2);
    for x in sum {
        out.push_str(&format!("{x:02x}"));
    }
    out
}

fn snap_headers(v: &[HeaderMatcher]) -> Vec<SnapHeader> {
    v.iter()
        .map(|h| SnapHeader {
            name: h.name.clone(),
            value_regex: h.value_regex.clone(),
        })
        .collect()
}

fn rule_snapshot(r: &Rule) -> String {
    let m = &r.matchers;
    let actions = r
        .actions
        .iter()
        .map(|a| {
            let mut sa = SnapAction {
                typ: a.typ.clone(),
                name: a.name.clone(),
                regex: a.regex.clone(),
                value: None,
                sensitive: a.sensitive,
                value_len: 0,
                value_sha256: String::new(),
            };
            if !a.sensitive {
                if a.value.len() > SNAPSHOT_VALUE_MAX {
                    sa.value_len = a.value.len();
                    sa.value_sha256 = sha256_hex(a.value.as_bytes());
                } else {
                    sa.value = Some(a.value.clone());
                }
            }
            sa
        })
        .collect();
    let snap = RuleSnapshot {
        id: r.id,
        name: r.name.clone(),
        phase: r.phase.clone(),
        priority: r.priority,
        matchers: SnapMatchers {
            methods: m.methods.clone(),
            host: m.host.clone(),
            url_regex: m.url_regex.clone(),
            headers: snap_headers(&m.headers),
            body_regex: m.body_regex.clone(),
            status: m.status.clone(),
            resp_headers: snap_headers(&m.resp_headers),
            resp_body_regex: m.resp_body_regex.clone(),
        },
        actions,
    };
    serde_json::to_string(&snap).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct HeaderMatcherC {
    name: String,
    re: Option<BRegex>,
}

#[derive(Clone)]
struct CompiledAction {
    typ: String,
    name: String,
    re: Option<BRegex>,
    value: String,
    status: i32,
    sensitive: bool,
}

impl CompiledAction {
    fn field(&self) -> String {
        match action_class(&self.typ) {
            0 => {
                if self.typ == "set_method" {
                    "method".to_string()
                } else {
                    "url".to_string()
                }
            }
            1 | 2 => format!("header:{}", self.name),
            3 => "body".to_string(),
            _ => "status".to_string(),
        }
    }
}

#[derive(Clone)]
struct CompiledRule {
    id: i64,
    phase: String,
    snapshot: String,
    methods: Option<HashMap<String, bool>>,
    host: String,
    host_glob: bool,
    url_re: Option<BRegex>,
    headers: Vec<HeaderMatcherC>,
    body_re: Option<BRegex>,
    status: Vec<super::rewrite_rule::StatusRange>,
    resp_headers: Vec<HeaderMatcherC>,
    resp_body_re: Option<BRegex>,
    actions: Vec<CompiledAction>,
    req_body: bool,
    resp_body: bool,
}

fn compile_pattern(field: &str, expr: &str) -> Result<Option<BRegex>, String> {
    if expr.is_empty() {
        return Ok(None);
    }
    if expr.len() > MAX_PATTERN_SIZE {
        return Err(format!(
            "{field}: pattern longer than {MAX_PATTERN_SIZE} bytes"
        ));
    }
    BRegex::new(expr)
        .map(Some)
        .map_err(|e| format!("{field}: {e}"))
}

fn required_pattern(expr: &str) -> Result<Option<BRegex>, String> {
    if expr.is_empty() {
        return Err("regex cannot be empty".to_string());
    }
    compile_pattern("regex", expr)
}

fn compile_header_matchers(
    field: &str,
    in_m: &[HeaderMatcher],
) -> Result<Vec<HeaderMatcherC>, String> {
    let mut out = Vec::new();
    for h in in_m {
        if !valid_token(&h.name) {
            return Err(format!("{field}: invalid header name"));
        }
        out.push(HeaderMatcherC {
            name: canonical_header_key(&h.name),
            re: compile_pattern(field, &h.value_regex)?,
        });
    }
    Ok(out)
}

fn compile_action(phase: &str, a: &Action) -> Result<CompiledAction, String> {
    let mut ca = CompiledAction {
        typ: a.typ.clone(),
        name: String::new(),
        re: None,
        value: a.value.clone(),
        status: 0,
        sensitive: a.sensitive,
    };
    if a.value.len() > MAX_VALUE_SIZE {
        return Err(format!("value longer than {MAX_VALUE_SIZE} bytes"));
    }
    match a.typ.as_str() {
        "set_method" | "set_path_query" | "replace_url" if phase != PHASE_REQUEST => {
            return Err("request-phase action".to_string());
        }
        "set_status" if phase != PHASE_RESPONSE => {
            return Err("response-phase action".to_string());
        }
        _ => {}
    }
    match a.typ.as_str() {
        "set_method" => {
            if !valid_token(&a.value) || a.value.eq_ignore_ascii_case("CONNECT") {
                return Err("value must be an HTTP method other than CONNECT".to_string());
            }
        }
        "set_path_query" => {
            if !parse_path_query(&a.value) {
                return Err("value must start with / and be a valid request URI".to_string());
            }
        }
        "replace_url" | "replace_body" => {
            ca.re = required_pattern(&a.regex)?;
        }
        "delete_header" => {
            ca.name = header_target(&a.name).map_err(|e| e.to_string())?;
        }
        "set_header" => {
            ca.name = header_target(&a.name).map_err(|e| e.to_string())?;
            header_value(&a.value).map_err(|e| e.to_string())?;
        }
        "replace_header" => {
            ca.name = header_target(&a.name).map_err(|e| e.to_string())?;
            ca.re = required_pattern(&a.regex)?;
            header_value(&a.value).map_err(|e| e.to_string())?;
        }
        "set_body" => {}
        "set_status" => {
            let n = a.value.parse::<i32>().map_err(|_| {
                "value must be a status from 200 to 599, except 204 and 304".to_string()
            })?;
            if !(200..=599).contains(&n) || !body_allowed_for_status(n) {
                return Err(
                    "value must be a status from 200 to 599, except 204 and 304".to_string()
                );
            }
            ca.status = n;
        }
        _ => return Err("unknown action type".to_string()),
    }
    Ok(ca)
}

fn compile_rule(r: &Rule) -> Result<CompiledRule, String> {
    if r.name.trim().is_empty() {
        return Err("name cannot be empty".to_string());
    }
    if r.phase != PHASE_REQUEST && r.phase != PHASE_RESPONSE {
        return Err("phase must be request or response".to_string());
    }
    let m = &r.matchers;
    if r.phase == PHASE_REQUEST
        && (!m.status.is_empty() || !m.resp_headers.is_empty() || !m.resp_body_regex.is_empty())
    {
        return Err(
            "status, resp_headers and resp_body_regex are response-phase matchers".to_string(),
        );
    }
    let methods = if m.methods.is_empty() {
        None
    } else {
        let mut set = HashMap::new();
        for method in &m.methods {
            if !valid_token(method) {
                return Err("methods: invalid method".to_string());
            }
            set.insert(method.to_uppercase(), true);
        }
        Some(set)
    };
    let (host, host_glob) = if m.host.is_empty() {
        (String::new(), false)
    } else {
        compile_host(&m.host).map_err(|e| e.to_string())?
    };
    let url_re = compile_pattern("url_regex", &m.url_regex)?;
    let headers = compile_header_matchers("headers", &m.headers)?;
    let body_re = compile_pattern("body_regex", &m.body_regex)?;
    let mut status = Vec::new();
    for s in &m.status {
        let Some(sr) = parse_status(s) else {
            return Err(format!(
                "status: {s:?} is neither a code (404) nor a class (4xx)"
            ));
        };
        status.push(sr);
    }
    let resp_headers = compile_header_matchers("resp_headers", &m.resp_headers)?;
    let resp_body_re = compile_pattern("resp_body_regex", &m.resp_body_regex)?;

    if r.actions.is_empty() {
        return Err("a rule needs at least one action".to_string());
    }
    let mut actions = Vec::new();
    let mut body_action = false;
    for (i, a) in r.actions.iter().enumerate() {
        let ca = compile_action(&r.phase, a)
            .map_err(|e| format!("action {} ({}): {}", i + 1, a.typ, e))?;
        body_action |= action_class(&ca.typ) == 3;
        actions.push(ca);
    }
    actions.sort_by_key(|a| action_class(&a.typ));

    let (req_body, resp_body) = if r.phase == PHASE_REQUEST {
        (body_re.is_some() || body_action, false)
    } else {
        (body_re.is_some(), resp_body_re.is_some() || body_action)
    };
    Ok(CompiledRule {
        id: r.id,
        phase: r.phase.clone(),
        snapshot: rule_snapshot(r),
        methods,
        host,
        host_glob,
        url_re,
        headers,
        body_re,
        status,
        resp_headers,
        resp_body_re,
        actions,
        req_body,
        resp_body,
    })
}

// ---------------------------------------------------------------------------
// Set: immutable compiled snapshot
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Options {
    pub trace_key: Vec<u8>,
    pub errors: Option<Arc<super::rewrite_rule::ErrorLog>>,
}

pub struct Set {
    request: Vec<CompiledRule>,
    response: Vec<CompiledRule>,
    load_errors: BTreeMap<i64, String>,
    trace_key: Vec<u8>,
    errors: Option<Arc<super::rewrite_rule::ErrorLog>>,
}

impl Set {
    pub fn compile(rules: &[Rule], opts: Options) -> Set {
        let mut enabled: Vec<Rule> = rules.iter().filter(|r| r.enabled).cloned().collect();
        enabled.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
        let mut s = Set {
            request: Vec::new(),
            response: Vec::new(),
            load_errors: BTreeMap::new(),
            trace_key: opts.trace_key,
            errors: opts.errors,
        };
        for r in enabled {
            match compile_rule(&r) {
                Ok(c) => {
                    if c.phase == PHASE_REQUEST {
                        s.request.push(c);
                    } else {
                        s.response.push(c);
                    }
                }
                Err(e) => {
                    s.load_errors.insert(r.id, e);
                }
            }
        }
        s
    }

    pub fn len(&self, phase: &str) -> usize {
        if phase == PHASE_REQUEST {
            self.request.len()
        } else {
            self.response.len()
        }
    }

    pub fn is_empty(&self, phase: &str) -> bool {
        self.len(phase) == 0
    }

    pub fn load_errors(&self) -> &BTreeMap<i64, String> {
        &self.load_errors
    }

    pub fn note_error(&self, id: i64, class: &str) {
        if let Some(e) = &self.errors {
            e.record(id, class);
        }
    }

    pub fn fingerprint(&self, b: &[u8]) -> String {
        if self.trace_key.is_empty() {
            return String::new();
        }
        let Ok(mut m) = Hmac::<Sha256>::new_from_slice(&self.trace_key) else {
            return String::new();
        };
        m.update(b);
        let out = m.finalize().into_bytes();
        let mut s = String::with_capacity(16 * 2);
        for x in &out[..16] {
            s.push_str(&format!("{x:02x}"));
        }
        s
    }

    fn rules(&self, phase: &str) -> &[CompiledRule] {
        if phase == PHASE_REQUEST {
            &self.request
        } else {
            &self.response
        }
    }

    pub fn apply(&self, phase: &str, input: Input) -> ApplyResult {
        let mut res = ApplyResult {
            request: input.request.clone(),
            response: input.response.clone(),
            changed: false,
            lines: Vec::new(),
        };
        if self.len(phase) == 0 {
            return res;
        }
        let mut run = Run {
            set: self,
            phase,
            stream: input.stream,
            is_binary: input.is_binary.as_deref(),
            st: State {
                req: input.request,
                resp: input.response,
            },
            tainted: HashSet::new(),
            seen: HashSet::new(),
            lines: Vec::new(),
            changed: false,
        };
        let upgrade = run
            .st
            .req
            .header
            .values("Upgrade")
            .iter()
            .any(|v| !v.is_empty());
        for r in self.rules(phase) {
            run.apply_rule(r, upgrade);
        }
        // The trace keeps the body of the last body action of the phase only.
        let last = run
            .lines
            .iter()
            .rposition(|l| l.field == "body" && l.outcome == OUTCOME_APPLIED);
        for (i, l) in run.lines.iter_mut().enumerate() {
            if l.field == "body" && Some(i) != last {
                l.after = None;
            }
        }
        res.lines = run.lines;
        if run.changed {
            res.changed = true;
            res.request = run.st.req;
            res.response = run.st.resp;
        }
        res
    }
}

// ---------------------------------------------------------------------------
// Run: one phase's mutable pipeline state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct State {
    req: Request,
    resp: Response,
}

struct Run<'a> {
    set: &'a Set,
    phase: &'a str,
    stream: bool,
    is_binary: Option<&'a BinaryFn>,
    st: State,
    tainted: HashSet<String>,
    seen: HashSet<i64>,
    lines: Vec<TraceLine>,
    changed: bool,
}

fn header_mut<'a>(st: &'a mut State, phase: &str) -> &'a mut Header {
    if phase == PHASE_REQUEST {
        &mut st.req.header
    } else {
        &mut st.resp.header
    }
}

fn body_mut<'a>(st: &'a mut State, phase: &str) -> &'a mut Vec<u8> {
    if phase == PHASE_REQUEST {
        &mut st.req.body
    } else {
        &mut st.resp.body
    }
}

fn ensure_header<'a>(st: &'a mut State, own: &mut bool, phase: &str) -> &'a mut Header {
    if !*own {
        let cloned = header_mut(st, phase).clone();
        *header_mut(st, phase) = cloned;
        *own = true;
    }
    header_mut(st, phase)
}

impl<'a> Run<'a> {
    fn hash(&self, b: &[u8], secret: bool) -> String {
        if secret {
            self.set.fingerprint(b)
        } else {
            sha256_hex(b)
        }
    }

    fn apply_rule(&mut self, r: &CompiledRule, upgrade: bool) {
        let result = catch_unwind(AssertUnwindSafe(|| self.apply_rule_inner(r, upgrade)));
        if result.is_err() {
            self.fail(r, "", "", ERR_CLASS_PANIC);
        }
    }

    fn apply_rule_inner(&mut self, r: &CompiledRule, upgrade: bool) {
        if !r.match_head(&self.st) {
            return;
        }
        if upgrade {
            self.skip(r, REASON_STREAM);
            return;
        }
        if let Some(reason) = self.ineligible(r) {
            self.skip(r, &reason);
            return;
        }
        if !r.match_body(&self.st) {
            return;
        }
        if r.writes_connection_header(header_mut(&mut self.st, self.phase)) {
            self.skip(r, REASON_CONNECTION_HEADER);
            return;
        }
        let mut st = self.st.clone();
        let mut own_header = false;
        let mut lines = Vec::new();
        let mut taint: Vec<String> = Vec::new();
        for a in &r.actions {
            if let Some(class) = self.exec(a, &mut st, &mut own_header, &mut lines, &mut taint) {
                self.fail(r, &a.typ, &a.field(), &class);
                return;
            }
        }
        if lines.is_empty() {
            return;
        }
        self.st = st;
        self.changed = true;
        for f in taint {
            self.tainted.insert(f);
        }
        for l in lines {
            self.emit(r, l);
        }
    }

    fn ineligible(&self, r: &CompiledRule) -> Option<String> {
        if r.resp_body {
            if self.stream {
                return Some(REASON_STREAM.to_string());
            }
            if self.st.req.method == "HEAD" || !body_allowed_for_status(self.st.resp.status) {
                return Some(REASON_NO_BODY_ALLOWED.to_string());
            }
            if let Some(reason) = self.body_ineligible(&self.st.resp.header, &self.st.resp.body) {
                return Some(reason);
            }
        }
        if r.req_body {
            return self.body_ineligible(&self.st.req.header, &self.st.req.body);
        }
        None
    }

    fn body_ineligible(&self, h: &Header, body: &[u8]) -> Option<String> {
        if body.len() > MAX_BODY_SIZE {
            return Some(REASON_BODY_TOO_LARGE.to_string());
        }
        if let Some(ce) = h.get("Content-Encoding") {
            let ce = ce.trim();
            if !ce.is_empty() && !ce.eq_ignore_ascii_case("identity") {
                return Some(REASON_CONTENT_ENCODING.to_string());
            }
        }
        let binary = self.is_binary.unwrap_or(&nul_in_head);
        let ct = h.get("Content-Type").unwrap_or("");
        if binary(ct, body) {
            return Some(REASON_BINARY.to_string());
        }
        None
    }

    fn note(
        &self,
        a: &CompiledAction,
        taint: &mut Vec<String>,
        before: &[u8],
        had_before: bool,
        after: &[u8],
        has_after: bool,
    ) -> TraceLine {
        let field = a.field();
        let tainted = self.tainted.contains(&field) || taint.contains(&field);
        let secret = tainted || a.sensitive;
        let mut l = TraceLine {
            action: a.typ.clone(),
            field,
            outcome: OUTCOME_APPLIED.to_string(),
            ..TraceLine::default()
        };
        if had_before {
            l.before_hash = self.hash(before, tainted);
        }
        if has_after {
            l.after_hash = self.hash(after, secret);
            if !secret {
                l.after = Some(after.to_vec());
            }
        }
        if a.sensitive && !tainted {
            taint.push(l.field.clone());
        }
        l
    }

    fn emit(&mut self, r: &CompiledRule, mut l: TraceLine) {
        l.phase = self.phase.to_string();
        l.rule_id = r.id;
        if !self.seen.contains(&r.id) {
            self.seen.insert(r.id);
            l.rule_snapshot = r.snapshot.clone();
        }
        self.lines.push(l);
    }

    fn skip(&mut self, r: &CompiledRule, reason: &str) {
        let l = TraceLine {
            outcome: OUTCOME_SKIPPED.to_string(),
            reason: reason.to_string(),
            ..TraceLine::default()
        };
        self.emit(r, l);
    }

    fn fail(&mut self, r: &CompiledRule, action: &str, field: &str, class: &str) {
        self.set.note_error(r.id, class);
        let l = TraceLine {
            action: action.to_string(),
            field: field.to_string(),
            outcome: OUTCOME_ERROR.to_string(),
            reason: class.to_string(),
            ..TraceLine::default()
        };
        self.emit(r, l);
    }

    fn exec(
        &self,
        a: &CompiledAction,
        st: &mut State,
        own_header: &mut bool,
        lines: &mut Vec<TraceLine>,
        taint: &mut Vec<String>,
    ) -> Option<String> {
        match a.typ.as_str() {
            "set_method" => {
                if st.req.method != a.value {
                    let before = st.req.method.clone().into_bytes();
                    let line = self.note(a, taint, &before, true, a.value.as_bytes(), true);
                    lines.push(line);
                    st.req.method = a.value.clone();
                }
            }
            "set_path_query" | "replace_url" => {
                let before = st.req.url.request_uri();
                let after = if a.typ == "replace_url" {
                    let (out, ok) =
                        expand_replace(a.re.as_ref().unwrap(), before.as_bytes(), &a.value);
                    if !ok {
                        return Some(ERR_CLASS_EXPANSION.to_string());
                    }
                    String::from_utf8_lossy(&out).into_owned()
                } else {
                    a.value.clone()
                };
                if after == before {
                    return None;
                }
                if !parse_path_query(&after) {
                    return Some(ERR_CLASS_INVALID_URL.to_string());
                }
                let mut nu = st.req.url.clone();
                nu.set_path_query(&after);
                let new_uri = nu.request_uri();
                let line = self.note(a, taint, before.as_bytes(), true, new_uri.as_bytes(), true);
                lines.push(line);
                st.req.url = nu;
            }
            "delete_header" | "set_header" | "replace_header" => {
                let h = ensure_header(st, own_header, self.phase);
                let old = h.values(&a.name);
                let next: Vec<String> = match a.typ.as_str() {
                    "set_header" => vec![a.value.clone()],
                    "replace_header" => {
                        let mut out = Vec::new();
                        for v in &old {
                            let (expanded, ok) =
                                expand_replace(a.re.as_ref().unwrap(), v.as_bytes(), &a.value);
                            if !ok {
                                return Some(ERR_CLASS_EXPANSION.to_string());
                            }
                            out.push(String::from_utf8_lossy(&expanded).into_owned());
                        }
                        out
                    }
                    _ => Vec::new(),
                };
                if old == next {
                    return None;
                }
                let before = old.join("\n").into_bytes();
                let after = next.join("\n").into_bytes();
                let line = self.note(a, taint, &before, !old.is_empty(), &after, !next.is_empty());
                lines.push(line);
                if next.is_empty() {
                    h.del(&a.name);
                } else {
                    h.set_values(&a.name, next);
                }
            }
            "set_body" | "replace_body" => {
                let cur = body_mut(st, self.phase).clone();
                let next = if a.typ == "replace_body" {
                    let (out, ok) = expand_replace(a.re.as_ref().unwrap(), &cur, &a.value);
                    if !ok {
                        return Some(ERR_CLASS_EXPANSION.to_string());
                    }
                    out
                } else {
                    a.value.as_bytes().to_vec()
                };
                if cur == next {
                    return None;
                }
                let line = self.note(a, taint, &cur, true, &next, true);
                lines.push(line);
                *body_mut(st, self.phase) = next;
                let h = header_mut(st, self.phase);
                for name in ["Content-Length", "Etag", "Content-Md5", "Content-Encoding"] {
                    h.del(name);
                }
            }
            "set_status" if st.resp.status != a.status => {
                let before = st.resp.status.to_string().into_bytes();
                let line = self.note(a, taint, &before, true, a.value.as_bytes(), true);
                lines.push(line);
                st.resp.status = a.status;
            }
            _ => {}
        }
        None
    }
}

fn nul_in_head(_content_type: &str, body: &[u8]) -> bool {
    body[..body.len().min(512)].contains(&0)
}

impl CompiledRule {
    fn match_head(&self, st: &State) -> bool {
        if let Some(methods) = &self.methods {
            if !methods.contains_key(&st.req.method.to_uppercase()) {
                return false;
            }
        }
        if !self.host.is_empty() {
            let h = st.req.url.hostname();
            if self.host_glob {
                if h.len() <= self.host.len() || !h.ends_with(&self.host) {
                    return false;
                }
            } else if h != self.host {
                return false;
            }
        }
        if !self.status.is_empty()
            && !self
                .status
                .iter()
                .any(|s| st.resp.status >= s.lo && st.resp.status < s.hi)
        {
            return false;
        }
        if !match_headers(&self.headers, &st.req.header)
            || !match_headers(&self.resp_headers, &st.resp.header)
        {
            return false;
        }
        if let Some(re) = &self.url_re {
            if !re.is_match(st.req.url.request_uri().as_bytes()) {
                return false;
            }
        }
        true
    }

    fn match_body(&self, st: &State) -> bool {
        self.body_re
            .as_ref()
            .map(|re| re.is_match(&st.req.body))
            .unwrap_or(true)
            && self
                .resp_body_re
                .as_ref()
                .map(|re| re.is_match(&st.resp.body))
                .unwrap_or(true)
    }

    fn writes_connection_header(&self, h: &Header) -> bool {
        for v in h.values("Connection") {
            for token in v.split(',') {
                let name = canonical_header_key(token.trim());
                if !name.is_empty() && self.actions.iter().any(|a| a.name == name) {
                    return true;
                }
            }
        }
        false
    }
}

fn match_headers(ms: &[HeaderMatcherC], h: &Header) -> bool {
    for m in ms {
        let values = h.values(&m.name);
        if values.is_empty() {
            return false;
        }
        if let Some(re) = &m.re {
            if !values.iter().any(|v| re.is_match(v.as_bytes())) {
                return false;
            }
        }
    }
    true
}

/// `expandReplace`: regexp-style bounded replacement. Returns src itself when
/// nothing matches, and never writes to src.
fn expand_replace(re: &BRegex, src: &[u8], template: &str) -> (Vec<u8>, bool) {
    let matches: Vec<_> = re.captures_iter(src).collect();
    if matches.is_empty() {
        return (src.to_vec(), true);
    }
    let mut dst: Vec<u8> = Vec::new();
    let mut last = 0usize;
    for caps in matches {
        let m = caps.get(0).unwrap();
        dst.extend_from_slice(&src[last..m.start()]);
        let mut rep: Vec<u8> = Vec::new();
        caps.expand(template.as_bytes(), &mut rep);
        dst.extend_from_slice(&rep);
        last = m.end();
        if dst.len() > MAX_EXPANSION {
            return (Vec::new(), false);
        }
    }
    dst.extend_from_slice(&src[last..]);
    if dst.len() > MAX_EXPANSION {
        return (Vec::new(), false);
    }
    (dst, true)
}
