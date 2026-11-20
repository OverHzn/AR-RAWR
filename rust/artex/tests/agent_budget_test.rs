//! Differential tests for `agent/budget.go` — ports of the Go budget tests
//! (effective budget, margin/dispatch, wrapup once, restore spend, write-back
//! tools allowed after exhaustion).

use artex::agent::budget::{
    budget_wrapup_blocks, effective_budget, wrapup_margin, Budget, BudgetLedger, Spend, UNLIMITED,
};
use artex::llm::Usage;

fn usage(tokens: i64) -> Usage {
    Usage {
        input_tokens: tokens,
        output_tokens: 0,
        ..Usage::default()
    }
}

#[test]
fn effective_budget_never_exceeds_parent() {
    let cases: &[(Budget, Budget, Budget)] = &[
        (
            Budget::new(50, 5),
            Budget::new(100, 10),
            Budget {
                tokens: 50,
                turns: 5,
            },
        ),
        (
            Budget::new(200, 20),
            Budget::new(100, 10),
            Budget {
                tokens: 100,
                turns: 10,
            },
        ),
        (
            Budget::new(0, 0),
            Budget::new(100, 10),
            Budget {
                tokens: 100,
                turns: 10,
            },
        ),
        (
            Budget::new(70, 7),
            Budget::new(0, 0),
            Budget {
                tokens: 70,
                turns: 7,
            },
        ),
        (
            Budget::new(0, 0),
            Budget::new(0, 0),
            Budget {
                tokens: UNLIMITED,
                turns: UNLIMITED,
            },
        ),
        (
            Budget::new(70, 7),
            Budget {
                tokens: 0,
                turns: 0,
            },
            Budget {
                tokens: 0,
                turns: 0,
            },
        ),
    ];
    for (own, parent, want) in cases {
        assert_eq!(effective_budget(*own, *parent), *want);
        assert!(want.tokens <= parent.tokens && want.turns <= parent.turns);
    }
}

#[test]
fn ledger_margin_dispatch_and_wrapup() {
    let l = BudgetLedger::new(Budget::new(100_000, 10), Budget::new(100_000, 5)).unwrap();
    assert_eq!(
        wrapup_margin(Budget::new(100_000, 10)),
        Budget {
            tokens: 20_000,
            turns: 2
        }
    );
    l.restore_spend(Spend {
        tokens: 70_000,
        turns: 1,
    });
    let mut intent = l.start_intent();
    assert_eq!(
        intent.remaining(),
        Budget {
            tokens: 10_000,
            turns: 5
        }
    );
    intent.record(&usage(10_000));
    assert!(intent.exhausted());
    assert!(intent.remaining().tokens == 0);
    intent.finish();

    // Wrapup is triggered once and stops dispatch.
    assert!(l.begin_wrapup());
    assert!(!l.begin_wrapup());
    assert!(!l.state().dispatch);
    assert_eq!(l.state().spent.tokens, 80_000);
}

#[test]
fn unset_budget_is_passthrough() {
    assert!(BudgetLedger::new(Budget::new(0, 0), Budget::new(0, 0)).is_none());
    let l = BudgetLedger::new(Budget::new(0, 0), Budget::new(10, 2)).unwrap();
    assert_eq!(l.state().remaining.tokens, UNLIMITED);
    assert!(l.state().dispatch);
    assert!(budget_wrapup_blocks("Bash"));
    assert!(!budget_wrapup_blocks("record_fact"));
    assert!(!budget_wrapup_blocks("insert_assets"));
}

#[test]
fn dispatch_stops_under_one_worker_estimate_and_line() {
    let l = BudgetLedger::new(Budget::new(100_000, 10), Budget::new(100_000, 5)).unwrap();
    // First worker finishes with a large average estimate.
    let mut w = l.start_intent();
    w.record(&usage(60_000));
    w.finish();
    let st = l.state();
    assert_eq!(st.estimate.tokens, 60_000);
    assert!(!st.dispatch, "40k left cannot afford a 60k worker estimate");
    let line = st.line();
    assert!(line.contains("[Task budget] remaining: tokens"));
    assert!(line.contains("wrap-up reserve: 20000 tokens"));

    // Spend past the reserve: dispatch stops.
    l.record(&usage(70_000));
    let st = l.state();
    assert!(!st.dispatch);
    assert!(st.at_margin);
}

#[test]
fn restore_spend_counts_toward_cap() {
    let l = BudgetLedger::new(Budget::new(100, 10), Budget::new(100, 5)).unwrap();
    l.restore_spend(Spend {
        tokens: 95,
        turns: 0,
    });
    let st = l.state();
    assert_eq!(st.remaining.tokens, 5);
    assert_eq!(st.spent.tokens, 95);
    let (task, _intent) = artex::agent::budget::budgets_from_env();
    assert_eq!(task.tokens, UNLIMITED, "unset budget env is unlimited");
}
