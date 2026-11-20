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

use crate::db::constants::{
    KIND_FACT, KIND_FINDING, KIND_GOAL, KIND_INTENT, REL_DERIVED_FROM, REL_YIELDS, STATE_ORIGIN,
};
use crate::db::error::Error;
use crate::db::gotime;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

// ---------------------------------------------------------------------------
// Intent lifecycle — state transitions (port of the `*ExplorationStore` intent
// state methods in `db/exploration.go`). Soft-delete and cancel (which rewrite
// lineage / roll up tokens) are separate units.
// ---------------------------------------------------------------------------

/// `IntentClaimTTL`: how long an intent claim stays valid without a heartbeat,
/// in seconds. Go's `IntentClaimTTL = 90 * time.Second`; bound into the
/// takeover query as `$2 * INTERVAL '1 second'`.
pub const INTENT_CLAIM_TTL_SECS: f64 = 90.0;

/// `ErrIntentStateConflict`: a compare-and-set / soft-delete transition lost a
/// race with a concurrent state change (callers map it to HTTP 409).
pub const ERR_INTENT_STATE_CONFLICT: &str = "intent state changed concurrently";

/// An intent is terminal (stamps `completed_at`) in these states; reopening
/// (open/running/paused) clears it. Mirrors Go's inline check.
fn is_terminal_intent_state(state: &str) -> bool {
    matches!(state, "done" | "blocked" | "exhausted" | "stopped")
}

impl ExplorationStore {
    /// `ResetRunningIntents`: reopen running intents whose claim expired (or was
    /// never heartbeated — a legacy row), returning the number taken over. The
    /// expiry check and takeover are one atomic UPDATE on the database clock;
    /// clearing `unclaim_token` fences the previous claimer out.
    pub async fn reset_running_intents(&self) -> Result<i64> {
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
              SET state='open', completed_at=NULL, blocked_reason=NULL, unclaim_token=NULL \
              WHERE exploration_id=$1 AND kind='intent' AND state='running' \
                AND (last_heartbeat_at IS NULL OR last_heartbeat_at < now() - $2 * INTERVAL '1 second')",
                &[&self.exp_id, &INTENT_CLAIM_TTL_SECS],
            )
            .await?;
        Ok(n as i64)
    }

    /// `ReopenIntent`: flip ONE not-successfully-finished intent
    /// (blocked/exhausted/stopped) back to 'open' so a worker re-claims it.
    /// done/open/running are left untouched. Returns whether a row changed.
    pub async fn reopen_intent(&self, id: i64) -> Result<bool> {
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
              SET state='open', completed_at=NULL, blocked_reason=NULL \
              WHERE id=$1 AND exploration_id=$2 AND kind='intent' \
                AND state IN ('blocked','exhausted','stopped')",
                &[&id, &self.exp_id],
            )
            .await?;
        Ok(n > 0)
    }

    /// `ReopenBlockedIntents`: flip EVERY 'blocked' intent in this exploration
    /// back to 'open' (batch rerun after a mass outage). Returns the number
    /// reopened.
    pub async fn reopen_blocked_intents(&self) -> Result<i64> {
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
              SET state='open', completed_at=NULL, blocked_reason=NULL \
              WHERE exploration_id=$1 AND kind='intent' AND state='blocked'",
                &[&self.exp_id],
            )
            .await?;
        Ok(n as i64)
    }

    /// `SetIntentState`: set one intent's state. Terminal states stamp
    /// `completed_at`; reopening (open/running/paused) clears it. Bumps
    /// `content_version` and clears `blocked_reason`. No-op if the id is not an
    /// intent of this exploration.
    pub async fn set_intent_state(&self, id: i64, state: &str) -> Result<()> {
        let terminal = is_terminal_intent_state(state);
        self.db
            .execute(
                "UPDATE exploration_nodes \
              SET state=$1, blocked_reason=NULL, content_version=content_version+1, \
                  completed_at = CASE WHEN $4 THEN now() ELSE NULL END \
              WHERE id=$2 AND exploration_id=$3 AND kind='intent'",
                &[&state, &id, &self.exp_id, &terminal],
            )
            .await?;
        Ok(())
    }

    /// `CompareAndSetIntentState`: transition one local intent only when its
    /// current state still matches `expected` (the pause/resume + worker
    /// settlement boundary, so a stale read can't overwrite a concurrent finish).
    /// Returns whether the row changed.
    pub async fn compare_and_set_intent_state(
        &self,
        id: i64,
        expected: &str,
        state: &str,
    ) -> Result<bool> {
        let terminal = is_terminal_intent_state(state);
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
              SET state=$1, blocked_reason=NULL, content_version=content_version+1, \
                  completed_at = CASE WHEN $5 THEN now() ELSE NULL END \
              WHERE id=$2 AND exploration_id=$3 AND kind='intent' AND state=$4",
                &[&state, &id, &self.exp_id, &expected, &terminal],
            )
            .await?;
        Ok(n == 1)
    }

    /// `CountFinishedIntents`: intents in a terminal *explored* state
    /// (done/blocked/exhausted) — excludes 'stopped' (killed/deleted).
    pub async fn count_finished_intents(&self) -> Result<i64> {
        let row = self
            .db
            .query_one(
                "SELECT COUNT(*) FROM exploration_nodes \
              WHERE exploration_id=$1 AND kind='intent' AND state IN ('done','blocked','exhausted')",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `CountOpenIntents`: this exploration's open intents.
    pub async fn count_open_intents(&self) -> Result<i64> {
        let row = self
            .db
            .query_one(
                "SELECT COUNT(*) FROM exploration_nodes \
              WHERE exploration_id=$1 AND kind='intent' AND state='open'",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `SoftDeleteIntent`: soft-delete an open/running/paused intent — it sets
    /// state='deleted' + records the delete reason, keeping the node and its
    /// lineage (unlike a hard cancel). Side-question sessions for the intent are
    /// atomically cleaned up in the same transaction (a kept session would reject
    /// the delayed snapshot). Returns the intent's summary from *before* deletion,
    /// for the planner notification. The caller must stop any running worker
    /// first to avoid later writes. Errors with [`ERR_INTENT_STATE_CONFLICT`] when
    /// the intent is in a non-deletable state (done/blocked/exhausted/stopped/
    /// deleted).
    pub async fn soft_delete_intent(&self, id: i64, reason: &str) -> Result<String> {
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;

        let row = tx
            .query_opt(
                "SELECT state, payload FROM exploration_nodes \
             WHERE id=$1 AND exploration_id=$2 AND kind='intent' FOR UPDATE",
                &[&id, &self.exp_id],
            )
            .await?;
        let (state, payload): (String, Value) = match row {
            Some(r) => (r.get(0), r.get(1)),
            None => return Err(Error::msg("intent not found")),
        };
        if state != "running" && state != "paused" && state != "open" {
            return Err(Error::sentinel(
                ERR_INTENT_STATE_CONFLICT,
                format!("{ERR_INTENT_STATE_CONFLICT}: intent state {state} cannot be deleted"),
            ));
        }

        tx.execute(
            "UPDATE exploration_nodes \
          SET state='deleted', delete_reason=$3, blocked_reason=NULL, \
              content_version=content_version+1, completed_at=now() \
          WHERE id=$1 AND exploration_id=$2",
            &[&id, &self.exp_id, &reason],
        )
        .await?;
        tx.execute(
            "DELETE FROM side_question_sessions WHERE intent_id=$1",
            &[&id],
        )
        .await?;

        let summary = payload
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        tx.commit().await?;
        Ok(summary)
    }

    /// `CancelIntent`: physically delete an intent plus all exclusively-owned
    /// descendant nodes "supported only by that intent" — nodes reachable
    /// downward along yields/derived_from whose every parent lies inside the
    /// deletion set. Goals and origin facts are never deleted; shared nodes still
    /// referenced outside the set are kept, so other branches stay intact. Each
    /// deleted intent gets a token rollup first (preserving irreversible
    /// metering), then has its activity + side sessions cleaned; a deleted finding
    /// has its `findings` row removed first (node_id FK is ON DELETE SET NULL).
    /// Edges CASCADE with the nodes; the whole cleanup is one transaction. The
    /// caller must stop any running worker first. Errors "intent not found" when
    /// the id is not an intent of this exploration (idempotent after deletion),
    /// and "intent cleanup changed concurrently" if the node set shifted.
    pub async fn cancel_intent(&self, id: i64) -> Result<IntentCleanup> {
        let mut out = IntentCleanup::default();
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;

        // Lock the intent row to confirm it exists (idempotent: already gone ->
        // not found). State is not checked — hard deletion is valid for any state.
        let exists = tx
            .query_opt(
                "SELECT 1 FROM exploration_nodes \
             WHERE id=$1 AND exploration_id=$2 AND kind='intent' FOR UPDATE",
                &[&id, &self.exp_id],
            )
            .await?;
        if exists.is_none() {
            return Err(Error::msg("intent not found"));
        }

        // Load the whole graph's nodes (to decide protected) and edges (for
        // downward reachability + the parent sets). The graph is small for real
        // tasks.
        let mut kind: HashMap<i64, String> = HashMap::new();
        let mut protected: HashSet<i64> = HashSet::new();
        let nrows = tx
            .query(
                "SELECT id, kind, state FROM exploration_nodes WHERE exploration_id=$1",
                &[&self.exp_id],
            )
            .await?;
        for r in &nrows {
            let nid: i64 = r.get(0);
            let k: String = r.get(1);
            let st: String = r.get(2);
            if k == KIND_GOAL || (k == KIND_FACT && st == STATE_ORIGIN) {
                protected.insert(nid); // goals + task root facts are never deleted
            }
            kind.insert(nid, k);
        }

        let mut parents_of: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut down_of: HashMap<i64, Vec<i64>> = HashMap::new();
        let erows = tx
            .query(
                "SELECT src_id, rel, dst_id FROM exploration_edges WHERE exploration_id=$1",
                &[&self.exp_id],
            )
            .await?;
        for r in &erows {
            let src: i64 = r.get(0);
            let rel: String = r.get(1);
            let dst: i64 = r.get(2);
            parents_of.entry(dst).or_default().push(src); // any rel counts as a parent
            if rel == REL_YIELDS || rel == REL_DERIVED_FROM {
                down_of.entry(src).or_default().push(dst);
            }
        }

        // Exclusive cascade: expand downward from the intent; a node joins the
        // deletion set iff it is unprotected and every parent is already in the
        // set. Iterate to a fixed point.
        let mut del: HashSet<i64> = HashSet::new();
        del.insert(id);
        loop {
            let mut changed = false;
            let sources: Vec<i64> = del.iter().copied().collect();
            for src in sources {
                if let Some(dsts) = down_of.get(&src) {
                    for &dst in dsts {
                        if del.contains(&dst) || protected.contains(&dst) {
                            continue;
                        }
                        let exclusive = parents_of
                            .get(&dst)
                            .is_none_or(|ps| ps.iter().all(|p| del.contains(p)));
                        if exclusive {
                            del.insert(dst);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }

        // Bucket by kind.
        let mut ids: Vec<i64> = Vec::with_capacity(del.len());
        let mut intent_ids: Vec<i64> = Vec::new();
        let mut finding_ids: Vec<i64> = Vec::new();
        for &nid in &del {
            ids.push(nid);
            match kind.get(&nid).map(String::as_str) {
                Some(KIND_INTENT) => {
                    intent_ids.push(nid);
                    out.intents += 1;
                }
                Some(KIND_FACT) => out.facts += 1,
                Some(KIND_FINDING) => {
                    finding_ids.push(nid);
                    out.findings += 1;
                }
                _ => {}
            }
        }

        // Each deleted intent: token rollup (preserve irreversible metering), then
        // clear activity. The rollup row's node_id is NULL, so the node_id-based
        // deletion below does not hit it.
        for iid in &intent_ids {
            let buckets = intent_token_rollup(&tx, self.exp_id, *iid).await?;
            let n = tx
                .execute(
                    "DELETE FROM activity WHERE exploration_id=$1 AND node_id=$2",
                    &[&self.exp_id, iid],
                )
                .await?;
            out.activities += n as i64;
            for bucket in buckets {
                let metadata = serde_json::json!({
                    "cancelled_intent_id": iid,
                    "token_day": gotime::civil_date_from_days(bucket.day),
                    "token_rollup": true,
                });
                let created_at = UNIX_EPOCH + Duration::from_secs((bucket.day * 86_400) as u64);
                tx.execute(
                    "INSERT INTO activity(exploration_id, worker, kind, summary, metadata, \
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, created_at) \
                 VALUES ($1,'token-ledger','result',$2,$3,$4,$5,$6,$7,$8)",
                    &[
                        &self.exp_id,
                        &format!("Token metering for cancelled intent #{iid}"),
                        &metadata,
                        &(bucket.usage.input as i32),
                        &(bucket.usage.output as i32),
                        &(bucket.usage.cache_read as i32),
                        &(bucket.usage.cache_write as i32),
                        &created_at,
                    ],
                )
                .await?;
            }
            tx.execute(
                "DELETE FROM side_question_sessions WHERE intent_id=$1",
                &[iid],
            )
            .await?;
        }

        // Delete the findings-table row before deleting the finding node (else a
        // node_id=NULL orphan is left behind).
        for fid in &finding_ids {
            tx.execute("DELETE FROM findings WHERE node_id=$1", &[fid])
                .await?;
        }

        // Delete the nodes (edges CASCADE). Delete one by one and verify row
        // counts to guard against concurrent changes.
        let mut removed: i64 = 0;
        for nid in &ids {
            let n = tx
                .execute(
                    "DELETE FROM exploration_nodes WHERE id=$1 AND exploration_id=$2",
                    &[nid, &self.exp_id],
                )
                .await?;
            removed += n as i64;
        }
        if removed != ids.len() as i64 {
            return Err(Error::msg("intent cleanup changed concurrently"));
        }
        tx.commit().await?;
        Ok(out)
    }
}

/// `IntentCleanup`: summary of the blackboard records removed when a user
/// cancels one worker intent. Global assets and traffic are deliberately not
/// part of this operation; anchors disappear only because their owning nodes do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntentCleanup {
    pub intents: i64,
    pub facts: i64,
    pub findings: i64,
    pub activities: i64,
}

/// One per-worker token aggregate (the 4 token counters of Go's `TokenUsage`).
#[derive(Debug, Clone, Copy, Default)]
struct TokenUsage {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
}

/// A day's rolled-up token usage (keyed by UTC day-number since the epoch).
struct TokenUsageBucket {
    day: i64,
    usage: TokenUsage,
}

/// `intentTokenRollup`: exactly-once usage grouped by its original UTC day. Each
/// `result` row terminates one run — a result carrying usage is authoritative; an
/// older error/cancel result without usage falls back to the latest cumulative
/// `usage` frame since the previous result. A trailing `usage` frame with no
/// following result represents a run interrupted before its terminal result was
/// persisted. Buckets come back sorted by day ascending.
async fn intent_token_rollup(
    tx: &deadpool_postgres::Transaction<'_>,
    exploration_id: i64,
    intent_id: i64,
) -> Result<Vec<TokenUsageBucket>> {
    let rows = tx
        .query(
            "SELECT kind, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, \
         created_at FROM activity \
         WHERE exploration_id=$1 AND node_id=$2 AND kind IN ('usage','result') ORDER BY id",
            &[&exploration_id, &intent_id],
        )
        .await?;

    // day-number (UTC days since epoch) -> summed usage.
    let mut by_day: std::collections::BTreeMap<i64, TokenUsage> = std::collections::BTreeMap::new();
    let mut pending = TokenUsage::default();
    let mut pending_at = UNIX_EPOCH;
    let mut has_pending = false;

    for r in &rows {
        let kind: String = r.get(0);
        let input: Option<i32> = r.get(1);
        let output: Option<i32> = r.get(2);
        let read: Option<i32> = r.get(3);
        let write: Option<i32> = r.get(4);
        let created_at: SystemTime = r.get(5);

        let has_current = input.is_some() || output.is_some() || read.is_some() || write.is_some();
        if kind == "usage" {
            if has_current {
                pending = TokenUsage {
                    input: input.unwrap_or(0) as i64,
                    output: output.unwrap_or(0) as i64,
                    cache_read: read.unwrap_or(0) as i64,
                    cache_write: write.unwrap_or(0) as i64,
                };
                pending_at = created_at;
                has_pending = true;
            }
            continue;
        }

        // A terminal 'result' row.
        if has_current {
            // Preserve a partially populated legacy terminal row by filling only
            // its missing dimensions from the latest cumulative usage frame.
            let mut current = TokenUsage {
                input: input.unwrap_or(0) as i64,
                output: output.unwrap_or(0) as i64,
                cache_read: read.unwrap_or(0) as i64,
                cache_write: write.unwrap_or(0) as i64,
            };
            if has_pending {
                if input.is_none() {
                    current.input = pending.input;
                }
                if output.is_none() {
                    current.output = pending.output;
                }
                if read.is_none() {
                    current.cache_read = pending.cache_read;
                }
                if write.is_none() {
                    current.cache_write = pending.cache_write;
                }
            }
            add_usage(&mut by_day, created_at, current);
        } else if has_pending {
            add_usage(&mut by_day, created_at, pending);
        }
        has_pending = false;
    }
    if has_pending {
        add_usage(&mut by_day, pending_at, pending);
    }

    Ok(by_day
        .into_iter()
        .map(|(day, usage)| TokenUsageBucket { day, usage })
        .collect())
}

/// Fold one usage frame into its UTC-day bucket (zero-usage frames are ignored,
/// mirroring Go's early return).
fn add_usage(
    by_day: &mut std::collections::BTreeMap<i64, TokenUsage>,
    at: SystemTime,
    usage: TokenUsage,
) {
    if usage.input == 0 && usage.output == 0 && usage.cache_read == 0 && usage.cache_write == 0 {
        return;
    }
    let secs = at
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let day = secs.div_euclid(86_400);
    let e = by_day.entry(day).or_default();
    e.input += usage.input;
    e.output += usage.output;
    e.cache_read += usage.cache_read;
    e.cache_write += usage.cache_write;
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
