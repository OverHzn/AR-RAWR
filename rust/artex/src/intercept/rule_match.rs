//! Intercept rule compilation + matching — pure slice of `intercept/intercept.go`.
//!
//! Rules are evaluated in priority order by the interceptor; this module only
//! decides whether one rule matches one tool call.

use regex::Regex;

use crate::db::intercept_store::InterceptRule;
use crate::intercept::argv::parse_argv_pattern;
use crate::intercept::argv_allow::check_argv_allow;

pub struct CompiledRule {
    pub rule: InterceptRule,
    re: Option<Regex>,
    argv: Vec<String>,
}

/// `compileRule`: a rule with an invalid regex or argv pattern never matches.
pub fn compile_rule(r: InterceptRule) -> Option<CompiledRule> {
    let (re, argv) = match r.match_type.as_str() {
        "regex" => (Some(Regex::new(&r.pattern).ok()?), Vec::new()),
        "argv" => (None, parse_argv_pattern(&r.pattern).ok()?),
        _ => (None, Vec::new()),
    };
    Some(CompiledRule { rule: r, re, argv })
}

/// A conservative shell tokenizer for simple argv rules (quotes stripped, no
/// metacharacters). Full Bash parsing (`ArgvPrefix`) remains deferred.
fn simple_argv(command: &str) -> Option<Vec<String>> {
    if command
        .chars()
        .any(|c| c.is_control() || ";|&<>$`()".contains(c))
    {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in command.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                _ => {
                    cur.push(c);
                    started = true;
                }
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        out.push(cur);
    }
    Some(out)
}

/// `ruleMatches`: whether one compiled rule covers a tool call.
pub fn rule_matches(r: &CompiledRule, tool_name: &str, input: &[u8]) -> bool {
    if r.rule.match_type == "argv" {
        // Allow rules for denylisted programs are neutralized; deny/ask still match.
        if r.rule.action == "allow" && check_argv_allow(&r.argv).is_err() {
            return false;
        }
        if r.rule.match_target != "tool_input" || tool_name != "Bash" {
            return false;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(input) else {
            return false;
        };
        let Some(command) = v.get("command").and_then(|x| x.as_str()) else {
            return false;
        };
        let Some(tokens) = simple_argv(command) else {
            return false;
        };
        return tokens.starts_with(&r.argv);
    }
    let subject = match r.rule.match_target.as_str() {
        "tool_name" => tool_name.to_string(),
        "tool_input" => String::from_utf8_lossy(input).into_owned(),
        _ => return false,
    };
    if r.rule.match_type == "regex" {
        return r
            .re
            .as_ref()
            .map(|re| re.is_match(&subject))
            .unwrap_or(false);
    }
    subject.contains(&r.rule.pattern)
}

/// The subset of an intercept decision needed by callers of `Match`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decision {
    pub rule_name: String,
    pub action: String,
    pub message: String,
    pub rule_id: i64,
    pub timeout_enabled: bool,
    pub timeout_seconds: i32,
    pub timeout_action: String,
}

/// `defaultMessage`: the fallback reason when a rule has no explicit message.
pub fn default_message(action: &str, name: &str) -> String {
    match action {
        "deny" => format!("intercept rule [{name}] forbids executing this tool"),
        "ask" => format!("intercept rule [{name}] requires user approval, please wait"),
        _ => String::new(),
    }
}

/// `Match`: first matching rule in priority order (callers pass rules already
/// sorted by priority). `ConfigDigest` is deferred with the full audit payload.
pub fn match_first(rules: &[CompiledRule], tool_name: &str, input: &[u8]) -> Option<Decision> {
    let mut ordered: Vec<&CompiledRule> = rules.iter().collect();
    ordered.sort_by_key(|r| std::cmp::Reverse(r.rule.priority));
    for r in ordered {
        if !r.rule.enabled || !rule_matches(r, tool_name, input) {
            continue;
        }
        let message = if r.rule.message.is_empty() {
            default_message(&r.rule.action, &r.rule.name)
        } else {
            r.rule.message.clone()
        };
        return Some(Decision {
            rule_name: r.rule.name.clone(),
            action: r.rule.action.clone(),
            message,
            rule_id: r.rule.id,
            timeout_enabled: r.rule.timeout_enabled,
            timeout_seconds: r.rule.timeout_seconds,
            timeout_action: r.rule.timeout_action.clone(),
        });
    }
    None
}
