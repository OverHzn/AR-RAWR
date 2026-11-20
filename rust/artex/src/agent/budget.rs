//! Hierarchical run budgets — pure port of `agent/budget.go`.
//!
//! A child's effective budget is min(its own, parent's remaining above the
//! wrap-up reserve). One metered call = one turn; tokens = input + output.

use std::sync::Mutex;

use crate::llm::Usage;

pub const UNLIMITED: i64 = i64::MAX;
pub const BUDGET_MARGIN_PERCENT: i64 = 5;
pub const BUDGET_MARGIN_MIN_TOKENS: i64 = 20_000;
pub const BUDGET_MARGIN_MIN_TURNS: i64 = 2;
pub const DEFAULT_WORKER_ESTIMATE_TOKENS: i64 = 50_000;
pub const DEFAULT_WORKER_ESTIMATE_TURNS: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub tokens: i64,
    pub turns: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spend {
    pub tokens: i64,
    pub turns: i64,
}

fn budget_cap(n: i64) -> i64 {
    if n <= 0 {
        UNLIMITED
    } else {
        n
    }
}

impl Budget {
    pub fn new(tokens: i64, turns: i64) -> Budget {
        Budget {
            tokens: budget_cap(tokens),
            turns: budget_cap(turns),
        }
    }
    pub fn remaining(self, s: Spend) -> Budget {
        let left = |cap: i64, spent: i64| {
            if cap == UNLIMITED {
                UNLIMITED
            } else {
                (cap - spent).max(0)
            }
        };
        Budget {
            tokens: left(self.tokens, s.tokens),
            turns: left(self.turns, s.turns),
        }
    }
}

pub fn effective_budget(own: Budget, parent: Budget) -> Budget {
    Budget {
        tokens: own.tokens.min(parent.tokens),
        turns: own.turns.min(parent.turns),
    }
}

/// Reserve kept for the forced wrap-up (0 for unlimited dimensions).
pub fn wrapup_margin(task: Budget) -> Budget {
    let m = |capacity: i64, floor: i64| {
        if capacity == UNLIMITED {
            0
        } else {
            (capacity * BUDGET_MARGIN_PERCENT / 100).max(floor)
        }
    };
    Budget {
        tokens: m(task.tokens, BUDGET_MARGIN_MIN_TOKENS),
        turns: m(task.turns, BUDGET_MARGIN_MIN_TURNS),
    }
}

fn add_usage(s: Spend, u: &Usage) -> Spend {
    Spend {
        tokens: s.tokens + u.input_tokens + u.output_tokens,
        turns: s.turns + 1,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BudgetState {
    pub task: Budget,
    pub remaining: Budget,
    pub margin: Budget,
    pub spent: Spend,
    pub estimate: Spend,
    pub at_margin: bool,
    pub dispatch: bool,
}

/// Meters one task. `None` means no budget configured (passthrough).
pub struct BudgetLedger {
    task: Budget,
    intent: Budget,
    inner: Mutex<LedgerInner>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    spent: Spend,
    worker_runs: i64,
    worker_spent: Spend,
    wrapup: bool,
}

impl BudgetLedger {
    pub fn new(task: Budget, intent: Budget) -> Option<BudgetLedger> {
        if task == Budget::new(0, 0) && intent == Budget::new(0, 0) {
            return None;
        }
        Some(BudgetLedger {
            task,
            intent,
            inner: Mutex::new(LedgerInner::default()),
        })
    }

    pub fn restore_spend(&self, spent: Spend) {
        if spent.tokens == 0 && spent.turns == 0 {
            return;
        }
        let mut l = self.inner.lock().unwrap();
        l.spent.tokens += spent.tokens;
        l.spent.turns += spent.turns;
    }

    pub fn record(&self, u: &Usage) {
        let mut l = self.inner.lock().unwrap();
        l.spent = add_usage(l.spent, u);
    }

    pub fn state(&self) -> BudgetState {
        let l = self.inner.lock().unwrap();
        let rem = self.task.remaining(l.spent);
        let margin = wrapup_margin(self.task);
        let mut estimate = Spend {
            tokens: DEFAULT_WORKER_ESTIMATE_TOKENS,
            turns: DEFAULT_WORKER_ESTIMATE_TURNS,
        };
        if l.worker_runs > 0 {
            estimate = Spend {
                tokens: l.worker_spent.tokens / l.worker_runs,
                turns: l.worker_spent.turns / l.worker_runs,
            };
        }
        estimate = Spend {
            tokens: estimate.tokens.min(self.intent.tokens),
            turns: estimate.turns.min(self.intent.turns),
        };
        let at = (rem.tokens != UNLIMITED && rem.tokens <= margin.tokens)
            || (rem.turns != UNLIMITED && rem.turns <= margin.turns);
        BudgetState {
            task: self.task,
            remaining: rem,
            margin,
            spent: l.spent,
            estimate,
            at_margin: at,
            dispatch: !l.wrapup
                && !at
                && rem.tokens >= estimate.tokens
                && rem.turns >= estimate.turns,
        }
    }

    /// First caller wins: the forced wrap-up runs exactly once.
    pub fn begin_wrapup(&self) -> bool {
        let mut l = self.inner.lock().unwrap();
        let first = !l.wrapup;
        l.wrapup = true;
        first
    }

    pub fn start_intent(&self) -> IntentMeter<'_> {
        IntentMeter {
            ledger: self,
            spent: Spend::default(),
        }
    }
}

/// Meters one worker run against min(intent budget, task remaining above reserve).
pub struct IntentMeter<'a> {
    ledger: &'a BudgetLedger,
    spent: Spend,
}

impl IntentMeter<'_> {
    pub fn record(&mut self, u: &Usage) {
        let mut l = self.ledger.inner.lock().unwrap();
        self.spent = add_usage(self.spent, u);
        l.spent = add_usage(l.spent, u);
    }

    pub fn remaining(&self) -> Budget {
        let l = self.ledger.inner.lock().unwrap();
        let parent = if l.wrapup {
            Budget {
                tokens: 0,
                turns: 0,
            }
        } else {
            let rem = self.ledger.task.remaining(l.spent);
            let res = wrapup_margin(self.ledger.task);
            let left = |cap: i64, r: i64| {
                if cap == UNLIMITED {
                    UNLIMITED
                } else {
                    (cap - r).max(0)
                }
            };
            Budget {
                tokens: left(rem.tokens, res.tokens),
                turns: left(rem.turns, res.turns),
            }
        };
        effective_budget(self.ledger.intent.remaining(self.spent), parent)
    }

    pub fn exhausted(&self) -> bool {
        let r = self.remaining();
        r.tokens == 0 || r.turns == 0
    }

    pub fn finish(&self) {
        let mut l = self.ledger.inner.lock().unwrap();
        l.worker_runs += 1;
        l.worker_spent.tokens += self.spent.tokens;
        l.worker_spent.turns += self.spent.turns;
    }
}

/// A write-back tool is still allowed once the budget is exhausted.
pub fn budget_wrapup_blocks(tool: &str) -> bool {
    !matches!(
        tool,
        "record_fact"
            | "insert_assets"
            | "report_finding"
            | "update_finding_report"
            | "record_coverage"
    )
}

impl BudgetState {
    /// One-line remaining budget shown to the planner and logged.
    pub fn line(&self) -> String {
        let of = |rem: i64, cap: i64| {
            if cap == UNLIMITED {
                "unlimited".to_string()
            } else {
                format!("{rem} of {cap}")
            }
        };
        format!(
            "[Task budget] remaining: tokens {}, turns {} (wrap-up reserve: {} tokens, {} turns)",
            of(self.remaining.tokens, self.task.tokens),
            of(self.remaining.turns, self.task.turns),
            self.margin.tokens,
            self.margin.turns,
        )
    }
}

/// `BudgetsFromEnv`: ARTEX_TASK_BUDGET_{TOKENS,TURNS} and
/// ARTEX_INTENT_BUDGET_{TOKENS,TURNS}. Unset / 0 / invalid -> Unlimited.
pub fn budgets_from_env() -> (Budget, Budget) {
    let get = |k: &str| {
        std::env::var(k)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0)
    };
    (
        Budget::new(
            get("ARTEX_TASK_BUDGET_TOKENS"),
            get("ARTEX_TASK_BUDGET_TURNS"),
        ),
        Budget::new(
            get("ARTEX_INTENT_BUDGET_TOKENS"),
            get("ARTEX_INTENT_BUDGET_TURNS"),
        ),
    )
}

// Related-overview text budgeting + compact finding projection
// (agent/tools.go, pure helpers).

pub const RELATED_OVERVIEW_TOTAL_TEXT_RUNES: usize = 48_000;
pub const RELATED_OVERVIEW_MAX_TEXT_PER_SOURCE: usize = 8_000;
pub const MAX_TASK_SOURCE_COUNT: usize = 8;

/// Per-source budget: total / sources, capped at 8k, with at least 1 source.
pub fn related_overview_budget_for_sources(source_count: usize) -> usize {
    let n = source_count.clamp(1, MAX_TASK_SOURCE_COUNT);
    (RELATED_OVERVIEW_TOTAL_TEXT_RUNES / n).min(RELATED_OVERVIEW_MAX_TEXT_PER_SOURCE)
}

/// A bounded, rune-safe text budget shared across overview fields.
#[derive(Debug, Clone, Default)]
pub struct OverviewTextBudget {
    pub remaining: usize,
    pub truncated: bool,
}

impl OverviewTextBudget {
    /// `take` clips a value to `field_limit` runes (with an ellipsis) and to the
    /// budget remaining; never produces invalid UTF-8.
    pub fn take(&mut self, value: Option<&str>, field_limit: usize) -> String {
        let Some(value) = value else {
            return String::new();
        };
        let text = value.trim();
        if text.is_empty() {
            return String::new();
        }
        if self.remaining == 0 || field_limit == 0 {
            self.truncated = true;
            return String::new();
        }
        let mut runes: Vec<char> = text.chars().collect();
        let limit = field_limit.min(self.remaining);
        if runes.len() > limit {
            self.truncated = true;
            runes = if limit == 1 {
                vec!['…']
            } else {
                let mut v: Vec<char> = runes[..limit - 1].to_vec();
                v.push('…');
                v
            };
        }
        self.remaining -= runes.len();
        runes.into_iter().collect()
    }
}

/// `compactFinding`: the lean map the planner sees for a finding node.
pub fn compact_finding(
    node: &crate::db::exploration::Node,
) -> serde_json::Map<String, serde_json::Value> {
    let payload = node.payload.as_object();
    let get = |k: &str| {
        payload
            .and_then(|p| p.get(k))
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .cloned()
    };
    let mut m = serde_json::Map::new();
    m.insert("id".into(), serde_json::json!(node.id));
    m.insert("state".into(), serde_json::json!(node.state));
    m.insert(
        "summary".into(),
        get("summary").unwrap_or(serde_json::Value::Null),
    );
    if let Some(v) = get("vulnclass") {
        m.insert("vulnclass".into(), v);
    }
    if let Some(v) = get("severity") {
        m.insert("severity".into(), v);
    }
    if let Some(v) = get("status") {
        m.insert("status".into(), v);
    }
    m
}
