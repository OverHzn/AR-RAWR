//! Asset DSL parser + SQL WHERE builder — port of `db/asset_dsl.go` (pure slice).
//!
//! Grammar: AND binds tighter than OR; parentheses group. Field expressions are
//! `field op value` (op in =, ==, !=, >, >=, <, <=); a bare word is a full-text
//! fuzzy match. Values are always parameterized.

use crate::db::error::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Expr {
    /// Empty = bare-text full-text search.
    pub field: String,
    pub op: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    And(Vec<AstNode>),
    Or(Vec<AstNode>),
    Leaf(Expr),
}

fn known_string_col(f: &str) -> Option<&'static str> {
    Some(match f {
        "domain" => "domain",
        "root_domain" => "root_domain",
        "ip" => "ip",
        "url" => "url",
        "page_title" | "title" => "page_title",
        "icp" => "icp",
        "service_name" => "service_name",
        "app_name" => "app_name",
        "bundle_id" => "bundle_id",
        "category" => "category",
        "app_icp" => "app_icp",
        "method" => "method",
        "service_type" => "service_type",
        "record_type" => "record_type",
        _ => return None,
    })
}

fn known_array_col(f: &str) -> Option<&'static str> {
    match f {
        "technology" | "technologies" | "tech" => Some("technologies"),
        _ => None,
    }
}

fn known_numeric_col(f: &str) -> Option<&'static str> {
    match f {
        "port" => Some("port"),
        "status_code" | "status" => Some("status_code"),
        _ => None,
    }
}

fn is_known_field(f: &str) -> bool {
    let f = f.to_lowercase();
    known_string_col(&f).is_some()
        || known_array_col(&f).is_some()
        || known_numeric_col(&f).is_some()
        || f == "company_id"
        || f == "task_id"
}

const FULL_TEXT_COLS: &[&str] = &[
    "domain",
    "root_domain",
    "ip",
    "url",
    "page_title",
    "icp",
    "service_name",
    "app_name",
    "app_description",
];

#[derive(Debug, Clone, PartialEq)]
struct Tok {
    kind: &'static str,
    expr: Option<Expr>,
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_lowercase() || c.is_ascii_uppercase()
}
fn is_ident_char(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// Read a quoted or unquoted token starting at `pos`.
fn read_token(s: &[u8], pos: usize) -> (String, usize) {
    if pos >= s.len() {
        return (String::new(), pos);
    }
    if s[pos] == b'"' {
        let mut i = pos + 1;
        while i < s.len() && s[i] != b'"' {
            i += 1;
        }
        let val = String::from_utf8_lossy(&s[pos + 1..i]).into_owned();
        if i < s.len() {
            i += 1;
        }
        return (val, i);
    }
    let mut i = pos;
    while i < s.len() && !(s[i] as char).is_ascii_whitespace() && s[i] != b'(' && s[i] != b')' {
        i += 1;
    }
    (String::from_utf8_lossy(&s[pos..i]).into_owned(), i)
}

/// Try to parse `field op value` at `pos`.
fn try_parse_field_expr(s: &[u8], pos: usize) -> Option<(Expr, usize)> {
    if pos >= s.len() || !is_ident_start(s[pos]) {
        return None;
    }
    let mut i = pos;
    while i < s.len() && is_ident_char(s[i]) {
        i += 1;
    }
    let field = String::from_utf8_lossy(&s[pos..i]).to_lowercase();
    if !is_known_field(&field) {
        return None;
    }
    if i >= s.len() {
        return None;
    }
    let op = if i + 1 < s.len() && matches!(s[i], b'=' | b'!' | b'>' | b'<') && s[i + 1] == b'=' {
        let op = String::from_utf8_lossy(&s[i..i + 2]).into_owned();
        i += 2;
        op
    } else if matches!(s[i], b'>' | b'<' | b'=') {
        let op = (s[i] as char).to_string();
        i += 1;
        op
    } else {
        return None;
    };
    let (value, end) = read_token(s, i);
    if end == i {
        return None;
    }
    Some((Expr { field, op, value }, end))
}

fn tokenize(s: &str) -> Vec<Tok> {
    let bytes = s.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'(' => {
                tokens.push(Tok {
                    kind: "LPAREN",
                    expr: None,
                });
                i += 1;
            }
            b')' => {
                tokens.push(Tok {
                    kind: "RPAREN",
                    expr: None,
                });
                i += 1;
            }
            _ => {
                if let Some((expr, end)) = try_parse_field_expr(bytes, i) {
                    tokens.push(Tok {
                        kind: "FIELD",
                        expr: Some(expr),
                    });
                    i = end;
                    continue;
                }
                let (word, end) = read_token(bytes, i);
                if word.is_empty() {
                    i += 1;
                    continue;
                }
                match word.to_uppercase().as_str() {
                    "AND" => tokens.push(Tok {
                        kind: "AND",
                        expr: None,
                    }),
                    "OR" => tokens.push(Tok {
                        kind: "OR",
                        expr: None,
                    }),
                    _ => tokens.push(Tok {
                        kind: "BARE",
                        expr: Some(Expr {
                            field: String::new(),
                            op: "=".to_string(),
                            value: word,
                        }),
                    }),
                }
                i = end;
            }
        }
    }
    tokens.push(Tok {
        kind: "EOF",
        expr: None,
    });
    tokens
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        self.tokens
            .get(self.pos)
            .unwrap_or(&self.tokens[self.tokens.len() - 1])
    }
    fn consume(&mut self) -> Tok {
        let t = self.peek().clone();
        self.pos += 1;
        t
    }
    fn parse_or(&mut self) -> Result<AstNode> {
        let left = self.parse_and()?;
        let mut children = vec![left];
        while self.peek().kind == "OR" {
            self.consume();
            children.push(self.parse_and()?);
        }
        if children.len() == 1 {
            Ok(children.remove(0))
        } else {
            Ok(AstNode::Or(children))
        }
    }
    fn parse_and(&mut self) -> Result<AstNode> {
        let left = self.parse_atom()?;
        let mut children = vec![left];
        while self.peek().kind == "AND" {
            self.consume();
            children.push(self.parse_atom()?);
        }
        if children.len() == 1 {
            Ok(children.remove(0))
        } else {
            Ok(AstNode::And(children))
        }
    }
    fn parse_atom(&mut self) -> Result<AstNode> {
        match self.peek().kind {
            "FIELD" | "BARE" => {
                let t = self.consume();
                Ok(AstNode::Leaf(t.expr.unwrap_or_default()))
            }
            "LPAREN" => {
                self.consume();
                let node = self.parse_or()?;
                if self.peek().kind != "RPAREN" {
                    return Err(Error::msg(
                        "DSL syntax error: missing closing parenthesis ')'",
                    ));
                }
                self.consume();
                Ok(node)
            }
            "EOF" => Err(Error::msg("DSL syntax error: incomplete expression")),
            other => Err(Error::msg(format!(
                "DSL syntax error: unexpected token '{other}'"
            ))),
        }
    }
}

/// `ParseDSL`: parse a DSL query string into an expression tree (None when empty).
pub fn parse_dsl(s: &str) -> Result<Option<AstNode>> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    let mut p = Parser {
        tokens: tokenize(s),
        pos: 0,
    };
    let node = p.parse_or()?;
    if p.peek().kind != "EOF" {
        return Err(Error::msg(format!(
            "DSL syntax error: unexpected content '{}'",
            p.peek().kind
        )));
    }
    Ok(Some(node))
}

/// One heterogeneous SQL parameter emitted by the DSL builder.
#[derive(Debug, Clone, PartialEq)]
pub enum DslParam {
    Text(String),
    Bigint(i64),
}

impl DslParam {
    pub fn as_tosql(&self) -> &(dyn tokio_postgres::types::ToSql + Sync) {
        match self {
            DslParam::Text(s) => s,
            DslParam::Bigint(n) => n,
        }
    }
}

struct WhereBuilder {
    args: Vec<DslParam>,
    base: usize,
}

impl WhereBuilder {
    fn next_text(&mut self, v: String) -> String {
        self.args.push(DslParam::Text(v));
        format!("${}", self.base + self.args.len())
    }
    fn next_int(&mut self, v: i64) -> String {
        self.args.push(DslParam::Bigint(v));
        format!("${}", self.base + self.args.len())
    }
    fn build(&mut self, node: &AstNode) -> Result<String> {
        match node {
            AstNode::And(children) | AstNode::Or(children) => {
                let joiner = if matches!(node, AstNode::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                let mut parts = Vec::with_capacity(children.len());
                for c in children {
                    parts.push(format!("({})", self.build(c)?));
                }
                Ok(parts.join(joiner))
            }
            AstNode::Leaf(e) => self.build_leaf(e),
        }
    }
    fn build_leaf(&mut self, e: &Expr) -> Result<String> {
        let f = e.field.to_lowercase();
        if f.is_empty() {
            let p = self.next_text(format!("%{}%", e.value));
            let mut parts: Vec<String> = FULL_TEXT_COLS
                .iter()
                .map(|c| format!("{c} ILIKE {p}"))
                .collect();
            parts.push(format!(
                "EXISTS (SELECT 1 FROM unnest(technologies) t(v) WHERE v ILIKE {p})"
            ));
            parts.push(format!(
                "EXISTS (SELECT 1 FROM unnest(bound_domains) t(v) WHERE v ILIKE {p})"
            ));
            return Ok(format!("({})", parts.join(" OR ")));
        }
        if f == "task_id" {
            let n = e.value.parse::<i64>().map_err(|_| {
                Error::msg(format!("task_id requires an integer value: {}", e.value))
            })?;
            return Ok(format!("{} = ANY(task_ids)", self.next_int(n)));
        }
        if f == "company_id" {
            let n = e.value.parse::<i64>().map_err(|_| {
                Error::msg(format!("company_id requires an integer value: {}", e.value))
            })?;
            return Ok(format!("company_id = {}", self.next_int(n)));
        }
        if let Some(col) = known_numeric_col(&f) {
            let n = e.value.parse::<i64>().map_err(|_| {
                Error::msg(format!("field {f} requires an integer value: {}", e.value))
            })?;
            let op = if e.op == "==" { "=" } else { &e.op };
            if !matches!(op, "=" | "!=" | ">" | ">=" | "<" | "<=") {
                return Err(Error::msg(format!(
                    "field {f} does not support operator {}",
                    e.op
                )));
            }
            return Ok(format!("{col} {op} {}", self.next_int(n)));
        }
        if let Some(col) = known_array_col(&f) {
            return match e.op.as_str() {
                "==" => Ok(format!("{} = ANY({col})", self.next_text(e.value.clone()))),
                "!=" => Ok(format!(
                    "NOT ({} = ANY({col}))",
                    self.next_text(e.value.clone())
                )),
                "=" => {
                    let p = self.next_text(format!("%{}%", e.value));
                    Ok(format!(
                        "EXISTS (SELECT 1 FROM unnest({col}) t(v) WHERE v ILIKE {p})"
                    ))
                }
                _ => Err(Error::msg(format!(
                    "array field {f} does not support operator {}",
                    e.op
                ))),
            };
        }
        if let Some(col) = known_string_col(&f) {
            return match e.op.as_str() {
                "=" => Ok(format!(
                    "{col} ILIKE {}",
                    self.next_text(format!("%{}%", e.value))
                )),
                "==" => Ok(format!("{col} = {}", self.next_text(e.value.clone()))),
                "!=" => Ok(format!(
                    "{col} NOT ILIKE {}",
                    self.next_text(format!("%{}%", e.value))
                )),
                _ => Err(Error::msg(format!(
                    "string field {f} does not support operator {}",
                    e.op
                ))),
            };
        }
        Err(Error::msg(format!("unknown field: {f}")))
    }
}

/// `buildDSLWhereBase`: compile to (clause, params), numbering placeholders from base+1.
pub fn build_dsl_where_base(
    node: Option<&AstNode>,
    base: usize,
) -> Result<(String, Vec<DslParam>)> {
    let Some(node) = node else {
        return Ok(("1=1".to_string(), Vec::new()));
    };
    let mut b = WhereBuilder {
        args: Vec::new(),
        base,
    };
    let clause = b.build(node)?;
    Ok((clause, b.args))
}

pub fn build_dsl_where(node: Option<&AstNode>) -> Result<(String, Vec<DslParam>)> {
    build_dsl_where_base(node, 0)
}

/// `ValidateDSL`: parse + compile without touching the database.
pub fn validate_dsl(dsl: &str) -> Result<()> {
    let node = parse_dsl(dsl)?;
    build_dsl_where(node.as_ref())?;
    Ok(())
}
