//! Minimal HTTP router — foundation for the server API layer (`server/server.go`).
//! It only matches method + path patterns; no server is started here.

use std::collections::HashMap;

/// One registered route: `"GET /api/tasks/{id}"` style.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub method: String,
    pub pattern: String,
    pub handler: String,
}

/// A matched route with its path parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub route: Route,
    pub params: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct Router {
    routes: Vec<Route>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a route. Patterns must be `"METHOD /path"`.
    pub fn add(&mut self, method: &str, pattern: &str, handler: &str) {
        self.routes.push(Route {
            method: method.to_string(),
            pattern: pattern.to_string(),
            handler: handler.to_string(),
        });
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Match a request, preferring the most specific pattern (fewest parameters,
    /// then longest literal prefix) — the Go ServeMux rule.
    pub fn match_route(&self, method: &str, path: &str) -> Option<Match> {
        let mut best: Option<((i32, usize), usize, Match)> = None;
        for route in &self.routes {
            if route.method != method {
                continue;
            }
            let pat = route
                .pattern
                .strip_prefix(&format!("{method} "))
                .unwrap_or(&route.pattern);
            let Some(params) = match_segments(pat, path) else {
                continue;
            };
            let literals = pat
                .split('/')
                .filter(|s| !s.starts_with('{') && !s.is_empty())
                .count();
            let specificity = (literals as i32, pat.len());
            if best
                .as_ref()
                .map(|(s, _, _)| specificity > *s)
                .unwrap_or(true)
            {
                best = Some((
                    specificity,
                    pat.len(),
                    Match {
                        route: route.clone(),
                        params,
                    },
                ));
            }
        }
        best.map(|(_, _, m)| m)
    }

    /// Whether any route accepts the path with a different method (405 analogue).
    pub fn method_not_allowed(&self, method: &str, path: &str) -> bool {
        self.routes.iter().any(|r| {
            r.method != method && {
                let pat = r
                    .pattern
                    .strip_prefix(&format!("{} ", r.method))
                    .unwrap_or(&r.pattern);
                match_segments(pat, path).is_some()
            }
        })
    }
}

fn match_segments(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    if pattern == "/" {
        return (path == "/").then_some(HashMap::new());
    }
    let p: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let s: Vec<&str> = path.trim_matches('/').split('/').collect();
    if p.len() != s.len() {
        return None;
    }
    let mut params = HashMap::new();
    for (pat, seg) in p.iter().zip(s.iter()) {
        if seg.is_empty() {
            return None;
        }
        if let Some(name) = pat.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            params.insert(name.to_string(), (*seg).to_string());
        } else if pat != seg {
            return None;
        }
    }
    Some(params)
}
