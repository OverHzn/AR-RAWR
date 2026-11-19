//! LLM failover health persistence — port of `db/llmhealth.go`.
//!
//! One row per LLM profile tracking its circuit-breaker state. The authoritative
//! copy lives in memory (`llmpool.Registry`); this table only survives a restart
//! so a cooling-off window isn't silently reset by one.
//!
//! `open_until` is `NULL`/past = closed (healthy). [`DB::load_llm_health`] returns
//! only profiles still inside an *unexpired* cooling-off window: a profile that
//! finished cooling is treated as healthy again (re-probed on its next call), not
//! resurrected as broken.

use crate::db::error::Result;
use crate::db::DB;
use std::time::SystemTime;

/// `LLMHealth` — one profile's circuit-breaker state.
#[derive(Debug, Clone)]
pub struct LLMHealth {
    pub profile_id: i64,
    /// Consecutive failures; cleared on success.
    pub fails: i32,
    /// Total trips; drives the backoff ladder.
    pub trips: i32,
    /// Cooldown deadline; `None`/past = closed (healthy).
    pub open_until: Option<SystemTime>,
    pub last_error: String,
    pub last_at: SystemTime,
}

impl DB {
    /// `LoadLLMHealth`: profiles still in an UNEXPIRED cooling-off window.
    pub async fn load_llm_health(&self) -> Result<Vec<LLMHealth>> {
        let rows = self
            .query(
                "SELECT profile_id,fails,trips,open_until,COALESCE(last_error,''),last_at \
                 FROM llm_profile_health WHERE open_until IS NOT NULL AND open_until > now()",
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(LLMHealth {
                profile_id: row.get(0),
                fails: row.get(1),
                trips: row.get(2),
                open_until: row.get(3),
                last_error: row.get(4),
                last_at: row.get(5),
            });
        }
        Ok(out)
    }

    /// `SaveLLMHealth`: upsert one profile's circuit-breaker state (`last_at` is
    /// always stamped `now()`, never taken from the caller).
    pub async fn save_llm_health(&self, h: &LLMHealth) -> Result<()> {
        self.execute(
            "INSERT INTO llm_profile_health(profile_id,fails,trips,open_until,last_error,last_at) \
             VALUES ($1,$2,$3,$4,$5,now()) \
             ON CONFLICT (profile_id) DO UPDATE SET \
               fails=EXCLUDED.fails, trips=EXCLUDED.trips, open_until=EXCLUDED.open_until, \
               last_error=EXCLUDED.last_error, last_at=now()",
            &[
                &h.profile_id,
                &h.fails,
                &h.trips,
                &h.open_until,
                &h.last_error,
            ],
        )
        .await?;
        Ok(())
    }

    /// `ClearLLMHealth`: drop one profile's state (manual "recover now").
    pub async fn clear_llm_health(&self, profile_id: i64) -> Result<()> {
        self.execute(
            "DELETE FROM llm_profile_health WHERE profile_id=$1",
            &[&profile_id],
        )
        .await?;
        Ok(())
    }
}
