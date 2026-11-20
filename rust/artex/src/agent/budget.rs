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
