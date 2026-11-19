//! Exploration graph store — port of the `ExplorationStore` handle in
//! `db/exploration.go`.
//!
//! A `DB::exploration(id)` handle is bound to one exploration's reasoning graph +
//! activity. Go's `*ExplorationStore` wraps `*DB` + an `expID`; the Rust handle
//! owns a cheap-cloneable [`DB`] (an `Arc` pool) plus the id, so it is `Clone` and
//! Send. This module starts with the handle itself and the root row; the node/edge
//! methods grow here as the exploration group is ported.

use crate::db::error::Result;
use crate::db::DB;

/// `ExplorationStore`: a handle onto one exploration's reasoning graph + activity.
#[derive(Clone)]
pub struct ExplorationStore {
    db: DB,
    exp_id: i64,
}

impl DB {
    /// `CreateExploration`: create a new exploration root, returning its id.
    pub async fn create_exploration(&self, description: &str, goal: &str) -> Result<i64> {
        let row = self
            .query_one(
                "INSERT INTO explorations(description, goal) VALUES ($1, $2) RETURNING id",
                &[&description, &goal],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `Exploration`: a handle bound to an exploration id.
    pub fn exploration(&self, id: i64) -> ExplorationStore {
        ExplorationStore {
            db: self.clone(),
            exp_id: id,
        }
    }
}

impl ExplorationStore {
    /// `ID`: the bound exploration id.
    pub fn id(&self) -> i64 {
        self.exp_id
    }

    /// `Root`: the exploration's description (NULL -> "") and goal.
    pub async fn root(&self) -> Result<(String, String)> {
        let row = self
            .db
            .query_one(
                "SELECT description, goal FROM explorations WHERE id=$1",
                &[&self.exp_id],
            )
            .await?;
        Ok((
            row.get::<_, Option<String>>(0).unwrap_or_default(),
            row.get(1),
        ))
    }
}
