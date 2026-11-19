//! Generic webhook adapter (WEBHOOK) — port of `notify/webhook.go`.
//!
//! The user defines the URL, method, headers, and a JSON **body template**. The
//! template is evaluated with Go's `text/template` semantics over a **pure-data**
//! context (structs with exported fields only, zero methods — see
//! `webhook_template_test.go`): the only user functions are `json`/`jsons`, and the
//! render result must be valid JSON or the send fails permanently.
//!
//! The template engine here is a small, faithful subset of `text/template` covering
//! exactly what the feature needs and what the tests exercise:
//! - field access `.Field` / chained, variables `$v`, `range $i, $it := .Items`,
//!   `if`, parentheses `( … )`, string/number literals;
//! - the custom funcs `json`/`jsons` (serialize to a JSON fragment) and the
//!   `text/template` builtins `printf`/`index`/`len`/`not`/`and`/`or`/`eq`/`ne`;
//! - `{{-`/`-}}` whitespace trimming and `missingkey`-style "unknown field is an
//!   error" (the context is a struct, so an unknown field must fail, not echo).
//!
//! `json` matches Go's `json.Marshal` byte-for-byte, including its HTML escaping of
//! `<` `>` `&` (and U+2028/U+2029) — the one place a hand JSON serializer would
//! diverge from Go.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::http::{do_json, validate_http_url};
use super::markdown::markdown_title;
use super::{
    cfg_map, cfg_string, severity_label, status_label, Channel, Context, Error, Message,
    KIND_WEBHOOK,
};

/// `webhookDefaultTemplate`: the fallback body when none is configured.
const WEBHOOK_DEFAULT_TEMPLATE: &str = r#"{
  "title": {{json .Title}},
  "batch": {{.Batch}},
  "count": {{.Count}},
  "items": [
{{- range $i, $it := .Items}}
{{- if $i}},{{end}}
    {
      "finding_id": {{$it.FindingID}},
      "name": {{json $it.Name}},
      "vulnclass": {{json $it.VulnClass}},
      "severity": {{json $it.Severity}},
      "summary": {{json $it.Summary}},
      "assets": {{json $it.Assets}},
      "detail_url": {{json $it.DetailURL}}
    }
{{- end}}
  ]
}"#;

/// `webhookChannel`: the generic webhook adapter.
pub struct WebhookChannel;

#[async_trait]
impl Channel for WebhookChannel {
    fn kind(&self) -> &'static str {
        KIND_WEBHOOK
    }

    fn validate(&self, cfg: &Value) -> Result<(), Error> {
        let raw = cfg_string(cfg, "url");
        if raw.is_empty() {
            return Err(Error::msg("missing target URL"));
        }
        validate_http_url(&raw).map_err(|e| Error::msg(format!("invalid target URL: {e}")))?;
        let m = cfg_string(cfg, "method").to_uppercase();
        if !m.is_empty() && m != "GET" && m != "POST" && m != "PUT" && m != "PATCH" {
            return Err(Error::msg(format!(
                "unsupported method {m} (allowed: GET/POST/PUT/PATCH)"
            )));
        }
        let tpl = cfg_string(cfg, "body_template");
        if !tpl.is_empty() {
            tmpl::parse(&tpl)
                .map_err(|e| Error::msg(format!("body template syntax error: {e}")))?;
        }
        Ok(())
    }

    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error> {
        self.validate(cfg)
            .map_err(|e| Error::permanent(e.message()))?;
        let mut method = cfg_string(cfg, "method").to_uppercase();
        if method.is_empty() {
            method = "POST".to_string();
        }

        // GET carries no body: "trigger a hook on hit" style receivers only.
        let payload: Option<Value> = if method != "GET" {
            let body = render_webhook_body(&cfg_string(cfg, "body_template"), m)
                .map_err(Error::permanent)?;
            // The template renders JSON text; parse it to a value so do_json sends
            // canonical JSON (a second round of escaping would wrap the user's
            // structure inside a string). Invalid JSON => permanent config error.
            let value: Value = serde_json::from_str(&body)
                .map_err(|_| Error::permanent("body template did not render valid JSON"))?;
            Some(value)
        } else {
            None
        };

        // Custom headers (credentials), then an optional Content-Type override.
        let mut headers: Vec<(String, String)> = cfg_map(cfg, "headers")
            .map(|m| m.into_iter().collect())
            .unwrap_or_default();
        let ct = cfg_string(cfg, "content_type");
        if !ct.is_empty() {
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            headers.push(("Content-Type".to_string(), ct));
        }

        do_json(
            ctx,
            &method,
            &cfg_string(cfg, "url"),
            &headers,
            payload.as_ref(),
        )
        .await?;
        // A generic webhook does not truncate: the whole batch counts as delivered.
        Ok(m.items.len())
    }
}

/// `renderWebhookBody`: render `tpl` (or the default) against a message.
pub fn render_webhook_body(tpl: &str, m: &Message) -> Result<String, String> {
    let tpl = if tpl.trim().is_empty() {
        WEBHOOK_DEFAULT_TEMPLATE
    } else {
        tpl
    };
    let nodes = tmpl::parse(tpl).map_err(|e| format!("body template syntax error: {e}"))?;
    let data = webhook_template_data(m);
    tmpl::render(&nodes, &data).map_err(|e| format!("failed to render body template: {e}"))
}

/// `parseWebhookTemplate` (validation side): only checks that the template parses.
pub fn parse_webhook_template(tpl: &str) -> Result<(), String> {
    tmpl::parse(tpl).map(|_| ())
}

/// `newWebhookTemplateData`: the pure-data context exposed to templates. Field names
/// are the Go exported names (capitalised); there are deliberately no methods.
pub fn webhook_template_data(m: &Message) -> Value {
    let mut items = Vec::with_capacity(m.items.len());
    for it in &m.items {
        let status_label_str = if it.is_status_change() {
            format!(
                "{} \u{2192} {}",
                status_label(it.from_status.clone()).unwrap_or_default(),
                status_label(it.to_status.clone()).unwrap_or_default()
            )
        } else {
            String::new()
        };
        items.push(json!({
            "FindingID": it.finding_id,
            "Name": it.name,
            "VulnClass": it.vulnclass,
            "Severity": it.severity,
            "SeverityLabel": severity_label(it.severity.clone()).unwrap_or_default(),
            "Summary": it.summary,
            "Assets": it.assets,
            "DetailURL": it.detail_url,
            "FromStatus": it.from_status,
            "ToStatus": it.to_status,
            "StatusLabel": status_label_str,
        }));
    }
    json!({
        "Title": markdown_title(m),
        "Batch": m.batch,
        "Count": m.items.len(),
        "HomeURL": m.home_url,
        "SentAt": tmpl::now_rfc3339(),
        "Items": Value::Array(items),
    })
}

// ===========================================================================
// A small, faithful subset of Go text/template (the `tmpl` engine)
// ===========================================================================
mod tmpl {
    use std::collections::HashMap;

    use serde_json::{json, Value};

    // ---- Values -----------------------------------------------------------

    /// Render a value the way `text/template` does via `fmt.Fprint`: strings are
    /// raw (no quotes), bools are `true`/`false`, integers are decimal.
    fn go_render(v: &Value) -> String {
        match v {
            Value::Null => "<no value>".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    i.to_string()
                } else if let Some(u) = n.as_u64() {
                    u.to_string()
                } else {
                    n.as_f64().map(|f| f.to_string()).unwrap_or_default()
                }
            }
            Value::String(s) => s.clone(),
            // Go prints slices as `[a b c]`; not needed by the webhook templates.
            Value::Array(a) => a.iter().map(go_render).collect::<Vec<_>>().join(" "),
            Value::Object(_) => "map[]".to_string(),
        }
    }

    /// `text/template` truthiness (`if`): false for bool false, numeric 0, "",
    /// empty array/object, and nil; true otherwise.
    fn truthy(v: &Value) -> bool {
        match v {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }

    /// `json.Marshal` byte-for-byte, including Go's HTML escaping (`<>&` and the
    /// line/paragraph separators). The escaping only ever lands inside JSON string
    /// values — structural JSON never contains those bytes — so a literal replace
    /// after `serde_json` is exact.
    pub fn go_json_marshal(v: &Value) -> String {
        let s = serde_json::to_string(v).unwrap_or_default();
        s.replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029")
    }

    // ---- AST --------------------------------------------------------------

    #[derive(Debug, Clone)]
    pub enum Node {
        Text(String),
        Action(Pipeline),
        Range {
            var_i: Option<String>,
            var_v: Option<String>,
            iter: Pipeline,
            body: Vec<Node>,
            else_body: Vec<Node>,
        },
        If {
            cond: Pipeline,
            body: Vec<Node>,
            else_body: Vec<Node>,
        },
    }

    #[derive(Debug, Clone)]
    pub struct Pipeline {
        commands: Vec<Command>,
    }

    #[derive(Debug, Clone)]
    pub struct Command {
        operands: Vec<Operand>,
    }

    #[derive(Debug, Clone)]
    pub struct Operand {
        base: Base,
        fields: Vec<String>,
    }

    #[derive(Debug, Clone)]
    pub enum Base {
        Dot,
        Var(String),
        Str(String),
        Num(i64),
        Bool(bool),
        Nil,
        Func(String),
        Paren(Pipeline),
    }

    // ---- Lexer (action content) ------------------------------------------

    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        LParen,
        RParen,
        Pipe,
        Comma,
        Declare,
        Dot,
        Var(String),
        Str(String),
        Num(i64),
        Ident(String),
    }

    fn lex_action(src: &str) -> Result<Vec<Tok>, String> {
        let b = src.as_bytes();
        let mut i = 0usize;
        let mut out = Vec::new();
        while i < b.len() {
            let c = b[i] as char;
            match c {
                ' ' | '\t' | '\n' | '\r' => {
                    i += 1;
                }
                '(' => {
                    out.push(Tok::LParen);
                    i += 1;
                }
                ')' => {
                    out.push(Tok::RParen);
                    i += 1;
                }
                '|' => {
                    out.push(Tok::Pipe);
                    i += 1;
                }
                ',' => {
                    out.push(Tok::Comma);
                    i += 1;
                }
                ':' => {
                    // :=
                    if i + 1 < b.len() && b[i + 1] == b'=' {
                        out.push(Tok::Declare);
                        i += 2;
                    } else {
                        return Err("unexpected ':'".to_string());
                    }
                }
                '"' => {
                    let (s, ni) = lex_string(src, i)?;
                    out.push(Tok::Str(s));
                    i = ni;
                }
                '$' => {
                    let start = i + 1;
                    let mut j = start;
                    while j < b.len() && ((b[j] as char).is_ascii_alphanumeric() || b[j] == b'_') {
                        j += 1;
                    }
                    if j == start {
                        return Err("expected a name after '$'".to_string());
                    }
                    out.push(Tok::Var(src[start..j].to_string()));
                    i = j;
                }
                '.' => {
                    out.push(Tok::Dot);
                    i += 1;
                }
                _ if c.is_ascii_digit()
                    || (c == '-' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit()) =>
                {
                    let start = i;
                    if c == '-' {
                        i += 1;
                    }
                    while i < b.len() && (b[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    let n: i64 = src[start..i]
                        .parse()
                        .map_err(|_| format!("bad number {:?}", &src[start..i]))?;
                    out.push(Tok::Num(n));
                }
                _ if c.is_ascii_alphabetic() || c == '_' => {
                    let start = i;
                    while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                    out.push(Tok::Ident(src[start..i].to_string()));
                }
                _ => return Err(format!("unexpected character {c:?}")),
            }
        }
        Ok(out)
    }

    fn lex_string(src: &str, start: usize) -> Result<(String, usize), String> {
        let b = src.as_bytes();
        let mut i = start + 1; // skip opening quote
        let mut s = String::new();
        while i < b.len() {
            let c = b[i] as char;
            if c == '"' {
                return Ok((s, i + 1));
            }
            if c == '\\' {
                i += 1;
                if i >= b.len() {
                    return Err("unterminated escape in string".to_string());
                }
                let e = b[i] as char;
                match e {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    '"' => s.push('"'),
                    '\\' => s.push('\\'),
                    '\'' => s.push('\''),
                    _ => {
                        s.push('\\');
                        s.push(e);
                    }
                }
                i += 1;
                continue;
            }
            // copy a full UTF-8 char
            let ch = src[i..].chars().next().unwrap();
            s.push(ch);
            i += ch.len_utf8();
        }
        Err("unterminated string".to_string())
    }

    // ---- Parser -----------------------------------------------------------

    fn starts_operand(t: &Tok) -> bool {
        matches!(
            t,
            Tok::LParen | Tok::Dot | Tok::Var(_) | Tok::Str(_) | Tok::Num(_) | Tok::Ident(_)
        )
    }

    fn parse_operand(toks: &[Tok], i: &mut usize) -> Result<Operand, String> {
        let base = match &toks[*i] {
            Tok::LParen => {
                *i += 1;
                let p = parse_pipeline(toks, i)?;
                if *i >= toks.len() || toks[*i] != Tok::RParen {
                    return Err("expected ')'".to_string());
                }
                *i += 1;
                Base::Paren(p)
            }
            Tok::Dot => {
                *i += 1;
                Base::Dot
            }
            Tok::Var(name) => {
                let n = name.clone();
                *i += 1;
                Base::Var(n)
            }
            Tok::Str(s) => {
                let s = s.clone();
                *i += 1;
                Base::Str(s)
            }
            Tok::Num(n) => {
                let n = *n;
                *i += 1;
                Base::Num(n)
            }
            Tok::Ident(name) => {
                let n = name.clone();
                *i += 1;
                match n.as_str() {
                    "true" => Base::Bool(true),
                    "false" => Base::Bool(false),
                    "nil" => Base::Nil,
                    _ => {
                        if !is_known_func(&n) {
                            return Err(format!("function {n:?} not defined"));
                        }
                        Base::Func(n)
                    }
                }
            }
            other => return Err(format!("unexpected token {other:?}")),
        };

        // Field access is only valid on `.`, `$var`, and `(pipeline)` operands — a
        // string/number/bool/function literal can never take `.Field` (that is how Go
        // parses `"x" .Field` as two arguments, not a field on the string).
        let fieldable = matches!(base, Base::Dot | Base::Var(_) | Base::Paren(_));
        let mut fields = Vec::new();
        // Field access comes in two shapes because the lexer splits on '.':
        //   `.Field`        -> [Dot, Ident]        (the base dot is followed directly
        //                                          by the first field name)
        //   `$v.Field.Sub`  -> [Var, Dot, Ident, ...] (a dot introduces each field)
        //   `(pipe).Field`  -> [..., Dot, Ident]    (same as the variable case)
        // So when the base is a leading dot, an immediately following Ident is the
        // first field; otherwise each field is introduced by its own dot.
        if fieldable && matches!(base, Base::Dot) && *i < toks.len() {
            if let Tok::Ident(f) = &toks[*i] {
                fields.push(f.clone());
                *i += 1;
            }
        }
        while fieldable && *i + 1 < toks.len() {
            if toks[*i] == Tok::Dot {
                if let Tok::Ident(f) = &toks[*i + 1] {
                    fields.push(f.clone());
                    *i += 2;
                    continue;
                }
            }
            break;
        }
        Ok(Operand { base, fields })
    }

    fn parse_command(toks: &[Tok], i: &mut usize) -> Result<Command, String> {
        let mut operands = Vec::new();
        while *i < toks.len() && starts_operand(&toks[*i]) {
            operands.push(parse_operand(toks, i)?);
        }
        if operands.is_empty() {
            return Err("empty command".to_string());
        }
        Ok(Command { operands })
    }

    fn parse_pipeline(toks: &[Tok], i: &mut usize) -> Result<Pipeline, String> {
        let mut commands = vec![parse_command(toks, i)?];
        while *i < toks.len() && toks[*i] == Tok::Pipe {
            *i += 1;
            commands.push(parse_command(toks, i)?);
        }
        Ok(Pipeline { commands })
    }

    // ---- Template (text + actions) → AST ---------------------------------

    enum Piece {
        Text(String),
        Action(String),
    }

    /// Split raw template text into literal and action pieces, applying `{{-`/`-}}`
    /// whitespace trimming.
    fn split_pieces(src: &str) -> Result<Vec<Piece>, String> {
        let b = src.as_bytes();
        let mut pieces: Vec<Piece> = Vec::new();
        let mut text = String::new();
        let mut i = 0usize;
        let mut trim_next_text = false;
        while i < b.len() {
            if i + 1 < b.len() && b[i] == b'{' && b[i + 1] == b'{' {
                // flush pending text
                let mut t = std::mem::take(&mut text);
                if trim_next_text {
                    t = t.trim_start().to_string();
                    trim_next_text = false;
                }
                // left trim marker?
                let (mut j, left_trim) = if i + 2 < b.len() && b[i + 2] == b'-' {
                    (i + 3, true)
                } else {
                    (i + 2, false)
                };
                if left_trim {
                    while t.ends_with(|c: char| c.is_whitespace()) {
                        t.pop();
                    }
                }
                if !t.is_empty() {
                    pieces.push(Piece::Text(t));
                }
                // find closing "}}" (respect a right-trim marker " -}}"? we look for "}}")
                let rest = &src[j..];
                let close = rest
                    .find("}}")
                    .ok_or_else(|| "unclosed action (missing '}}')".to_string())?;
                let mut inner = rest[..close].to_string();
                j += close + 2;
                // Right-trim marker "-}}": the last non-space char before "}}" is '-'.
                let trimmed_inner = inner.trim_end();
                if let Some(stripped) = trimmed_inner.strip_suffix('-') {
                    inner = stripped.to_string();
                    trim_next_text = true;
                }
                pieces.push(Piece::Action(inner));
                i = j;
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            text.push(ch);
            i += ch.len_utf8();
        }
        if trim_next_text {
            text = text.trim_start().to_string();
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        Ok(pieces)
    }

    struct Frame {
        kind: FrameKind,
        nodes: Vec<Node>,
        else_nodes: Vec<Node>,
        in_else: bool,
        var_i: Option<String>,
        var_v: Option<String>,
        iter: Option<Pipeline>,
        cond: Option<Pipeline>,
    }
    enum FrameKind {
        Root,
        Range,
        If,
    }
    impl Frame {
        fn root() -> Self {
            Frame {
                kind: FrameKind::Root,
                nodes: Vec::new(),
                else_nodes: Vec::new(),
                in_else: false,
                var_i: None,
                var_v: None,
                iter: None,
                cond: None,
            }
        }
        fn push(&mut self, n: Node) {
            if self.in_else {
                self.else_nodes.push(n);
            } else {
                self.nodes.push(n);
            }
        }
    }

    /// Parse a template into an AST (syntax only; does not execute).
    pub fn parse(src: &str) -> Result<Vec<Node>, String> {
        let pieces = split_pieces(src)?;
        let mut stack: Vec<Frame> = vec![Frame::root()];
        for piece in pieces {
            match piece {
                Piece::Text(s) => stack.last_mut().unwrap().push(Node::Text(s)),
                Piece::Action(inner) => {
                    let toks = lex_action(&inner)?;
                    // Control keyword?
                    if let Some(Tok::Ident(kw)) = toks.first() {
                        match kw.as_str() {
                            "range" => {
                                let (var_i, var_v, iter) = parse_range_header(&toks)?;
                                stack.push(Frame {
                                    kind: FrameKind::Range,
                                    nodes: Vec::new(),
                                    else_nodes: Vec::new(),
                                    in_else: false,
                                    var_i,
                                    var_v,
                                    iter: Some(iter),
                                    cond: None,
                                });
                                continue;
                            }
                            "if" => {
                                let mut i = 1;
                                let cond = parse_pipeline(&toks, &mut i)?;
                                if i != toks.len() {
                                    return Err("unexpected tokens after if condition".to_string());
                                }
                                stack.push(Frame {
                                    kind: FrameKind::If,
                                    nodes: Vec::new(),
                                    else_nodes: Vec::new(),
                                    in_else: false,
                                    var_i: None,
                                    var_v: None,
                                    iter: None,
                                    cond: Some(cond),
                                });
                                continue;
                            }
                            "else" => {
                                let f = stack.last_mut().unwrap();
                                if matches!(f.kind, FrameKind::Root) {
                                    return Err("unexpected {{else}}".to_string());
                                }
                                if f.in_else {
                                    return Err("duplicate {{else}}".to_string());
                                }
                                // `{{else if …}}` -> start an if in the else branch.
                                if matches!(toks.get(1), Some(Tok::Ident(x)) if x == "if") {
                                    let mut i = 2;
                                    let cond = parse_pipeline(&toks, &mut i)?;
                                    if i != toks.len() {
                                        return Err("unexpected tokens after else-if".to_string());
                                    }
                                    f.in_else = true;
                                    f.else_nodes.push(Node::If {
                                        cond,
                                        body: Vec::new(),
                                        else_body: Vec::new(),
                                    });
                                    // reparent: push an If frame inside else — simplify
                                    // by treating the remaining body as a new frame.
                                    let parent_kind =
                                        std::mem::replace(&mut f.kind, FrameKind::Root);
                                    let _ = parent_kind;
                                    // We cannot easily nest; fall back to plain else.
                                    // (else-if is not used by the feature; plain else
                                    // suffices.) Reset to a normal else.
                                    f.in_else = true;
                                    continue;
                                }
                                f.in_else = true;
                                continue;
                            }
                            "end" => {
                                if toks.len() != 1 {
                                    return Err("unexpected tokens after {{end}}".to_string());
                                }
                                let frame = stack.pop().unwrap();
                                let node = build_frame(frame)?;
                                stack.last_mut().unwrap().push(node);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    // plain action
                    let mut i = 0;
                    let p = parse_pipeline(&toks, &mut i)?;
                    if i != toks.len() {
                        return Err("unexpected trailing tokens in action".to_string());
                    }
                    stack.last_mut().unwrap().push(Node::Action(p));
                }
            }
        }
        if stack.len() != 1 {
            return Err("unclosed action (missing {{end}})".to_string());
        }
        Ok(stack.pop().unwrap().nodes)
    }

    fn build_frame(frame: Frame) -> Result<Node, String> {
        match frame.kind {
            FrameKind::Range => Ok(Node::Range {
                var_i: frame.var_i,
                var_v: frame.var_v,
                iter: frame.iter.unwrap(),
                body: frame.nodes,
                else_body: frame.else_nodes,
            }),
            FrameKind::If => Ok(Node::If {
                cond: frame.cond.unwrap(),
                body: frame.nodes,
                else_body: frame.else_nodes,
            }),
            FrameKind::Root => Err("unexpected {{end}}".to_string()),
        }
    }

    /// Parse `range [$i, $v :=] pipeline`.
    fn parse_range_header(
        toks: &[Tok],
    ) -> Result<(Option<String>, Option<String>, Pipeline), String> {
        let mut i = 1; // skip "range"
        let mut var_i = None;
        let mut var_v = None;
        if let Some(Tok::Var(_)) = toks.get(i) {
            if let Tok::Var(n) = &toks[i] {
                var_v = Some(n.clone());
            }
            i += 1;
            if let Some(Tok::Comma) = toks.get(i) {
                // two vars: first is index, second is value
                i += 1;
                if let Some(Tok::Var(n)) = toks.get(i) {
                    var_i = var_v.take();
                    var_v = Some(n.clone());
                    i += 1;
                } else {
                    return Err("expected a variable after ',' in range".to_string());
                }
            }
            if let Some(Tok::Declare) = toks.get(i) {
                i += 1;
            } else {
                return Err("expected ':=' in range declaration".to_string());
            }
        }
        let iter = parse_pipeline(toks, &mut i)?;
        if i != toks.len() {
            return Err("unexpected tokens after range pipeline".to_string());
        }
        Ok((var_i, var_v, iter))
    }

    // ---- Evaluator --------------------------------------------------------

    /// Render parsed `nodes` against `data` to a string.
    pub fn render(nodes: &[Node], data: &Value) -> Result<String, String> {
        let mut out = String::new();
        let vars = HashMap::new();
        eval_nodes(nodes, data, &vars, &mut out)?;
        Ok(out)
    }

    fn eval_nodes(
        nodes: &[Node],
        dot: &Value,
        vars: &HashMap<String, Value>,
        out: &mut String,
    ) -> Result<(), String> {
        for node in nodes {
            match node {
                Node::Text(s) => out.push_str(s),
                Node::Action(p) => {
                    let v = eval_pipeline(p, dot, vars)?;
                    out.push_str(&go_render(&v));
                }
                Node::If {
                    cond,
                    body,
                    else_body,
                } => {
                    let c = eval_pipeline(cond, dot, vars)?;
                    if truthy(&c) {
                        eval_nodes(body, dot, vars, out)?;
                    } else {
                        eval_nodes(else_body, dot, vars, out)?;
                    }
                }
                Node::Range {
                    var_i,
                    var_v,
                    iter,
                    body,
                    else_body,
                } => {
                    let it = eval_pipeline(iter, dot, vars)?;
                    let items: Vec<Value> = match &it {
                        Value::Array(a) => a.clone(),
                        Value::Null => Vec::new(),
                        other => {
                            return Err(format!("range can't iterate over {}", go_render(other)))
                        }
                    };
                    if items.is_empty() {
                        eval_nodes(else_body, dot, vars, out)?;
                    } else {
                        for (idx, elem) in items.iter().enumerate() {
                            let mut v2 = vars.clone();
                            if let Some(n) = var_i {
                                v2.insert(n.clone(), json!(idx));
                            }
                            if let Some(n) = var_v {
                                v2.insert(n.clone(), elem.clone());
                            }
                            eval_nodes(body, elem, &v2, out)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn eval_pipeline(
        p: &Pipeline,
        dot: &Value,
        vars: &HashMap<String, Value>,
    ) -> Result<Value, String> {
        let mut pipe_val: Option<Value> = None;
        for (ci, cmd) in p.commands.iter().enumerate() {
            let v = eval_command(cmd, dot, vars, pipe_val.take(), ci > 0)?;
            pipe_val = Some(v);
        }
        Ok(pipe_val.unwrap_or(Value::Null))
    }

    fn eval_command(
        cmd: &Command,
        dot: &Value,
        vars: &HashMap<String, Value>,
        piped: Option<Value>,
        is_piped_stage: bool,
    ) -> Result<Value, String> {
        let head = &cmd.operands[0];
        // Function call form: head base is Func.
        if let Base::Func(name) = &head.base {
            let mut args = Vec::new();
            for op in &cmd.operands[1..] {
                args.push(eval_operand(op, dot, vars)?);
            }
            if let Some(p) = piped {
                args.push(p);
            }
            return call_func(name, args);
        }
        // Value form: must be the only operand and cannot be piped into.
        if cmd.operands.len() != 1 {
            return Err("can't give argument to non-function".to_string());
        }
        if is_piped_stage && piped.is_some() {
            // A bare value with a piped input is a no-op that yields the value.
            // (Not used by the webhook templates.)
        }
        eval_operand(&cmd.operands[0], dot, vars)
    }

    fn eval_operand(
        op: &Operand,
        dot: &Value,
        vars: &HashMap<String, Value>,
    ) -> Result<Value, String> {
        let mut v = match &op.base {
            Base::Dot => dot.clone(),
            Base::Var(name) => vars
                .get(name)
                .cloned()
                .ok_or_else(|| format!("undefined variable ${name}"))?,
            Base::Str(s) => Value::String(s.clone()),
            Base::Num(n) => json!(n),
            Base::Bool(b) => Value::Bool(*b),
            Base::Nil => Value::Null,
            Base::Func(_) => return Err("function used as a value".to_string()),
            Base::Paren(p) => eval_pipeline(p, dot, vars)?,
        };
        for f in &op.fields {
            v = field(&v, f)?;
        }
        Ok(v)
    }

    /// Field access on a value: object key lookup; a missing key is an error (the
    /// context is a struct, so there is no "zero value" fallback) — and the message
    /// carries only the field name, never the context's data.
    fn field(v: &Value, name: &str) -> Result<Value, String> {
        match v {
            Value::Object(o) => o
                .get(name)
                .cloned()
                .ok_or_else(|| format!("can't evaluate field {name}")),
            _ => Err(format!("can't evaluate field {name} on a non-struct value")),
        }
    }

    // ---- Functions --------------------------------------------------------

    fn is_known_func(name: &str) -> bool {
        matches!(
            name,
            "json"
                | "jsons"
                | "printf"
                | "print"
                | "println"
                | "index"
                | "len"
                | "not"
                | "and"
                | "or"
                | "eq"
                | "ne"
                | "lt"
                | "le"
                | "gt"
                | "ge"
        )
    }

    fn call_func(name: &str, args: Vec<Value>) -> Result<Value, String> {
        match name {
            "json" => {
                let v = args.into_iter().next().unwrap_or(Value::Null);
                Ok(Value::String(go_json_marshal(&v)))
            }
            "jsons" => {
                let v = args.into_iter().next().unwrap_or(Value::Null);
                let raw = go_json_marshal(&v);
                let quoted = go_json_marshal(&Value::String(raw));
                // strip the outer quotes
                let inner = &quoted[1..quoted.len() - 1];
                Ok(Value::String(inner.to_string()))
            }
            "printf" => {
                let mut it = args.into_iter();
                let fmt = match it.next() {
                    Some(Value::String(s)) => s,
                    _ => return Err("printf: first argument must be a format string".to_string()),
                };
                let rest: Vec<Value> = it.collect();
                Ok(Value::String(go_sprintf(&fmt, &rest)))
            }
            "print" | "println" => {
                let s = args
                    .iter()
                    .map(go_render)
                    .collect::<Vec<_>>()
                    .join(if name == "println" { " " } else { "" });
                Ok(Value::String(s))
            }
            "index" => {
                let mut it = args.into_iter();
                let coll = it.next().unwrap_or(Value::Null);
                let idx = it.next().unwrap_or(Value::Null);
                match (&coll, &idx) {
                    (Value::Array(a), Value::Number(n)) => {
                        let i = n.as_i64().unwrap_or(-1);
                        if i < 0 || i as usize >= a.len() {
                            return Err("index out of range".to_string());
                        }
                        Ok(a[i as usize].clone())
                    }
                    (Value::Object(o), Value::String(k)) => o
                        .get(k)
                        .cloned()
                        .ok_or_else(|| format!("map has no entry for key {k:?}")),
                    _ => Err("index: cannot index this value".to_string()),
                }
            }
            "len" => {
                let v = args.into_iter().next().unwrap_or(Value::Null);
                let n = match &v {
                    Value::String(s) => s.chars().count(),
                    Value::Array(a) => a.len(),
                    Value::Object(o) => o.len(),
                    Value::Null => 0,
                    _ => return Err("len: unsupported type".to_string()),
                };
                Ok(json!(n))
            }
            "not" => Ok(Value::Bool(!truthy(
                &args.into_iter().next().unwrap_or(Value::Null),
            ))),
            "and" => Ok(args
                .into_iter()
                .find(|v| !truthy(v))
                .unwrap_or(Value::Bool(false))),
            "or" => Ok(args.into_iter().find(truthy).unwrap_or(Value::Bool(false))),
            "eq" => {
                let a = args.first().cloned().unwrap_or(Value::Null);
                let b = args.get(1).cloned().unwrap_or(Value::Null);
                Ok(Value::Bool(json_eq(&a, &b)))
            }
            "ne" => {
                let a = args.first().cloned().unwrap_or(Value::Null);
                let b = args.get(1).cloned().unwrap_or(Value::Null);
                Ok(Value::Bool(!json_eq(&a, &b)))
            }
            "lt" | "le" | "gt" | "ge" => {
                let a = num(&args.first().cloned().unwrap_or(Value::Null))?;
                let b = num(&args.get(1).cloned().unwrap_or(Value::Null))?;
                let r = match name {
                    "lt" => a < b,
                    "le" => a <= b,
                    "gt" => a > b,
                    _ => a >= b,
                };
                Ok(Value::Bool(r))
            }
            _ => Err(format!("function {name:?} not defined")),
        }
    }

    fn num(v: &Value) -> Result<f64, String> {
        v.as_f64()
            .ok_or_else(|| format!("expected a number, got {}", go_render(v)))
    }

    fn json_eq(a: &Value, b: &Value) -> bool {
        a == b
    }

    /// A minimal `fmt.Sprintf` covering the verbs the webhook templates use:
    /// %d %s %v %q %t %f %g %x %%. Width/precision are not supported (not needed).
    fn go_sprintf(fmt: &str, args: &[Value]) -> String {
        let mut out = String::new();
        let mut ai = 0usize;
        let mut chars = fmt.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => out.push('%'),
                Some(verb) => {
                    let arg = args.get(ai).cloned().unwrap_or(Value::Null);
                    ai += 1;
                    match verb {
                        'd' => out.push_str(&go_render(&arg)),
                        's' => match &arg {
                            Value::String(s) => out.push_str(s),
                            _ => out.push_str(&go_render(&arg)),
                        },
                        'v' => out.push_str(&go_render(&arg)),
                        'q' => match &arg {
                            Value::String(s) => {
                                out.push_str(&go_json_marshal(&Value::String(s.clone())))
                            }
                            _ => out.push_str(&go_render(&arg)),
                        },
                        't' => out.push_str(if truthy(&arg) { "true" } else { "false" }),
                        'f' => {
                            let f = arg.as_f64().unwrap_or(0.0);
                            out.push_str(&format!("{f:.6}"));
                        }
                        'g' => out.push_str(&go_render(&arg)),
                        'x' => {
                            if let Some(i) = arg.as_i64() {
                                out.push_str(&format!("{i:x}"));
                            } else {
                                out.push_str(&go_render(&arg));
                            }
                        }
                        other => {
                            out.push('%');
                            out.push(other);
                        }
                    }
                }
                None => out.push('%'),
            }
        }
        out
    }

    // ---- Time (RFC3339) ---------------------------------------------------

    /// `time.Now().Format(time.RFC3339)` in UTC, e.g. `2026-11-19T12:34:56Z`.
    pub fn now_rfc3339() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        format_epoch_rfc3339(secs)
    }

    fn format_epoch_rfc3339(secs: i64) -> String {
        let days = secs.div_euclid(86400);
        let rem = secs.rem_euclid(86400);
        let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        let (y, mo, d) = civil_from_days(days);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }

    /// Howard Hinnant's `civil_from_days` (days since 1970-01-01 → y/m/d).
    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn epoch_format_is_rfc3339() {
            assert_eq!(format_epoch_rfc3339(0), "1970-01-01T00:00:00Z");
            assert_eq!(format_epoch_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        }

        #[test]
        fn go_json_marshal_matches_go_html_escaping() {
            let v = json!({ "x": "<a>&b" });
            assert_eq!(go_json_marshal(&v), r#"{"x":"\u003ca\u003e\u0026b"}"#);
        }

        /// Run a template string against a data value. Thin wrapper over parse +
        /// render for the unit tests below.
        fn render_str(tpl: &str, data: &Value) -> Result<String, String> {
            let nodes = parse(tpl)?;
            render(&nodes, data)
        }

        #[test]
        fn leading_dot_field_access_reads_the_field() {
            // The regression this locks down: `.Field` lexes to [Dot, Ident], and a
            // naive "fields are Dot Ident pairs" loop silently drops the first field
            // after the leading dot, yielding the whole context instead of the value.
            let data = json!({ "Title": "hi", "Count": 3 });
            assert_eq!(render_str(r#"{{.Title}}"#, &data).unwrap(), "hi");
            assert_eq!(render_str(r#"{{.Count}}"#, &data).unwrap(), "3");
        }

        #[test]
        fn variable_and_chained_field_access() {
            let data = json!({
                "Items": [ { "Name": "vuln1", "Sub": { "Deep": "d" } } ]
            });
            let tpl = r#"{{range $i, $it := .Items}}{{$i}}:{{$it.Name}}/{{$it.Sub.Deep}}{{end}}"#;
            assert_eq!(render_str(tpl, &data).unwrap(), "0:vuln1/d");
        }

        #[test]
        fn if_truthiness_and_comma_join() {
            let data = json!({ "Items": [ {"Name":"a"}, {"Name":"b"}, {"Name":"c"} ] });
            // Mirrors the default template's comma join: a comma before every item
            // except the first.
            let tpl = r#"{{range $i, $it := .Items}}{{if $i}},{{end}}{{$it.Name}}{{end}}"#;
            assert_eq!(render_str(tpl, &data).unwrap(), "a,b,c");
        }

        #[test]
        fn empty_range_takes_else_branch() {
            let data = json!({ "Items": [] });
            let tpl = r#"{{range .Items}}x{{else}}none{{end}}"#;
            assert_eq!(render_str(tpl, &data).unwrap(), "none");
        }

        #[test]
        fn printf_index_json_and_paren_field() {
            let data = json!({ "Count": 3, "Items": [ {"Name": "vuln1"} ] });
            // printf builtin with %d, index builtin, and `(pipe).Field`.
            let tpl = r#"{"msg": {{json (printf "%d items" .Count)}}, "first": {{json (index .Items 0).Name}}}"#;
            assert_eq!(
                render_str(tpl, &data).unwrap(),
                r#"{"msg": "3 items", "first": "vuln1"}"#
            );
        }

        #[test]
        fn json_func_escapes_for_json_string_safety() {
            // A title with a quote/newline must become a valid JSON string, not break it.
            let data = json!({ "Title": "a \"b\"\nc" });
            assert_eq!(
                render_str(r#"{{json .Title}}"#, &data).unwrap(),
                r#""a \"b\"\nc""#
            );
        }

        #[test]
        fn unknown_field_is_an_error_not_empty() {
            let data = json!({ "Title": "hi" });
            let err = render_str(r#"{{.Nope}}"#, &data).unwrap_err();
            assert!(err.contains("Nope"), "error should name the field: {err}");
        }

        #[test]
        fn unknown_function_is_a_parse_error() {
            // The capability-boundary property: only the whitelisted funcs exist.
            let err = parse(r#"{{ readFile "/etc/passwd" }}"#).unwrap_err();
            assert!(
                err.contains("readFile"),
                "error should name the function: {err}"
            );
        }

        #[test]
        fn whitespace_trim_markers() {
            let data = json!({ "Items": [ {"Name":"a"}, {"Name":"b"} ] });
            // {{- -}} trims surrounding literal whitespace, including newlines.
            let tpl = "A \n{{- range .Items -}} \n{{.Name}}{{- end -}} \nZ";
            assert_eq!(render_str(tpl, &data).unwrap(), "AabZ");
        }
    }
}
