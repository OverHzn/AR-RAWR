//! Pure argv helpers — slice of `intercept/argv.go` (pattern + arity table).
//!
//! The full Bash parser (`ArgvPrefix`) is deferred; these are the deterministic
//! pieces used when loading and applying argv rules.

use crate::db::error::{Error, Result};

const ARGV_MAX_TOKENS: usize = 8;

/// Code-owned arity table: how many leading tokens form a command's human prefix.
const ARGV_ARITY: &[(&str, usize)] = &[
    ("cat", 1),
    ("cd", 1),
    ("ls", 1),
    ("rm", 1),
    ("grep", 1),
    ("head", 1),
    ("tail", 1),
    ("echo", 1),
    ("pwd", 1),
    ("find", 1),
    ("curl", 1),
    ("wget", 1),
    ("nmap", 1),
    ("git", 2),
    ("npm", 2),
    ("docker", 2),
    ("kubectl", 2),
    ("terraform", 2),
    ("npm run", 3),
    ("docker compose", 3),
    ("kubectl rollout", 3),
    ("git config", 3),
    ("gcloud", 3),
    ("gh", 3),
    ("aws", 3),
];

/// `argvPrefixLen`: longest matching entry wins; unknown programs take 1.
pub fn argv_prefix_len(tokens: &[String]) -> usize {
    for n in (1..=tokens.len()).rev() {
        let key = tokens[..n].join(" ");
        if let Some((_, arity)) = ARGV_ARITY.iter().find(|(name, _)| *name == key) {
            return (*arity).min(tokens.len());
        }
    }
    1.min(tokens.len())
}

/// `ArgvPrefixOf`: the program plus its subcommands.
pub fn argv_prefix_of(tokens: &[String]) -> Vec<String> {
    tokens[..argv_prefix_len(tokens)].to_vec()
}

/// `checkArgvToken`: 1..8 plain tokens, no flags, no shell metacharacters/spaces.
fn check_argv_token(tok: &str) -> Result<()> {
    if tok.is_empty() {
        return Err(Error::msg("a token is empty"));
    }
    if tok.starts_with('-') {
        return Err(Error::msg(format!(
            "token {tok:?} is a flag; a prefix holds the program and its subcommands only"
        )));
    }
    if tok
        .chars()
        .any(|c| ";;&|><`$()".contains(c) || c.is_whitespace())
    {
        return Err(Error::msg(format!(
            "token {tok:?} contains a shell metacharacter or a space"
        )));
    }
    Ok(())
}

/// `ParseArgvPattern`: a JSON array of 1..8 validated tokens.
pub fn parse_argv_pattern(pattern: &str) -> Result<Vec<String>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(pattern).map_err(|e| {
        Error::msg(format!(
            r#"an argv pattern is a JSON array of strings, for example ["git","push"]: {e}"#
        ))
    })?;
    if values.is_empty() || values.len() > ARGV_MAX_TOKENS {
        return Err(Error::msg(format!(
            "an argv pattern has 1 to {ARGV_MAX_TOKENS} tokens, got {}",
            values.len()
        )));
    }
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        let Some(tok) = v.as_str() else {
            return Err(Error::msg(
                r#"an argv pattern is a JSON array of strings, for example ["git","push"]"#,
            ));
        };
        check_argv_token(tok)?;
        out.push(tok.to_string());
    }
    Ok(out)
}
