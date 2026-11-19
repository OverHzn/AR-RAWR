//! Skill-gap report — port of `db/skill_gaps.go`.
//!
//! A `SkillGap` is one skill the agent asked for but the platform did not provide,
//! aggregated by skill, agent, and task category. A miss row is written when the
//! model names a skill that does not exist (`skill_usage.found=false`), so this
//! report is the evidence-backed capability backlog rather than a guess.
//!
//! `SkillGaps` mirrors Go's `QueryContext`: it returns `ctx.Err()` if the context
//! is already done, and cancels the in-flight query if the context fires mid-flight.

use crate::db::error::Result;
use crate::db::DB;
use crate::llm::Context;

/// `SkillGap` — one aggregated skill-miss row.
#[derive(Debug, Clone)]
pub struct SkillGap {
    pub skill: String,
    pub agent_key: String,
    pub category_id: i64,
    pub misses: i64,
    pub hits: i64,
    /// `EXTRACT(EPOCH FROM MAX(ts))::bigint` — Unix seconds of the last sighting.
    pub last_seen: i64,
}

/// The `SkillGaps` query, parameterised only by the row `LIMIT` (`$1`).
const SKILL_GAPS_SQL: &str = "
    SELECT su.skill,
           COALESCE(su.agent_key,''),
           COALESCE(t.category_id,0),
           COUNT(*) FILTER (WHERE NOT su.found) AS misses,
           COUNT(*) FILTER (WHERE su.found)     AS hits,
           EXTRACT(EPOCH FROM MAX(su.ts))::bigint
    FROM skill_usage su
    LEFT JOIN tasks t ON t.id=su.task_id
    GROUP BY 1,2,3
    HAVING COUNT(*) FILTER (WHERE NOT su.found) > 0
    ORDER BY misses DESC, 6 DESC, 1
    LIMIT $1";

impl DB {
    /// `SkillGaps`: the aggregated miss report, highest miss count first. `limit`
    /// `<= 0` defaults to 50 (the Go clamp is kept).
    pub async fn skill_gaps(&self, ctx: &Context, limit: i64) -> Result<Vec<SkillGap>> {
        let limit = if limit <= 0 { 50 } else { limit };
        // `QueryContext`: an already-cancelled context returns its error up front.
        if let Some(e) = ctx.err() {
            return Err(e.into());
        }
        // Bind the params slice to a local so it outlives the pinned query future.
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&limit];
        let fut = self.query(SKILL_GAPS_SQL, &params);
        tokio::pin!(fut);
        let rows = tokio::select! {
            biased;
            // Honour cancellation; `cancelled()` only resolves when ctx fires.
            () = ctx.cancelled() => {
                return Err(ctx.err().unwrap_or(crate::llm::Error::Canceled).into());
            }
            rows = fut => rows?,
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(SkillGap {
                skill: row.get(0),
                agent_key: row.get(1),
                category_id: row.get(2),
                misses: row.get(3),
                hits: row.get(4),
                last_seen: row.get(5),
            });
        }
        Ok(out)
    }
}
