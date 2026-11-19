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

// ---------------------------------------------------------------------------
// Nodes + edges (port of the `*ExplorationStore` node methods in
// `db/exploration.go`). The node/edge methods grow here as the group is ported.
// ---------------------------------------------------------------------------

use crate::db::constants::{KIND_FACT, KIND_INTENT};
use crate::db::error::Error;
use serde_json::Value;
use std::time::SystemTime;

/// `Node`: one typed reasoning-graph node. `scanNode` populates the core columns;
/// the finding-aware fields (finding_id/finding_node_id/traffic_count/anchors/
/// source_task_id/inherited) stay at their zero values unless a finding-aware read
/// fills them in later.
#[derive(Debug, Clone)]
pub struct Node {
    pub finding_id: i64,
    pub finding_node_id: i64,
    pub traffic_count: i64,
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    pub priority: i32,
    pub state: String,
    pub origin: String,
    pub owner: String,
    pub blocked_reason: String,
    pub delete_reason: String,
    pub anchors: Vec<i64>,
    pub created_at: SystemTime,
    pub source_task_id: i64,
    pub inherited: bool,
}

/// `nodeCols`: the shared read column list (matches Go's `scanNode` order).
const NODE_COLS: &str = "id, kind, payload, priority, state, COALESCE(origin,''), \
    COALESCE(owner,''), COALESCE(blocked_reason,''), COALESCE(delete_reason,''), created_at";

/// `scanNode`: scan a row on [`NODE_COLS`]. The finding-aware fields are left at
/// their defaults (only the core columns are present in the row).
fn scan_node(row: &tokio_postgres::Row) -> Node {
    Node {
        finding_id: 0,
        finding_node_id: 0,
        traffic_count: 0,
        id: row.get(0),
        kind: row.get(1),
        payload: row.get(2),
        priority: row.get(3),
        state: row.get(4),
        origin: row.get(5),
        owner: row.get(6),
        blocked_reason: row.get(7),
        delete_reason: row.get(8),
        anchors: Vec::new(),
        created_at: row.get(9),
        source_task_id: 0,
        inherited: false,
    }
}

impl ExplorationStore {
    /// `AddNode`: write a typed node with optional anchors to asset ids, in one
    /// transaction. Fact nodes validate their anchor assets exist first.
    pub async fn add_node(
        &self,
        kind: &str,
        payload: &Value,
        priority: i32,
        state: &str,
        origin: &str,
        anchors: &[i64],
    ) -> Result<i64> {
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;

        if kind == KIND_FACT && !anchors.is_empty() {
            check_fact_anchor_assets(&tx, anchors).await?;
        }

        let row = tx
            .query_one(
                "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
                 VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                &[&self.exp_id, &kind, &payload, &priority, &state, &origin],
            )
            .await?;
        let id: i64 = row.get(0);

        for a in anchors {
            tx.execute(
                "INSERT INTO exploration_anchors(node_id, asset_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
                &[&id, a],
            )
            .await?;
        }

        tx.commit().await?;
        Ok(id)
    }

    /// `AddIntent`: an open intent (convenience over [`add_node`](Self::add_node)).
    pub async fn add_intent(
        &self,
        payload: &Value,
        priority: i32,
        anchors: &[i64],
        origin: &str,
    ) -> Result<i64> {
        let origin = if origin.is_empty() { "planner" } else { origin };
        self.add_node(KIND_INTENT, payload, priority, "open", origin, anchors)
            .await
    }

    /// `AddGoal`: a goal node (state open).
    pub async fn add_goal(&self, payload: &Value, origin: &str) -> Result<i64> {
        self.add_node("goal", payload, 0, "open", origin, &[]).await
    }

    /// `OriginFactID`: this exploration's root fact id (the `fact` node with
    /// state='origin'), or 0 when none exists (legacy roots / not yet seeded).
    pub async fn origin_fact_id(&self) -> Result<i64> {
        let row = self
            .db
            .query_opt(
                "SELECT id FROM exploration_nodes \
             WHERE exploration_id=$1 AND kind='fact' AND state='origin' ORDER BY id LIMIT 1",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.map(|r| r.get::<_, i64>(0)).unwrap_or(0))
    }

    /// `Anchor`: record a node->asset reference (idempotent). Non-positive ids are a
    /// no-op (Go returns nil).
    pub async fn anchor(&self, node_id: i64, asset_id: i64) -> Result<()> {
        if node_id <= 0 || asset_id <= 0 {
            return Ok(());
        }
        self.db.execute(
            "INSERT INTO exploration_anchors(node_id, asset_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
            &[&node_id, &asset_id],
        ).await?;
        Ok(())
    }

    /// `Link`: add a typed exploration edge (idempotent).
    pub async fn link(&self, from: i64, rel: &str, to: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO exploration_edges(exploration_id, src_id, rel, dst_id) VALUES ($1,$2,$3,$4) \
             ON CONFLICT (exploration_id, src_id, rel, dst_id) DO NOTHING",
            &[&self.exp_id, &from, &rel, &to],
        ).await?;
        Ok(())
    }

    /// `SetNodeState`: update any node's state (never deletes). Bumps
    /// content_version so a folded member's state flip invalidates its digest body.
    pub async fn set_node_state(&self, id: i64, state: &str) -> Result<()> {
        self.db.execute(
            "UPDATE exploration_nodes SET state=$1, blocked_reason=NULL, content_version=content_version+1 \
             WHERE id=$2 AND exploration_id=$3",
            &[&state, &id, &self.exp_id],
        ).await?;
        Ok(())
    }

    /// `GetNode`: one node of this exploration (by id), or `None`.
    pub async fn get_node(&self, id: i64) -> Result<Option<Node>> {
        let sql =
            format!("SELECT {NODE_COLS} FROM exploration_nodes WHERE id=$1 AND exploration_id=$2");
        let row = self.db.query_opt(&sql, &[&id, &self.exp_id]).await?;
        Ok(row.as_ref().map(scan_node))
    }
}

/// `checkFactAnchorAssets`: verify every anchor asset exists, locking the rows with
/// `FOR KEY SHARE` so a concurrent delete can't slip in before commit.
async fn check_fact_anchor_assets(
    tx: &deadpool_postgres::Transaction<'_>,
    ids: &[i64],
) -> Result<()> {
    let rows = tx
        .query(
            "SELECT id FROM assets WHERE id = ANY($1::bigint[]) ORDER BY id FOR KEY SHARE",
            &[&ids],
        )
        .await?;
    let mut found = std::collections::HashSet::with_capacity(rows.len());
    for row in &rows {
        found.insert(row.get::<_, i64>(0));
    }
    let missing: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| !found.contains(id))
        .collect();
    if !missing.is_empty() {
        return Err(Error::msg(format!(
            "asset IDs do not exist: {missing:?}; use existing IDs or register the assets first"
        )));
    }
    Ok(())
}
