//! Pure agent tool helpers — slice of `agent/tools.go` (stagnation detection).

use crate::db::exploration::Node;

const STAGNATION_MIN_REPEATS: i32 = 3;

/// `normalizeDirection`: collapse whitespace, lowercase, replace each run of
/// digits with a single '#' so URL/api version differences share a direction.
pub fn normalize_direction(summary: &str) -> String {
    let collapsed: String = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut digit = false;
    for c in collapsed.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !digit {
                out.push('#');
            }
            digit = true;
            continue;
        }
        digit = false;
        out.push(c);
    }
    out
}

/// One stagnating direction: the earliest summary, repeat count and node id.
#[derive(Debug, Clone, PartialEq)]
pub struct StagnationDirection {
    pub direction: String,
    pub repeats: i32,
    pub since: i64,
}

/// `detectStagnation`: finished intents whose normalized summary repeats at
/// least 3 times with no finding produced after the first repeat. At most 3.
pub fn detect_stagnation(done: &[Node], findings: &[Node]) -> Vec<StagnationDirection> {
    struct Dir {
        first_id: i64,
        repeats: i32,
        summary: String,
    }
    let mut dirs: std::collections::HashMap<String, Dir> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for n in done {
        let summary = n
            .payload
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if summary.trim().is_empty() {
            continue;
        }
        let fp = normalize_direction(summary);
        if fp.is_empty() {
            continue;
        }
        let d = dirs.entry(fp.clone()).or_insert_with(|| Dir {
            first_id: n.id,
            repeats: 0,
            summary: summary.to_string(),
        });
        if n.id < d.first_id {
            d.first_id = n.id;
            d.summary = summary.to_string();
        }
        d.repeats += 1;
        if !order.contains(&fp) {
            order.push(fp);
        }
    }

    let mut out = Vec::new();
    for fp in order {
        let d = &dirs[&fp];
        if d.repeats < STAGNATION_MIN_REPEATS {
            continue;
        }
        if findings.iter().any(|f| f.id > d.first_id) {
            continue;
        }
        out.push(StagnationDirection {
            direction: d.summary.clone(),
            repeats: d.repeats,
            since: d.first_id,
        });
        if out.len() >= 3 {
            break;
        }
    }
    out
}
