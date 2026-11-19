//! LLM usage ledger — port of `db/llm_usage.go`.
//!
//! One lightweight row per completion call (the always-on usage ledger, distinct
//! from the gated full-body `llm_records`). Written on both success and error so
//! token accounting is complete even for interrupted/failed runs. Carries only
//! the dimensions needed to slice token spend, never prompt/response content.
//!
//! The global aggregators ([`DB::usage_by_profile`], [`DB::usage_daily`]) merge
//! live rows with archived-task summaries via
//! [`archived_task_aggregates`](crate::db::task_archive_aggregate::archived_task_aggregates).
//! `ModelTokenStat` is declared here because its only producer is
//! [`DB::token_by_model`] (Go happens to declare it in `commands.go`, but no
//! `commands.go` method returns it — so its semantic home is this module).

use crate::db::error::Result;
use crate::db::gotime;
use crate::db::task_archive_aggregate::archived_task_aggregates;
use crate::db::DB;

/// `LLMUsage` — one completion-call metering row.
#[derive(Debug, Clone, Default)]
pub struct LLMUsage {
    /// Task registry id (matches `llm_records.task_id`).
    pub task_id: String,
    /// Exploration id parsed from the session (0 = unknown/non-task).
    pub exploration_id: i64,
    /// Agent lane: worker / planner / mainagent / goals / judge / compactor.
    pub worker: String,
    pub model: String,
    pub profile_name: String,
    pub latency_ms: i32,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read: i32,
    pub cache_write: i32,
    /// ok | error.
    pub status: String,
}

/// `ModelTokenStat` — one model's aggregated token usage for a task.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ModelTokenStat {
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

/// `ProfileUsage` — global token spend for one LLM profile. `CostEur` is set only
/// when the profile carries a price (`omitempty` → skipped when `None`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProfileUsage {
    pub profile_name: String,
    pub calls: i64,
    pub tasks: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_eur: Option<f64>,
}

/// `ProfileDayUsage` — one (profile, UTC calendar day) token bucket.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProfileDayUsage {
    pub profile_name: String,
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
}

/// `JudgeDayUsage` — one UTC-day token bucket for the fallback judge sparkline.
#[derive(Debug, Clone, Default)]
pub struct JudgeDayUsage {
    pub date: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// `JudgeUsage` — cumulative + recent-daily token spend of the intercept fallback
/// judge (`worker='judge'` rows).
#[derive(Debug, Clone, Default)]
pub struct JudgeUsage {
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub daily: Vec<JudgeDayUsage>,
}

/// `nullIfZero`: `> 0` → `Some`, else SQL NULL.
fn null_if_zero(v: i64) -> Option<i64> {
    if v > 0 {
        Some(v)
    } else {
        None
    }
}

impl DB {
    /// `TaskLLMSpend`: token + turn total the in-memory task ledger would count.
    /// Planner/worker/judge/compactor only; every status counts.
    pub async fn task_llm_spend(&self, task_id: &str) -> Result<(i64, i64)> {
        if task_id.is_empty() {
            return Ok((0, 0));
        }
        let row = self
            .query_one(
                "SELECT COALESCE(SUM(input_tokens + output_tokens), 0), COUNT(*) \
             FROM llm_usage \
             WHERE task_id=$1 AND worker IN ('planner', 'worker', 'judge', 'compactor')",
                &[&task_id],
            )
            .await?;
        Ok((row.get(0), row.get(1)))
    }

    /// `InsertLLMUsage`: append one metering row (best-effort at the call site).
    pub async fn insert_llm_usage(&self, u: &LLMUsage) -> Result<()> {
        self.execute(
            "INSERT INTO llm_usage(task_id, exploration_id, worker, model, profile_name, \
             latency_ms, input_tokens, output_tokens, cache_read, cache_write, status) \
             VALUES (NULLIF($1,''),$2,NULLIF($3,''),NULLIF($4,''),NULLIF($5,''),$6,$7,$8,$9,$10,$11)",
            &[
                &u.task_id,
                &null_if_zero(u.exploration_id),
                &u.worker,
                &u.model,
                &u.profile_name,
                &u.latency_ms,
                &u.input_tokens,
                &u.output_tokens,
                &u.cache_read,
                &u.cache_write,
                &u.status,
            ],
        ).await?;
        Ok(())
    }

    /// `TokenByModel`: a task's token usage grouped by model, most-used first.
    pub async fn token_by_model(&self, task_id: &str) -> Result<Vec<ModelTokenStat>> {
        let rows = self
            .query(
                "SELECT COALESCE(NULLIF(model,''),'(unknown)') AS model, COUNT(*) AS calls, \
                    COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                    COALESCE(SUM(cache_read),0), COALESCE(SUM(cache_write),0) \
             FROM llm_usage \
             WHERE COALESCE(task_id,'') = $1 \
             GROUP BY model \
             ORDER BY SUM(input_tokens) + SUM(output_tokens) DESC, model",
                &[&task_id],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(ModelTokenStat {
                model: row.get(0),
                calls: row.get(1),
                input_tokens: row.get(2),
                output_tokens: row.get(3),
                cache_read_tokens: row.get(4),
                cache_write_tokens: row.get(5),
            });
        }
        Ok(out)
    }

    /// `UsageByProfile`: global token spend grouped by profile, most-used first,
    /// merged with archived summaries, then priced ([`apply_cost_eur`]).
    pub async fn usage_by_profile(&self) -> Result<Vec<ProfileUsage>> {
        let rows = self
            .query(
                "SELECT COALESCE(profile_name,'') AS profile_name, COUNT(*) AS calls, \
                    COUNT(DISTINCT task_id) AS tasks, \
                    COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                    COALESCE(SUM(cache_read),0), COALESCE(SUM(cache_write),0) \
             FROM llm_usage \
             GROUP BY profile_name \
             ORDER BY SUM(input_tokens) + SUM(output_tokens) DESC",
                &[],
            )
            .await?;
        let mut by_name: std::collections::BTreeMap<String, ProfileUsage> =
            std::collections::BTreeMap::new();
        for row in &rows {
            let name: String = row.get(0);
            by_name.insert(
                name.clone(),
                ProfileUsage {
                    profile_name: name,
                    calls: row.get(1),
                    tasks: row.get(2),
                    input_tokens: row.get(3),
                    output_tokens: row.get(4),
                    cache_read_tokens: row.get(5),
                    cache_write_tokens: row.get(6),
                    cost_eur: None,
                },
            );
        }
        let archived = archived_task_aggregates(self).await?;
        for aggregate in &archived {
            for cold in &aggregate.token_profiles {
                let current = by_name.entry(cold.profile_name.clone()).or_default();
                current.profile_name = cold.profile_name.clone();
                current.calls += cold.calls;
                current.tasks += cold.tasks;
                current.input_tokens += cold.input_tokens;
                current.output_tokens += cold.output_tokens;
                current.cache_read_tokens += cold.cache_read_tokens;
                current.cache_write_tokens += cold.cache_write_tokens;
            }
        }
        let mut out: Vec<ProfileUsage> = by_name.into_values().collect();
        apply_cost_eur(self, &mut out).await?;
        // Stable order: tokens desc, then profile name asc.
        out.sort_by(|a, b| {
            let left = a.input_tokens + a.output_tokens;
            let right = b.input_tokens + b.output_tokens;
            right
                .cmp(&left)
                .then_with(|| a.profile_name.cmp(&b.profile_name))
        });
        Ok(out)
    }

    /// `UsageDaily`: per-(profile, day) token buckets for the past `days` days
    /// (default 365 for `days <= 0`), merged with archived daily summaries.
    pub async fn usage_daily(&self, days: i64) -> Result<Vec<ProfileDayUsage>> {
        let days = if days <= 0 { 365 } else { days };
        let rows = self
            .query(
                "SELECT COALESCE(profile_name,'') AS profile_name, \
                    to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS day, \
                    COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                    COALESCE(SUM(cache_read),0) \
             FROM llm_usage \
             WHERE ts >= now() - ($1 * interval '1 day') \
             GROUP BY profile_name, day \
             ORDER BY day",
                &[&days],
            )
            .await?;
        let cutoff = gotime::ymd_minus_days(days);
        let mut by_key: std::collections::BTreeMap<String, ProfileDayUsage> =
            std::collections::BTreeMap::new();
        for row in &rows {
            let name: String = row.get(0);
            let day: String = row.get(1);
            by_key.insert(
                format!("{name}\u{0}{day}"),
                ProfileDayUsage {
                    profile_name: name,
                    date: day,
                    input_tokens: row.get(2),
                    output_tokens: row.get(3),
                    cache_read_tokens: row.get(4),
                },
            );
        }
        let archived = archived_task_aggregates(self).await?;
        for aggregate in &archived {
            for cold in &aggregate.token_daily {
                if cold.date < cutoff {
                    continue;
                }
                let key = format!("{}\u{0}{}", cold.profile_name, cold.date);
                let current = by_key.entry(key).or_default();
                current.profile_name = cold.profile_name.clone();
                current.date = cold.date.clone();
                current.input_tokens += cold.input_tokens;
                current.output_tokens += cold.output_tokens;
                current.cache_read_tokens += cold.cache_read_tokens;
            }
        }
        let mut out: Vec<ProfileDayUsage> = by_key.into_values().collect();
        out.sort_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then_with(|| a.profile_name.cmp(&b.profile_name))
        });
        Ok(out)
    }

    /// `JudgeUsageStats`: the fallback judge's all-time totals + a per-day series
    /// over the past `days` days (default 30 for `days <= 0`).
    pub async fn judge_usage_stats(&self, days: i64) -> Result<JudgeUsage> {
        let days = if days <= 0 { 30 } else { days };
        let row = self
            .query_one(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                    COALESCE(SUM(cache_read),0), COALESCE(SUM(cache_write),0) \
             FROM llm_usage \
             WHERE worker = 'judge'",
                &[],
            )
            .await?;
        let mut u = JudgeUsage {
            calls: row.get(0),
            input_tokens: row.get(1),
            output_tokens: row.get(2),
            cache_read_tokens: row.get(3),
            cache_write_tokens: row.get(4),
            daily: Vec::new(),
        };
        let rows = self
            .query(
                "SELECT to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS day, \
                    COUNT(*), COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0) \
             FROM llm_usage \
             WHERE worker = 'judge' AND ts >= now() - ($1 * interval '1 day') \
             GROUP BY day \
             ORDER BY day",
                &[&days],
            )
            .await?;
        for r in &rows {
            u.daily.push(JudgeDayUsage {
                date: r.get(0),
                calls: r.get(1),
                input_tokens: r.get(2),
                output_tokens: r.get(3),
            });
        }
        Ok(u)
    }
}

/// `ParseExpID`: parse the exploration-id segment of a session string (0 when
/// empty/non-numeric/negative — chat sessions keyed by conversation id).
pub fn parse_exp_id(s: &str) -> i64 {
    match s.parse::<i64>() {
        Ok(n) if n >= 0 => n,
        _ => 0,
    }
}

/// `applyCostEur`: fill `CostEur` from stored per-profile prices (EUR / 1M tokens).
/// Profiles with no price keep `None`. Join is by profile name; the first matching
/// profile wins. `price_*_per_m` are `NUMERIC` → cast to `float8` so they scan as
/// `f64` (matching Go's `float64` scan of the same numeric value).
pub async fn apply_cost_eur(d: &DB, out: &mut [ProfileUsage]) -> Result<()> {
    let rows = d
        .query(
            "SELECT name, COALESCE(price_input_per_m,0)::float8, \
                COALESCE(price_output_per_m,0)::float8, \
                COALESCE(price_cache_read_per_m,0)::float8, \
                COALESCE(price_cache_write_per_m,0)::float8 \
         FROM llm_profiles",
            &[],
        )
        .await?;
    struct Prices {
        input: f64,
        output: f64,
        cache_read: f64,
        cache_write: f64,
    }
    let mut by_name: std::collections::BTreeMap<String, Prices> = std::collections::BTreeMap::new();
    for row in &rows {
        let name: String = row.get(0);
        // First matching profile wins (duplicate names are a config error).
        by_name.entry(name).or_insert(Prices {
            input: row.get(1),
            output: row.get(2),
            cache_read: row.get(3),
            cache_write: row.get(4),
        });
    }
    for p in out.iter_mut() {
        let Some(pr) = by_name.get(&p.profile_name) else {
            continue;
        };
        if pr.input == 0.0 && pr.output == 0.0 && pr.cache_read == 0.0 && pr.cache_write == 0.0 {
            continue;
        }
        // input_tokens already includes both cached subsets (Norma normalizes to
        // input + cache read + cache write). The fresh share is therefore input
        // minus both subsets; charging a subset at full input price would double-bill.
        let fresh = (p.input_tokens - p.cache_read_tokens - p.cache_write_tokens).max(0) as f64;
        let cost = fresh / 1e6 * pr.input
            + p.output_tokens as f64 / 1e6 * pr.output
            + p.cache_read_tokens as f64 / 1e6 * pr.cache_read
            + p.cache_write_tokens as f64 / 1e6 * pr.cache_write;
        p.cost_eur = Some(cost);
    }
    Ok(())
}
