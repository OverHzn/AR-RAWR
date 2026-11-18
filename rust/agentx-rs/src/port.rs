//! Rust port of the unexported, pure helpers in the Go `agent/` package.
//!
//! The rest of `agent/`'s unexported surface needs heavy ports (the LLM message
//! model, `db` graph/node types, `actool.CoreTool`, net/url, regexp) or touches I/O
//! and stays in Go. Go-stdlib string semantics come from `gocompat`.

use gocompat::gojson::go_quote_string;
use gocompat::strings::{go_fields_join, go_to_lower, go_trim_space};

/// `Unlimited` (`agent/budget.go`): `math.MaxInt64` — the cap of a budget dimension
/// with no limit.
const UNLIMITED: i64 = i64::MAX;

/// `relatedOverviewBudgetForSources` constants (`agent/tools.go`).
const RELATED_OVERVIEW_TOTAL_TEXT_RUNES: i64 = 48_000;
const RELATED_OVERVIEW_MAX_TEXT_PER_SOURCE: i64 = 8_000;
/// `db.MaxTaskSourceCount` (`db/tasks.go`).
const MAX_TASK_SOURCE_COUNT: i64 = 8;

/// `workerChatMarkerPrefix` (`agent/worker.go`).
const WORKER_CHAT_MARKER_PREFIX: &str = "<!-- ARTEX_WORKER_CHAT:";

/// `factSources` + `factSourceInference` (`agent/tools.go`), in declaration order.
const FACT_SOURCE_INFERENCE: &str = "inference";
const FACT_SOURCES: [&str; 5] = [
    "target_response",
    "tool_output",
    "third_party_intel",
    "operator_input",
    "inference",
];

/// `budgetCap` (`agent/budget.go`): a non-positive cap means "no limit" (`Unlimited`).
pub fn budget_cap(n: i64) -> i64 {
    if n <= 0 { UNLIMITED } else { n }
}

/// `left` (`agent/budget.go`): the budget left after `spent` (never negative);
/// `Unlimited` stays `Unlimited`. Matches Go's `max(capacity-spent, 0)`.
pub fn left(capacity: i64, spent: i64) -> i64 {
    if capacity == UNLIMITED {
        UNLIMITED
    } else {
        capacity.wrapping_sub(spent).max(0)
    }
}

/// `relatedOverviewBudgetForSources` (`agent/tools.go`): per-source text budget for a
/// "related overview" — the total rune budget split across the (capped) source count,
/// then capped per source. Non-positive count → 0.
pub fn related_overview_budget_for_sources(source_count: i64) -> i64 {
    if source_count <= 0 {
        return 0;
    }
    let mut source_count = source_count;
    if source_count > MAX_TASK_SOURCE_COUNT {
        source_count = MAX_TASK_SOURCE_COUNT;
    }
    let mut per_source = RELATED_OVERVIEW_TOTAL_TEXT_RUNES / source_count;
    if per_source > RELATED_OVERVIEW_MAX_TEXT_PER_SOURCE {
        per_source = RELATED_OVERVIEW_MAX_TEXT_PER_SOURCE;
    }
    per_source
}

/// `foldableKind` (`agent/coldgraph.go`): whether a node kind is `intent` or `fact`
/// (`db.KindIntent` / `db.KindFact`).
pub fn foldable_kind(k: &str) -> bool {
    k == "intent" || k == "fact"
}

/// `isFalsy` (`agent/provider.go`): a trimmed, lower-cased value in
/// {`0`, `false`, `off`, `no`}.
pub fn is_falsy(s: &str) -> bool {
    matches!(
        go_to_lower(go_trim_space(s)).as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// `firstLine` (`agent/capture.go`): trim, keep only the text before the first
/// newline, and — if it exceeds `max` runes — cut to `max` runes and append `…`.
pub fn first_line(s: &str, max: usize) -> String {
    let trimmed = go_trim_space(s);
    let head = match trimmed.split_once('\n') {
        Some((before, _)) => before,
        None => trimmed,
    };
    if head.chars().count() > max {
        let mut out: String = head.chars().take(max).collect();
        out.push('\u{2026}');
        out
    } else {
        head.to_string()
    }
}

/// `truncOutput` (`agent/planner.go`): if `s` exceeds `n` runes, cut to `n` runes and
/// append ` …(truncated; full text via get_worker_output)`.
pub fn trunc_output(s: &str, n: usize) -> String {
    let runes: Vec<char> = s.chars().collect();
    if runes.len() <= n {
        return s.to_string();
    }
    let head: String = runes[..n].iter().collect();
    format!("{head} \u{2026}(truncated; full text via get_worker_output)")
}

/// `workerChatMarker` (`agent/worker.go`): `workerChatMarkerPrefix + requestID + " -->"`.
pub fn worker_chat_marker(request_id: &str) -> String {
    format!("{WORKER_CHAT_MARKER_PREFIX}{request_id} -->")
}

/// `normalizeDirection` (`agent/tools.go`): lower-case, collapse whitespace runs to a
/// single space, then replace each maximal digit run with a single `#`.
pub fn normalize_direction(summary: &str) -> String {
    let lowered = go_to_lower(&go_fields_join(summary));
    let mut b = String::new();
    let mut digit = false;
    for r in lowered.chars() {
        if r.is_ascii_digit() {
            if !digit {
                b.push('#');
            }
            digit = true;
            continue;
        }
        digit = false;
        b.push(r);
    }
    b
}

/// `hintAuthor` (`agent/planner.go`): a human-readable phrase for a hint's origin.
pub fn hint_author(origin: &str) -> String {
    match origin {
        "human" => "The human (via the main agent)".to_string(),
        "orchestrator" => "A cross-task orchestration".to_string(),
        "" => "A source that was not recorded".to_string(),
        other => format!("An agent ({other})"),
    }
}

/// `normalizeFactSource` (`agent/tools.go`): normalize a fact's declared source. Empty
/// → `inference` (the default); a known source (case/space-insensitive) → itself;
/// anything else → `Err("source %q is not valid: use one of … (omit it for inference)")`
/// (Go `%q` of the original input).
pub fn normalize_fact_source(s: &str) -> Result<String, String> {
    let norm = go_to_lower(go_trim_space(s));
    if norm.is_empty() {
        return Ok(FACT_SOURCE_INFERENCE.to_string());
    }
    for v in FACT_SOURCES {
        if norm == v {
            return Ok(v.to_string());
        }
    }
    Err(format!(
        "source {} is not valid: use one of {} (omit it for inference)",
        go_quote_string(s),
        FACT_SOURCES.join(", ")
    ))
}

/// `splitLines` (`agent/tools_insert.go`): split on `\n`, trim each line, drop blanks.
/// Returns `None` (JSON `null`) when nothing remains, mirroring Go's nil slice.
pub fn split_lines(s: &str) -> Option<Vec<String>> {
    let out: Vec<String> = s
        .split('\n')
        .map(go_trim_space)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect();
    if out.is_empty() { None } else { Some(out) }
}
