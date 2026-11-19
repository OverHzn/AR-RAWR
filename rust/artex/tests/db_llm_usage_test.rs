//! Differential tests for the LLM usage ledger — port of the Go specs
//! `db/task_llm_spend_test.go`, `db/llm_usage_cost_test.go`, and
//! `db/llm_usage_ts_index_test.go`.
//!
//! Runs against `ARTEX_PG_DSN`, serialised behind one shared async lock, and skips
//! cleanly when the DSN is unset or unreachable. Rows are namespaced by a
//! process-unique suffix so the shared dev DB stays usable and the test cleans up
//! after itself.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::llm_usage::{apply_cost_eur, parse_exp_id, LLMUsage, ProfileUsage};
use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos + (N.fetch_add(1, Ordering::Relaxed) as u128) * 7919
}

async fn open(dsn: &str) -> Option<DB> {
    match DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

/// `costProfiles`: a priced profile (EUR/1M: input 2, output 4, cache read 0.2,
/// cache write 2.5) and an unpriced one, seeded via raw SQL (price_* are NUMERIC,
/// bound as inline literals). Returns `(priced, unpriced)` names.
async fn cost_profiles(d: &DB) -> (String, String) {
    let stamp = uniq();
    let priced = format!("t-cost-priced-{stamp}");
    let unpriced = format!("t-cost-unpriced-{stamp}");
    d.execute(
        "INSERT INTO llm_profiles(name, format, model, price_input_per_m, price_output_per_m, \
         price_cache_read_per_m, price_cache_write_per_m) \
         VALUES ($1,'anthropic','m',2.00,4.00,0.20,2.50)",
        &[&priced],
    )
    .await
    .expect("insert priced profile");
    d.execute(
        "INSERT INTO llm_profiles(name, format, model) VALUES ($1,'anthropic','m')",
        &[&unpriced],
    )
    .await
    .expect("insert unpriced profile");
    (priced, unpriced)
}

fn approx(got: Option<f64>, want: f64) -> bool {
    matches!(got, Some(g) if (g - want).abs() <= 1e-9)
}

/// `TestCostSeparatesFreshReadAndWriteBuckets`: input tokens already include the
/// cache read/write subsets, so only the remainder is billed at the input price.
#[tokio::test]
async fn cost_separates_fresh_read_and_write_buckets() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let (priced, unpriced) = cost_profiles(&d).await;

    // (name, in, read, write, want). Every case adds 100k output: 0.1 × 4.00 = 0.40.
    type Case<'a> = (&'a str, i64, i64, i64, f64);
    let cases: [Case; 5] = [
        ("fresh", 1_000_000, 0, 0, 2.40),
        ("read_only", 1_000_000, 1_000_000, 0, 0.60),
        ("write_only", 1_000_000, 0, 1_000_000, 2.90),
        ("mixed", 1_000_000, 200_000, 300_000, 2.19),
        ("inconsistent_gateway", 100_000, 200_000, 300_000, 1.19),
    ];
    for (name, input, read, write, want) in cases {
        let mut out = vec![
            ProfileUsage {
                profile_name: priced.clone(),
                input_tokens: input,
                cache_read_tokens: read,
                cache_write_tokens: write,
                output_tokens: 100_000,
                ..Default::default()
            },
            ProfileUsage {
                profile_name: unpriced.clone(),
                input_tokens: input,
                cache_read_tokens: read,
                cache_write_tokens: write,
                output_tokens: 100_000,
                ..Default::default()
            },
            ProfileUsage {
                profile_name: "t-cost-unknown-profile".into(),
                input_tokens: input,
                output_tokens: 100_000,
                ..Default::default()
            },
        ];
        apply_cost_eur(&d, &mut out).await.expect("apply_cost_eur");
        assert!(
            approx(out[0].cost_eur, want),
            "{name}: cost={:?} want {want}",
            out[0].cost_eur
        );
        assert!(out[1].cost_eur.is_none(), "{name}: unpriced got a cost");
        assert!(
            out[2].cost_eur.is_none(),
            "{name}: unknown profile got a cost"
        );
    }

    d.execute(
        "DELETE FROM llm_profiles WHERE name IN ($1,$2)",
        &[&priced, &unpriced],
    )
    .await
    .expect("cleanup profiles");
}

/// `TestUsageByProfileCostAggregatesRows`: several ledger rows of one profile are
/// summed before pricing.
#[tokio::test]
async fn usage_by_profile_cost_aggregates_rows() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let (priced, unpriced) = cost_profiles(&d).await;

    let rows = [
        // fresh 400k×2 + 200k×0.2 + 40k×4 = 0.80 + 0.04 + 0.16 = 1.00
        LLMUsage {
            input_tokens: 600_000,
            cache_read: 200_000,
            output_tokens: 40_000,
            model: "m".into(),
            status: "ok".into(),
            ..Default::default()
        },
        // fresh 100k×2 + 300k×2.5 + 60k×4 = 0.20 + 0.75 + 0.24 = 1.19
        LLMUsage {
            input_tokens: 400_000,
            cache_write: 300_000,
            output_tokens: 60_000,
            model: "m".into(),
            status: "ok".into(),
            ..Default::default()
        },
    ];
    for profile in [&priced, &unpriced] {
        for r in &rows {
            let mut r = r.clone();
            r.profile_name = profile.clone();
            d.insert_llm_usage(&r).await.expect("insert llm_usage");
        }
    }

    let usage = d.usage_by_profile().await.expect("usage_by_profile");
    let mut found = 0;
    for p in &usage {
        match p.profile_name.as_str() {
            n if n == priced => {
                found += 1;
                assert_eq!(p.calls, 2, "calls");
                assert_eq!(p.input_tokens, 1_000_000, "input");
                assert_eq!(p.cache_read_tokens, 200_000, "cache_read");
                assert_eq!(p.cache_write_tokens, 300_000, "cache_write");
                assert_eq!(p.output_tokens, 100_000, "output");
                // = 1.00 + 1.19.
                assert!(approx(p.cost_eur, 2.19), "priced cost={:?}", p.cost_eur);
            }
            n if n == unpriced => {
                found += 1;
                assert_eq!(p.calls, 2, "unpriced calls");
                assert!(p.cost_eur.is_none(), "unpriced got a cost");
            }
            _ => {}
        }
    }
    assert_eq!(found, 2, "both profiles present in usage_by_profile");

    d.execute(
        "DELETE FROM llm_usage WHERE profile_name IN ($1,$2)",
        &[&priced, &unpriced],
    )
    .await
    .expect("cleanup usage");
    d.execute(
        "DELETE FROM llm_profiles WHERE name IN ($1,$2)",
        &[&priced, &unpriced],
    )
    .await
    .expect("cleanup profiles");
}

/// `TestTaskLLMSpendCountsJudgeNotChat`: judge calls count toward the task cap;
/// chat rows for the same task do not.
#[tokio::test]
async fn task_llm_spend_counts_judge_not_chat() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let task_id = format!("spend-{}", uniq());

    async fn insert(d: &DB, task_id: &str, worker: &str, input: i32, output: i32) {
        d.insert_llm_usage(&LLMUsage {
            task_id: task_id.to_string(),
            worker: worker.to_string(),
            input_tokens: input,
            output_tokens: output,
            status: "ok".into(),
            ..Default::default()
        })
        .await
        .expect("insert");
    }
    insert(&d, &task_id, "judge", 30, 10).await;
    insert(&d, &task_id, "chat", 1000, 1000).await;

    let (tokens, turns) = d.task_llm_spend(&task_id).await.expect("task_llm_spend");
    assert_eq!((tokens, turns), (40, 1), "judge row only");

    d.execute("DELETE FROM llm_usage WHERE task_id=$1", &[&task_id])
        .await
        .expect("cleanup");
}

/// `TestLLMUsageTsIndex`: migration 0004 leaves a valid btree index on
/// `llm_usage(ts)` for the time-window queries.
#[tokio::test]
async fn llm_usage_ts_index_exists() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let row = d
        .query_opt(
            "SELECT i.indisvalid, i.indisready, pg_get_indexdef(i.indexrelid) \
             FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname='public' AND c.relname='idx_llm_usage_ts'",
            &[],
        )
        .await
        .expect("query index");
    let row = row.expect("idx_llm_usage_ts must exist after Open");
    let valid: bool = row.get(0);
    let ready: bool = row.get(1);
    let def: String = row.get(2);
    assert!(
        valid && ready,
        "indisvalid={valid} indisready={ready} ({def})"
    );
    assert!(
        def.contains("USING btree (ts)"),
        "index def {def:?} must be btree on ts"
    );
}

/// `TokenByModel` groups by model (unknown → "(unknown)"), most-used first.
#[tokio::test]
async fn token_by_model_groups_by_model() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let task_id = format!("tok-{}", uniq());
    let mk = |model: &str, input: i32, output: i32| LLMUsage {
        task_id: task_id.clone(),
        model: model.into(),
        worker: "worker".into(),
        status: "ok".into(),
        input_tokens: input,
        output_tokens: output,
        ..Default::default()
    };
    d.insert_llm_usage(&mk("alpha", 100, 50)).await.expect("i");
    d.insert_llm_usage(&mk("alpha", 100, 50)).await.expect("i");
    d.insert_llm_usage(&mk("beta", 10, 5)).await.expect("i");
    // An empty-model row coalesces to "(unknown)".
    d.insert_llm_usage(&mk("", 1, 1)).await.expect("i");

    let stats = d.token_by_model(&task_id).await.expect("token_by_model");
    let alpha = stats.iter().find(|s| s.model == "alpha").expect("alpha");
    assert_eq!(alpha.calls, 2);
    assert_eq!(alpha.input_tokens, 200);
    assert_eq!(alpha.output_tokens, 100);
    let unknown = stats
        .iter()
        .find(|s| s.model == "(unknown)")
        .expect("(unknown)");
    assert_eq!(unknown.calls, 1);
    // Most-used (by input+output) first: alpha (300) before beta (15).
    assert_eq!(
        stats[0].model,
        "alpha",
        "order: {:?}",
        stats.iter().map(|s| &s.model).collect::<Vec<_>>()
    );

    d.execute("DELETE FROM llm_usage WHERE task_id=$1", &[&task_id])
        .await
        .expect("cleanup");
}

/// `ParseExpID`: parse the exploration-id segment (0 for empty/negative/non-numeric).
#[test]
fn parse_exp_id_cases() {
    assert_eq!(parse_exp_id(""), 0);
    assert_eq!(parse_exp_id("0"), 0);
    assert_eq!(parse_exp_id("42"), 42);
    assert_eq!(parse_exp_id("-1"), 0);
    assert_eq!(parse_exp_id("conv-1"), 0);
    assert_eq!(parse_exp_id("999999"), 999_999);
}
