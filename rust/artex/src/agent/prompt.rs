//! Prompt rendering — pure slice of `agent/prompt.go`.
//!
//! Go's `text/template` with `missingkey=error` is used for DB-provided bodies;
//! this small renderer covers the constructs actually used: `{{.Field}}` and
//! `{{if .Field}}...{{else}}...{{end}}`. Unknown fields are an error so a broken
//! DB body falls back to the code-owned default.

use std::collections::HashMap;

/// Variables available to a prompt template.
#[derive(Debug, Clone, Default)]
pub struct Vars(pub HashMap<String, String>);

impl Vars {
    pub fn set(mut self, k: &str, v: impl Into<String>) -> Self {
        self.0.insert(k.to_string(), v.into());
        self
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }
}

/// Render a template with strict variable lookup.
pub fn render_template(tmpl: &str, vars: &Vars) -> Result<String, String> {
    render_range(tmpl, vars, 0, tmpl.len()).map(|(s, _)| s)
}

fn render_range(
    tmpl: &str,
    vars: &Vars,
    start: usize,
    end: usize,
) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut pos = start;
    while pos < end {
        let Some(rel) = tmpl[pos..end].find("{{") else {
            out.push_str(&tmpl[pos..end]);
            break;
        };
        let open = pos + rel;
        out.push_str(&tmpl[pos..open]);
        let Some(close_rel) = tmpl[open + 2..end].find("}}") else {
            return Err("unterminated template action".to_string());
        };
        let close = open + 2 + close_rel;
        let action = tmpl[open + 2..close].trim();
        if let Some(field) = action.strip_prefix('.') {
            let val = vars
                .get(field.trim())
                .ok_or_else(|| format!("missing key {}", field.trim()))?;
            out.push_str(val);
            pos = close + 2;
            continue;
        }
        if let Some(cond) = action.strip_prefix("if ") {
            let field = cond.trim().trim_start_matches('.');
            // Locate the matching {{else}}/{{end}} after the if body.
            let mut depth = 1usize;
            let mut scan = close + 2;
            let mut else_at = None;
            let mut end_at = None;
            while scan < end {
                let Some(a) = tmpl[scan..end].find("{{") else {
                    break;
                };
                let ao = scan + a;
                let Some(b) = tmpl[ao + 2..end].find("}}") else {
                    break;
                };
                let ac = ao + 2 + b;
                let act = tmpl[ao + 2..ac].trim();
                if act.starts_with("if ") {
                    depth += 1;
                } else if act == "end" {
                    depth -= 1;
                    if depth == 0 {
                        end_at = Some((ao, ac));
                        break;
                    }
                } else if act == "else" && depth == 1 {
                    else_at = Some((ao, ac));
                }
                scan = ac + 2;
            }
            let (eo, ec) = end_at.ok_or_else(|| "unterminated if".to_string())?;
            let truthy = vars.get(field).map(|v| !v.is_empty()).unwrap_or(false);
            if truthy {
                let stop = else_at.map(|(o, _)| o).unwrap_or(eo);
                let (rendered, _) = render_range(tmpl, vars, close + 2, stop)?;
                out.push_str(&rendered);
            } else if let Some((_, ec2)) = else_at {
                let (rendered, _) = render_range(tmpl, vars, ec2 + 2, eo)?;
                out.push_str(&rendered);
            }
            pos = ec + 2;
            continue;
        }
        return Err(format!("unsupported template action {action:?}"));
    }
    Ok((out, end))
}

/// Hook returning a DB-provided body for an agent key.
pub type PromptOverride<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `renderSystem`: override body if present, then render; any error falls back
/// to the code-owned default (and then to the literal default string).
pub fn render_system(
    agent_key: &str,
    def: &str,
    vars: &Vars,
    override_fn: Option<PromptOverride<'_>>,
) -> String {
    let tmpl = override_fn
        .and_then(|f| f(agent_key))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| def.to_string());
    if let Ok(out) = render_template(&tmpl, vars) {
        return out;
    }
    if let Ok(out) = render_template(def, vars) {
        return out;
    }
    def.to_string()
}
