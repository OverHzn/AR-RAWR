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
    KIND_DIGEST, KIND_FACT, KIND_FINDING, KIND_GOAL, KIND_INTENT, REL_COVERS, REL_DERIVED_FROM,
    REL_YIELDS, STATE_DIGEST_ACTIVE, STATE_DIGEST_SUPERSEDED, STATE_ORIGIN,
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

/// `ErrIntentClaimLost`: a fenced claim write (heartbeat / settle) found the
/// caller's unclaim token no longer current — the claim was taken over, or the
/// lease expired and another worker claimed the intent.
pub const ERR_INTENT_CLAIM_LOST: &str = "intent claim lost";

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

/// `TokenUsage`: one token aggregate (Go's `TokenUsage`) — the worker label plus
/// the 4 token counters. `worker` is empty for the rollup / whole-task totals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub worker: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// `SessionTokenUsage`: one session's token aggregate (Go's `SessionTokenUsage`) —
/// a session key (`main:<seg>` / `plan` / `intent:<id>`) plus the 4 counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTokenUsage {
    pub session: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
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
                    worker: String::new(),
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
                worker: String::new(),
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
            add_usage(&mut by_day, created_at, &current);
        } else if has_pending {
            add_usage(&mut by_day, created_at, &pending);
        }
        has_pending = false;
    }
    if has_pending {
        add_usage(&mut by_day, pending_at, &pending);
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
    usage: &TokenUsage,
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

// ---------------------------------------------------------------------------
// Node + edge reads (port of the `*ExplorationStore` listing methods in
// `db/exploration.go`) — read-only graph queries for viz / pagination / lineage.
// ---------------------------------------------------------------------------

/// `Edge`: one row from `exploration_edges`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: i64,
    pub rel: String,
    pub to: i64,
}

/// `NodeFilter`: narrows a [`nodes_page`](ExplorationStore::nodes_page) query.
/// Empty fields include every value.
#[derive(Debug, Clone, Default)]
pub struct NodeFilter {
    /// `exploration_nodes.kind` whitelist.
    pub kinds: Vec<String>,
    /// `exploration_nodes.state` whitelist.
    pub states: Vec<String>,
    /// Case-insensitive substring over `payload::text` / `origin`. A bare number
    /// (or the `#`-prefixed form the UI shows) is also matched as an exact id.
    pub query: String,
    /// `true` = oldest first (replay); default newest first (broadcast).
    pub asc: bool,
}

/// `scanNodes`: scan a batch of rows on [`NODE_COLS`].
fn scan_nodes(rows: &[tokio_postgres::Row]) -> Vec<Node> {
    rows.iter().map(scan_node).collect()
}

fn scan_edges(rows: &[tokio_postgres::Row]) -> Vec<Edge> {
    rows.iter()
        .map(|r| Edge {
            from: r.get(0),
            rel: r.get(1),
            to: r.get(2),
        })
        .collect()
}

impl ExplorationStore {
    /// `Nodes`: list all nodes in id order (graph viz), capped at `limit`
    /// (default 2000).
    pub async fn nodes(&self, limit: i32) -> Result<Vec<Node>> {
        let limit = (if limit <= 0 { 2000 } else { limit }) as i64;
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes WHERE exploration_id=$1 ORDER BY id LIMIT $2"
        );
        let rows = self.db.query(&sql, &[&self.exp_id, &limit]).await?;
        Ok(scan_nodes(&rows))
    }

    /// `ListByKind`: nodes of a kind, newest first, capped at `limit` (default 500).
    pub async fn list_by_kind(&self, kind: &str, limit: i32) -> Result<Vec<Node>> {
        let limit = (if limit <= 0 { 500 } else { limit }) as i64;
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes \
         WHERE exploration_id=$1 AND kind=$2 ORDER BY id DESC LIMIT $3"
        );
        let rows = self.db.query(&sql, &[&self.exp_id, &kind, &limit]).await?;
        Ok(scan_nodes(&rows))
    }

    /// `ListByKindPage`: one newest-first page of a kind for reverse pagination —
    /// up to `limit` nodes with id < `before` (`before`<=0 = newest page). The bool
    /// reports whether still-older nodes exist, so a client can page past the old
    /// fixed cap instead of losing older intents.
    pub async fn list_by_kind_page(
        &self,
        kind: &str,
        before: i64,
        limit: i32,
    ) -> Result<(Vec<Node>, bool)> {
        let limit = if limit <= 0 { 300 } else { limit };
        let limit1 = limit as i64 + 1; // fetch one extra to probe hasMore
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes \
         WHERE exploration_id=$1 AND kind=$2 AND ($3 <= 0::bigint OR id < $3) \
         ORDER BY id DESC LIMIT $4"
        );
        let rows = self
            .db
            .query(&sql, &[&self.exp_id, &kind, &before, &limit1])
            .await?;
        let mut nodes = scan_nodes(&rows);
        let has_more = nodes.len() > limit as usize;
        if has_more {
            nodes.truncate(limit as usize);
        }
        Ok((nodes, has_more))
    }

    /// `NodesPage`: one 1-based page of nodes (plus the total matching count) under
    /// an optional [`NodeFilter`]. `page`<1 -> 1; `size`<=0 -> 20, capped at 200.
    /// Pages in SQL (the broadcast board reads the graph as a time series) rather
    /// than pulling the whole graph like [`nodes`](Self::nodes) does.
    pub async fn nodes_page(
        &self,
        f: NodeFilter,
        page: i32,
        size: i32,
    ) -> Result<(Vec<Node>, i64)> {
        let page = if page < 1 { 1 } else { page } as i64;
        let mut size = size as i64;
        if size <= 0 {
            size = 20;
        }
        if size > 200 {
            size = 200;
        }
        let offset = (page - 1) * size;

        let q = f.query.trim().to_string();
        let like_pat = format!("%{q}%");
        // A bare number (or the UI's "#41" form) is also an exact id match.
        let id_val: Option<i64> = q.strip_prefix('#').unwrap_or(q.as_str()).parse().ok();
        let id_inner: i64 = id_val.unwrap_or(0);

        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&self.exp_id];
        let mut conds: Vec<String> = vec!["exploration_id=$1".to_string()];
        if !f.kinds.is_empty() {
            params.push(&f.kinds);
            conds.push(format!("kind = ANY(${}::text[])", params.len()));
        }
        if !f.states.is_empty() {
            params.push(&f.states);
            conds.push(format!("state = ANY(${}::text[])", params.len()));
        }
        if !q.is_empty() {
            params.push(&like_pat);
            let n = params.len();
            let mut ors = vec![
                format!("payload::text ILIKE ${n}"),
                format!("COALESCE(origin,'') ILIKE ${n}"),
            ];
            if id_val.is_some() {
                params.push(&id_inner);
                ors.push(format!("id = ${}", params.len()));
            }
            conds.push(format!("({})", ors.join(" OR ")));
        }
        let where_clause = conds.join(" AND ");

        let total: i64 = self
            .db
            .query_one(
                &format!("SELECT COUNT(*) FROM exploration_nodes WHERE {where_clause}"),
                &params,
            )
            .await?
            .get(0);

        params.push(&size);
        params.push(&offset);
        let limit_idx = params.len() - 1;
        let offset_idx = params.len();
        let order = if f.asc { "ASC" } else { "DESC" };
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes WHERE {where_clause} \
         ORDER BY id {order} LIMIT ${limit_idx} OFFSET ${offset_idx}"
        );
        let rows = self.db.query(&sql, &params).await?;
        Ok((scan_nodes(&rows), total))
    }

    /// `NodesByIDs`: load the given nodes of this exploration in id order (empty
    /// input -> empty). Resolves the neighbours of a board page without pulling the
    /// whole graph.
    pub async fn nodes_by_ids(&self, ids: &[i64]) -> Result<Vec<Node>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes \
         WHERE exploration_id=$1 AND id = ANY($2::bigint[]) ORDER BY id"
        );
        let rows = self.db.query(&sql, &[&self.exp_id, &ids]).await?;
        Ok(scan_nodes(&rows))
    }

    /// `Edges`: list exploration edges (graph viz), capped at `limit` (default
    /// 5000).
    pub async fn edges(&self, limit: i32) -> Result<Vec<Edge>> {
        let limit = (if limit <= 0 { 5000 } else { limit }) as i64;
        let rows = self
            .db
            .query(
                "SELECT src_id, rel, dst_id FROM exploration_edges \
             WHERE exploration_id=$1 LIMIT $2",
                &[&self.exp_id, &limit],
            )
            .await?;
        Ok(scan_edges(&rows))
    }

    /// `EdgesTouching`: every edge with one end among `ids` (empty input -> empty).
    pub async fn edges_touching(&self, ids: &[i64]) -> Result<Vec<Edge>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .db
            .query(
                "SELECT src_id, rel, dst_id FROM exploration_edges \
             WHERE exploration_id=$1 AND \
                   (src_id = ANY($2::bigint[]) OR dst_id = ANY($2::bigint[]))",
                &[&self.exp_id, &ids],
            )
            .await?;
        Ok(scan_edges(&rows))
    }

    /// `FactsYielded`: ids of fact nodes an intent produced (intent --yields-->
    /// fact), oldest first. Best-effort (empty for an intent with no facts).
    pub async fn facts_yielded(&self, intent_id: i64) -> Result<Vec<i64>> {
        let rows = self
            .db
            .query(
                "SELECT n.id FROM exploration_edges e \
             JOIN exploration_nodes n ON n.id=e.dst_id AND n.exploration_id=e.exploration_id \
             WHERE e.exploration_id=$1 AND e.src_id=$2 AND e.rel=$3 AND n.kind=$4 \
             ORDER BY n.id",
                &[&self.exp_id, &intent_id, &REL_YIELDS, &KIND_FACT],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get::<_, i64>(0)).collect())
    }
}

// ---------------------------------------------------------------------------
// Frontier + goals (port of the `*ExplorationStore` frontier / goal methods in
// `db/exploration.go`).
// ---------------------------------------------------------------------------

impl ExplorationStore {
    /// `Frontier`: this exploration's open intents, highest priority first
    /// (ties broken by id ascending), capped at `limit` (default 50). The
    /// claimable-work view the planner schedules against.
    pub async fn frontier(&self, limit: i32) -> Result<Vec<Node>> {
        let limit = (if limit <= 0 { 50 } else { limit }) as i64;
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes \
         WHERE exploration_id=$1 AND kind='intent' AND state='open' \
         ORDER BY priority DESC, id ASC LIMIT $2"
        );
        let rows = self.db.query(&sql, &[&self.exp_id, &limit]).await?;
        Ok(scan_nodes(&rows))
    }

    /// `HasActiveIntent`: whether any intent is still in play (open OR running).
    /// Counting open+running (not just open) is race-proof against a worker that
    /// claimed a seeded intent between the check and the first planner round.
    pub async fn has_active_intent(&self) -> Result<bool> {
        let row = self
            .db
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM exploration_nodes \
             WHERE exploration_id=$1 AND kind='intent' AND state IN ('open','running'))",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `HasOpenGoal`: whether any goal is still 'open'. false => all goals are
    /// met/abandoned (or there are none) => the planner stops and the task's end
    /// is decided by whether the frontier drains.
    pub async fn has_open_goal(&self) -> Result<bool> {
        let row = self
            .db
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM exploration_nodes \
             WHERE exploration_id=$1 AND kind='goal' AND state='open')",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `OpenGoalFingerprint`: a stable fingerprint of the open-goal set — the
    /// sorted goal ids joined by commas ("" when none). Planner quiet-mode uses it
    /// to tell "the goal set changed" from "nothing the plan depends on changed".
    pub async fn open_goal_fingerprint(&self) -> Result<String> {
        let rows = self
            .db
            .query(
                "SELECT id FROM exploration_nodes \
             WHERE exploration_id=$1 AND kind='goal' AND state='open' ORDER BY id",
                &[&self.exp_id],
            )
            .await?;
        let ids: Vec<String> = rows
            .iter()
            .map(|r| r.get::<_, i64>(0).to_string())
            .collect();
        Ok(ids.join(","))
    }

    /// `UpdateGoalPayload`: replace a goal's payload with `{"text", "vulnclass"?}`
    /// (`vulnclass` only when non-empty). Scoped to kind='goal'; errors "goal does
    /// not exist" when the id is not a goal of this exploration.
    pub async fn update_goal_payload(&self, id: i64, text: &str, vulnclass: &str) -> Result<()> {
        let mut map = serde_json::Map::new();
        map.insert("text".to_string(), Value::String(text.to_string()));
        if !vulnclass.is_empty() {
            map.insert(
                "vulnclass".to_string(),
                Value::String(vulnclass.to_string()),
            );
        }
        let payload = Value::Object(map);
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes SET payload=$1 \
             WHERE id=$2 AND exploration_id=$3 AND kind='goal'",
                &[&payload, &id, &self.exp_id],
            )
            .await?;
        if n == 0 {
            return Err(Error::msg("goal does not exist"));
        }
        Ok(())
    }

    /// `DeleteGoal`: hard-delete a goal node, scoped to kind='goal' so it can
    /// never drop an intent/fact by id (spawns edges + anchors CASCADE; activity
    /// node_id SET NULL). Errors "goal does not exist" when no such goal exists.
    pub async fn delete_goal(&self, id: i64) -> Result<()> {
        let n = self
            .db
            .execute(
                "DELETE FROM exploration_nodes \
             WHERE id=$1 AND exploration_id=$2 AND kind='goal'",
                &[&id, &self.exp_id],
            )
            .await?;
        if n == 0 {
            return Err(Error::msg("goal does not exist"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Finding reads (port of the `*ExplorationStore` finding methods in
// `db/exploration.go`): lineage + producing-intent maps.
// ---------------------------------------------------------------------------

impl ExplorationStore {
    /// `FindingLineage`: the ancestors of `node_id` — the node itself plus every
    /// node that can reach it via edges (walk src<-dst), in id order, together with
    /// the edges internal to that set. Returns `(vec![], vec![])` when the node
    /// doesn't exist (Go returns nil nodes and a nil edge slice; an empty result
    /// carries the same "no lineage" signal to callers).
    pub async fn finding_lineage(&self, node_id: i64) -> Result<(Vec<Node>, Vec<Edge>)> {
        let node_sql = format!(
            "WITH RECURSIVE anc(id) AS (\
         SELECT $2::bigint \
         UNION \
         SELECT e.src_id FROM exploration_edges e \
         JOIN anc ON e.dst_id = anc.id \
         WHERE e.exploration_id = $1\
         ) \
         SELECT {NODE_COLS} FROM exploration_nodes \
         WHERE exploration_id = $1 AND id IN (SELECT id FROM anc) ORDER BY id"
        );
        let node_rows = self.db.query(&node_sql, &[&self.exp_id, &node_id]).await?;
        let nodes = scan_nodes(&node_rows);
        if nodes.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let edge_sql = "WITH RECURSIVE anc(id) AS (\
             SELECT $2::bigint \
             UNION \
             SELECT e.src_id FROM exploration_edges e \
             JOIN anc ON e.dst_id = anc.id \
             WHERE e.exploration_id = $1\
             ) \
             SELECT src_id, rel, dst_id FROM exploration_edges \
             WHERE exploration_id = $1 \
             AND src_id IN (SELECT id FROM anc) \
             AND dst_id IN (SELECT id FROM anc)";
        let edge_rows = self.db.query(edge_sql, &[&self.exp_id, &node_id]).await?;
        Ok((nodes, scan_edges(&edge_rows)))
    }

    /// `FindingIntents`: map each finding id to the intent that produced it (the
    /// intent --yields--> finding edge; report_finding links it). Findings with no
    /// producing intent are absent. Precise (JOIN, no edge-scan limit).
    pub async fn finding_intents(&self) -> Result<std::collections::BTreeMap<i64, i64>> {
        let rows = self
            .db
            .query(
                "SELECT e.dst_id, e.src_id FROM exploration_edges e \
             JOIN exploration_nodes n ON n.id = e.dst_id AND n.exploration_id = e.exploration_id \
             WHERE e.exploration_id=$1 AND e.rel=$2 AND n.kind='finding'",
                &[&self.exp_id, &REL_YIELDS],
            )
            .await?;
        let mut out = std::collections::BTreeMap::new();
        for r in &rows {
            out.insert(r.get::<_, i64>(0), r.get::<_, i64>(1));
        }
        Ok(out)
    }

    /// `FindingIntentsTerminal`: the inherited-history variant — a finding's
    /// producing intent only when it has reached an immutable terminal state
    /// (done/blocked/exhausted/stopped). Keeps a source task's live worker topology
    /// private.
    pub async fn finding_intents_terminal(&self) -> Result<std::collections::BTreeMap<i64, i64>> {
        let rows = self
            .db
            .query(
                "SELECT e.dst_id, e.src_id \
             FROM exploration_edges e \
             JOIN exploration_nodes finding ON finding.id=e.dst_id AND finding.exploration_id=e.exploration_id \
             JOIN exploration_nodes intent ON intent.id=e.src_id AND intent.exploration_id=e.exploration_id \
             WHERE e.exploration_id=$1 AND e.rel=$2 AND finding.kind='finding' \
               AND intent.kind='intent' \
               AND intent.state IN ('done','blocked','exhausted','stopped')",
                &[&self.exp_id, &REL_YIELDS],
            )
            .await?;
        let mut out = std::collections::BTreeMap::new();
        for r in &rows {
            out.insert(r.get::<_, i64>(0), r.get::<_, i64>(1));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Intent claim lifecycle (port of the `*ExplorationStore` claim methods in
// `db/exploration.go`): the worker claim boundary — claim open/paused intent,
// renew its lease, and settle its result, all fenced by a fresh unclaim token.
// ---------------------------------------------------------------------------

/// `newUnclaimToken`: 16 random bytes hex-encoded (a 32-char hex token), the
/// fencing token a worker must present to heartbeat / settle its claim.
fn new_unclaim_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    let mut out = String::with_capacity(32);
    for byte in b {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

impl ExplorationStore {
    /// `ClaimIntent`: claim an open intent for `owner` (true when claimed). Thin
    /// wrapper over [`claim_intent_token`](Self::claim_intent_token).
    pub async fn claim_intent(&self, id: i64, owner: &str) -> Result<bool> {
        let token = self.claim_intent_token(id, owner).await?;
        Ok(!token.is_empty())
    }

    /// `ClaimIntentToken`: claim an open intent for `owner`, returning the claim's
    /// unclaim token ("" when it was not claimable). Every claim gets a fresh
    /// random token and starts its lease (`last_heartbeat_at`) in the same
    /// statement; the claimer must present the token to
    /// [`heartbeat_intent_claim`](Self::heartbeat_intent_claim) and the
    /// [`settle_intent_claim*`](Self::settle_intent_claim) writes.
    pub async fn claim_intent_token(&self, id: i64, owner: &str) -> Result<String> {
        let token = new_unclaim_token();
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
             SET state='running', owner=$1, unclaim_token=$4, last_heartbeat_at=now() \
             WHERE id=$2 AND exploration_id=$3 AND kind='intent' AND state='open'",
                &[&owner, &id, &self.exp_id, &token],
            )
            .await?;
        if n != 1 {
            return Ok(String::new());
        }
        Ok(token)
    }

    /// `ClaimPausedIntentToken`: the paused->running counterpart of
    /// [`claim_intent_token`](Self::claim_intent_token) (the human-message path,
    /// which never goes through 'open'). Same column updates as a paused->running
    /// CAS plus a fresh token. Returns "" when the intent is no longer paused.
    pub async fn claim_paused_intent_token(&self, id: i64) -> Result<String> {
        let token = new_unclaim_token();
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
             SET state='running', blocked_reason=NULL, content_version=content_version+1, \
                 completed_at=NULL, unclaim_token=$3, last_heartbeat_at=now() \
             WHERE id=$1 AND exploration_id=$2 AND kind='intent' AND state='paused'",
                &[&id, &self.exp_id, &token],
            )
            .await?;
        if n != 1 {
            return Ok(String::new());
        }
        Ok(token)
    }

    /// `HeartbeatIntentClaim`: renew the lease of the claim identified by `token`.
    /// Touches the row only while `token` is the running intent's current unclaim
    /// token; otherwise returns [`ERR_INTENT_CLAIM_LOST`] and writes nothing.
    pub async fn heartbeat_intent_claim(&self, id: i64, token: &str) -> Result<()> {
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes SET last_heartbeat_at=now() \
             WHERE id=$1 AND exploration_id=$2 AND kind='intent' \
             AND state='running' AND unclaim_token=$3",
                &[&id, &self.exp_id, &token],
            )
            .await?;
        if n != 1 {
            return Err(Error::sentinel(
                ERR_INTENT_CLAIM_LOST,
                format!("{ERR_INTENT_CLAIM_LOST}: heartbeat for intent {id} rejected"),
            ));
        }
        Ok(())
    }

    /// `SettleIntentClaim`: the fenced write of a worker's intent result — move the
    /// running intent to `state` and release the claim, but only while `token` is
    /// still the current unclaim token. A worker whose claim was taken over gets
    /// [`ERR_INTENT_CLAIM_LOST`]; a still-owned token on an intent that left
    /// 'running' concurrently gets [`ERR_INTENT_STATE_CONFLICT`].
    pub async fn settle_intent_claim(&self, id: i64, token: &str, state: &str) -> Result<()> {
        let terminal = is_terminal_intent_state(state);
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
             SET state=$1, blocked_reason=NULL, content_version=content_version+1, \
                 completed_at = CASE WHEN $5 THEN now() ELSE NULL END, unclaim_token=NULL \
             WHERE id=$2 AND exploration_id=$3 AND kind='intent' \
             AND state='running' AND unclaim_token=$4",
                &[&state, &id, &self.exp_id, &token, &terminal],
            )
            .await?;
        self.settle_intent_claim_result(id, token, n).await
    }

    /// `SettleIntentClaimBlocked`: the fenced counterpart of `SetIntentBlockedReason`
    /// — settle the running intent as blocked with `reason` under the same token
    /// check as [`settle_intent_claim`](Self::settle_intent_claim).
    pub async fn settle_intent_claim_blocked(
        &self,
        id: i64,
        token: &str,
        reason: &str,
    ) -> Result<()> {
        let n = self
            .db
            .execute(
                "UPDATE exploration_nodes \
             SET state='blocked', blocked_reason=NULLIF($1,''), completed_at=now(), unclaim_token=NULL \
             WHERE id=$2 AND exploration_id=$3 AND kind='intent' \
             AND state='running' AND unclaim_token=$4",
                &[&reason, &id, &self.exp_id, &token],
            )
            .await?;
        self.settle_intent_claim_result(id, token, n).await
    }

    /// `settleIntentClaimResult`: turn a fenced settle that matched no row into the
    /// explicit rejection — claim lost unless the token is still the current one (in
    /// which case the intent merely left 'running', a state conflict).
    async fn settle_intent_claim_result(&self, id: i64, token: &str, affected: u64) -> Result<()> {
        if affected == 1 {
            return Ok(());
        }
        let current: Option<String> = match self
            .db
            .query_opt(
                "SELECT unclaim_token FROM exploration_nodes \
             WHERE id=$1 AND exploration_id=$2 AND kind='intent'",
                &[&id, &self.exp_id],
            )
            .await?
        {
            Some(r) => r.get::<_, Option<String>>(0),
            None => None,
        };
        if current.as_deref() == Some(token) {
            return Err(Error::sentinel(
                ERR_INTENT_STATE_CONFLICT,
                format!(
                    "{ERR_INTENT_STATE_CONFLICT}: intent {id} is no longer in the running state"
                ),
            ));
        }
        Err(Error::sentinel(
            ERR_INTENT_CLAIM_LOST,
            format!("{ERR_INTENT_CLAIM_LOST}: result write for intent {id} rejected"),
        ))
    }
}

// ---------------------------------------------------------------------------
// Stats + token aggregation (port of the `*ExplorationStore` stat methods in
// `db/exploration.go`): dashboard node counts and whole-task / per-worker /
// per-session token totals (all from the `kind='result'` activity rows).
// ---------------------------------------------------------------------------

impl ExplorationStore {
    /// `Stats`: node counts grouped by kind (for the dashboard).
    pub async fn stats(&self) -> Result<std::collections::HashMap<String, i64>> {
        let rows = self
            .db
            .query(
                "SELECT kind, count(*) FROM exploration_nodes \
             WHERE exploration_id=$1 GROUP BY kind",
                &[&self.exp_id],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            out.insert(r.get::<_, String>(0), r.get::<_, i64>(1));
        }
        Ok(out)
    }

    /// `TokenTotal`: sum token usage across ALL workers for this exploration
    /// (the whole-task total). Same source as
    /// [`token_stats_by_worker`](Self::token_stats_by_worker) (`kind='result'`
    /// rows), just ungrouped.
    pub async fn token_total(&self) -> Result<TokenUsage> {
        let row = self
            .db
            .query_one(
                "SELECT \
             COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
             COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0) \
             FROM activity WHERE exploration_id=$1 AND kind='result'",
                &[&self.exp_id],
            )
            .await?;
        Ok(TokenUsage {
            worker: String::new(),
            input: row.get(0),
            output: row.get(1),
            cache_read: row.get(2),
            cache_write: row.get(3),
        })
    }

    /// `TokenStatsByWorker`: sum token usage per worker for this exploration (from
    /// the `kind='result'` records that carry usage), ordered by worker. Used by
    /// the per-agent token display.
    pub async fn token_stats_by_worker(&self) -> Result<Vec<TokenUsage>> {
        let rows = self
            .db
            .query(
                "SELECT COALESCE(worker,''), \
             COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
             COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0) \
             FROM activity WHERE exploration_id=$1 AND kind='result' \
             GROUP BY worker ORDER BY worker",
                &[&self.exp_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TokenUsage {
                worker: r.get(0),
                input: r.get(1),
                output: r.get(2),
                cache_read: r.get(3),
                cache_write: r.get(4),
            })
            .collect())
    }

    /// `TokenStatsBySession`: aggregate every persisted completed run, independent
    /// of activity-history pagination. Main/planner use fixed keys (`main:<seg>` /
    /// `plan`); worker executions use their intent node id (`intent:<id>`), even
    /// when a different `work#N` process handled a retry.
    pub async fn token_stats_by_session(&self) -> Result<Vec<SessionTokenUsage>> {
        let rows = self
            .db
            .query(
                "SELECT \
             CASE \
                 WHEN COALESCE(a.worker,'')='mainagent' THEN 'main:' || COALESCE(a.main_seg,0)::text \
                 WHEN COALESCE(a.worker,'')='planner' THEN 'plan' \
                 WHEN n.id IS NOT NULL THEN 'intent:' || n.id::text \
                 ELSE '' \
             END AS session_key, \
             COALESCE(SUM(a.input_tokens),0), COALESCE(SUM(a.output_tokens),0), \
             COALESCE(SUM(a.cache_read_tokens),0), COALESCE(SUM(a.cache_write_tokens),0) \
             FROM activity a \
             LEFT JOIN exploration_nodes n \
               ON n.id=a.node_id AND n.exploration_id=a.exploration_id AND n.kind='intent' \
             WHERE a.exploration_id=$1 AND a.kind='result' \
               AND (COALESCE(a.worker,'') IN ('mainagent','planner') OR n.id IS NOT NULL) \
             GROUP BY session_key ORDER BY session_key",
                &[&self.exp_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| SessionTokenUsage {
                session: r.get(0),
                input: r.get(1),
                output: r.get(2),
                cache_read: r.get(3),
                cache_write: r.get(4),
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// DB-level exploration aggregates (port of the `*DB` exploration stat methods in
// `db/exploration.go`): one-query roll-ups across all explorations for the task
// list, plus a stuck-task diagnostic.
// ---------------------------------------------------------------------------

/// `GoalCounts`: the goal summary (total / met) for one exploration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoalCounts {
    pub total: i64,
    pub met: i64,
}

impl DB {
    /// `ExplorationDiag`: diagnostic for a stuck task. `(exp_exists, task_refs,
    /// max_exp_id)` tells apart the failure modes — no row + no task refs = the
    /// whole row set is gone (DB reset / wrong DB); no row + task refs = a broken
    /// FK (impossible under RESTRICT); row exists = the write's id is stale/wrong
    /// (compare against the in-memory store's id).
    pub async fn exploration_diag(&self, exp_id: i64) -> Result<(bool, i64, i64)> {
        let row = self
            .query_one(
                "SELECT \
             EXISTS(SELECT 1 FROM explorations WHERE id=$1), \
             (SELECT COUNT(*) FROM tasks WHERE exploration_id=$1), \
             COALESCE((SELECT MAX(id) FROM explorations),0)",
                &[&exp_id],
            )
            .await?;
        Ok((row.get(0), row.get(1), row.get(2)))
    }

    /// `TokenTotalsAll`: the whole-task token total for every exploration in one
    /// query (`exploration_id -> total`), so the task list shows per-task
    /// consumption without a query per row.
    pub async fn token_totals_all(&self) -> Result<std::collections::HashMap<i64, TokenUsage>> {
        let rows = self
            .query(
                "SELECT exploration_id, \
             COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
             COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_write_tokens),0) \
             FROM activity WHERE kind='result' GROUP BY exploration_id",
                &[],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            out.insert(
                r.get::<_, i64>(0),
                TokenUsage {
                    worker: String::new(),
                    input: r.get(1),
                    output: r.get(2),
                    cache_read: r.get(3),
                    cache_write: r.get(4),
                },
            );
        }
        Ok(out)
    }

    /// `LastActivityAll`: the unix time of the most recent activity per
    /// exploration (`exploration_id -> max created_at epoch`), one query for all
    /// tasks. Persisted (unlike the in-memory engine map), so it survives restarts
    /// and gives terminal-state tasks a stable "ran until" time for run duration.
    pub async fn last_activity_all(&self) -> Result<std::collections::HashMap<i64, i64>> {
        let rows = self
            .query(
                "SELECT exploration_id, EXTRACT(EPOCH FROM MAX(created_at))::bigint \
             FROM activity GROUP BY exploration_id",
                &[],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            out.insert(r.get::<_, i64>(0), r.get::<_, i64>(1));
        }
        Ok(out)
    }

    /// `GoalCountsAll`: goal totals (total / met) per exploration id, one query for
    /// all tasks — used by `listTasks` so the task list shows progress for every
    /// task.
    pub async fn goal_counts_all(&self) -> Result<std::collections::HashMap<i64, GoalCounts>> {
        let rows = self
            .query(
                "SELECT exploration_id, \
             COUNT(*) AS total, \
             COUNT(*) FILTER (WHERE state = 'met') AS met \
             FROM exploration_nodes WHERE kind = 'goal' GROUP BY exploration_id",
                &[],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            out.insert(
                r.get::<_, i64>(0),
                GoalCounts {
                    total: r.get(1),
                    met: r.get(2),
                },
            );
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Task-list metrics + daily token rollup (port of the `*DB` aggregate methods in
// `db/exploration.go`: TokenDailyAll / TaskListMetrics / TaskListMetricsAll).
// ---------------------------------------------------------------------------

/// `DailyTokenBucket`: one UTC calendar day's token aggregate across all tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DailyTokenBucket {
    pub day: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
}

/// `FindingSeverityCounts`: a task's finding count bucketed by severity (records
/// outside the severity whitelist are counted in no bucket).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FindingSeverityCounts {
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
}

/// `TaskListMetrics`: the aggregates rendered in task lists for one task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskListMetrics {
    pub tokens: TokenUsage,
    pub last_activity: i64,
    pub goals: GoalCounts,
    pub running_intents: i64,
    pub findings: FindingSeverityCounts,
}

/// `taskListMetricsQuery`: the select shared by `task_list_metrics_all` and
/// `task_list_metrics`; each appends its own WHERE on `task`. The lateral lookups
/// hit the per-exploration indexes instead of regrouping the whole history.
const TASK_LIST_METRICS_QUERY: &str = "
		SELECT task.exploration_id,
		       COALESCE(token_metrics.input_tokens,0),
		       COALESCE(token_metrics.output_tokens,0),
		       COALESCE(token_metrics.cache_read_tokens,0),
		       COALESCE(token_metrics.cache_write_tokens,0),
		       COALESCE(latest_activity.created_at,0),
		       COALESCE(goal_metrics.total,0),
		       COALESCE(goal_metrics.met,0),
		       COALESCE(intent_metrics.running,0),
		       COALESCE(finding_metrics.critical,0),
		       COALESCE(finding_metrics.high,0),
		       COALESCE(finding_metrics.medium,0),
		       COALESCE(finding_metrics.low,0)
		FROM tasks task
		LEFT JOIN LATERAL (
			SELECT SUM(input_tokens) AS input_tokens,
			       SUM(output_tokens) AS output_tokens,
			       SUM(cache_read_tokens) AS cache_read_tokens,
			       SUM(cache_write_tokens) AS cache_write_tokens
			FROM activity
			WHERE exploration_id=task.exploration_id AND kind='result'
		) token_metrics ON true
		LEFT JOIN LATERAL (
			SELECT EXTRACT(EPOCH FROM activity.created_at)::bigint AS created_at
			FROM activity
			WHERE activity.exploration_id=task.exploration_id
			ORDER BY activity.created_at DESC
			LIMIT 1
		) latest_activity ON true
		LEFT JOIN LATERAL (
			SELECT COUNT(*) AS total,
			       COUNT(*) FILTER (WHERE state='met') AS met
			FROM exploration_nodes
			WHERE exploration_id=task.exploration_id AND kind='goal'
		) goal_metrics ON true
		LEFT JOIN LATERAL (
			SELECT COUNT(*) AS running
			FROM exploration_nodes
			WHERE exploration_id=task.exploration_id AND kind='intent' AND state='running'
		) intent_metrics ON true
		LEFT JOIN LATERAL (
			SELECT COUNT(*) FILTER (WHERE severity='critical') AS critical,
			       COUNT(*) FILTER (WHERE severity='high')     AS high,
			       COUNT(*) FILTER (WHERE severity='medium')   AS medium,
			       COUNT(*) FILTER (WHERE severity='low')      AS low
			FROM findings
			WHERE task_id=task.id
		) finding_metrics ON true
		WHERE task.deleted_at IS NULL";

/// `scanTaskListMetrics`: read one row of [`TASK_LIST_METRICS_QUERY`] into
/// `(exploration_id, metrics)`.
fn scan_task_list_metrics(row: &tokio_postgres::Row) -> (i64, TaskListMetrics) {
    (
        row.get(0),
        TaskListMetrics {
            tokens: TokenUsage {
                worker: String::new(),
                input: row.get(1),
                output: row.get(2),
                cache_read: row.get(3),
                cache_write: row.get(4),
            },
            last_activity: row.get(5),
            goals: GoalCounts {
                total: row.get(6),
                met: row.get(7),
            },
            running_intents: row.get(8),
            findings: FindingSeverityCounts {
                critical: row.get(9),
                high: row.get(10),
                medium: row.get(11),
                low: row.get(12),
            },
        },
    )
}

impl DB {
    /// `TokenDailyAll`: aggregate token consumption by UTC calendar day for the
    /// past `days` days across all explorations (only `kind='result'` rows carry
    /// tokens). `days`<=0 defaults to 30.
    pub async fn token_daily_all(&self, days: i32) -> Result<Vec<DailyTokenBucket>> {
        let days = if days <= 0 { 30 } else { days };
        let rows = self
            .query(
                "SELECT TO_CHAR(DATE_TRUNC('day', created_at AT TIME ZONE 'UTC'), 'YYYY-MM-DD') AS day, \
             COALESCE(SUM(input_tokens), 0), \
             COALESCE(SUM(output_tokens), 0), \
             COALESCE(SUM(cache_read_tokens), 0) \
             FROM activity \
             WHERE kind = 'result' \
             AND created_at >= NOW() - ($1::int * INTERVAL '1 day') \
             GROUP BY day ORDER BY day",
                &[&days],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| DailyTokenBucket {
                day: r.get(0),
                input: r.get(1),
                output: r.get(2),
                cache_read: r.get(3),
            })
            .collect())
    }

    /// `TaskListMetricsAll`: list aggregates for every live (not soft-deleted)
    /// task in one query.
    pub async fn task_list_metrics_all(
        &self,
    ) -> Result<std::collections::HashMap<i64, TaskListMetrics>> {
        let rows = self.query(TASK_LIST_METRICS_QUERY, &[]).await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            let (eid, metrics) = scan_task_list_metrics(r);
            out.insert(eid, metrics);
        }
        Ok(out)
    }

    /// `TaskListMetrics`: the same aggregates for the one live task of exploration
    /// `exp_id`. A missing/deleted task gives zero metrics (like a missing key of
    /// [`task_list_metrics_all`](Self::task_list_metrics_all)).
    pub async fn task_list_metrics(&self, exp_id: i64) -> Result<TaskListMetrics> {
        let sql = format!("{TASK_LIST_METRICS_QUERY} AND task.exploration_id=$1");
        let row = self.query_opt(&sql, &[&exp_id]).await?;
        Ok(row
            .as_ref()
            .map(scan_task_list_metrics)
            .map(|(_, m)| m)
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Activity stream (port of the `*ExplorationStore` activity methods in
// `db/exploration.go`). `Activity` is the worker-step record; the read methods
// scan different column subsets into it, leaving the rest at defaults.
// ---------------------------------------------------------------------------

/// `Activity`: one worker step in the exploration's activity log. The read methods
/// fill only the columns they select (list/page include metadata + main_seg; the
/// terminal/inherited variants drop metadata and main_seg; traces keep summaries
/// only; by-ids fill the lazy `detail` blob).
#[derive(Debug, Clone)]
pub struct Activity {
    pub id: i64,
    pub node_id: Option<i64>,
    pub worker: String,
    pub kind: String,
    pub tool: String,
    pub tool_use_id: String,
    pub is_error: bool,
    pub summary: String,
    pub detail: String,
    pub metadata: Value,
    pub created_at: SystemTime,
    // Token usage — set only on the terminal kind='result' record; None elsewhere.
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cache_read_tokens: Option<i32>,
    pub cache_write_tokens: Option<i32>,
    pub source_task_id: i64,
    pub inherited: bool,
    // MainSeg is the main-agent conversation segment (None for non-mainagent rows).
    pub main_seg: Option<i32>,
}

impl Default for Activity {
    fn default() -> Self {
        Activity {
            id: 0,
            node_id: None,
            worker: String::new(),
            kind: String::new(),
            tool: String::new(),
            tool_use_id: String::new(),
            is_error: false,
            summary: String::new(),
            detail: String::new(),
            metadata: Value::Null,
            created_at: UNIX_EPOCH,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            source_task_id: 0,
            inherited: false,
            main_seg: None,
        }
    }
}

impl Activity {
    /// Constructor for an append with all builder-style fields defaulted.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: Option<i64>,
        worker: &str,
        kind: &str,
        tool: &str,
        tool_use_id: &str,
        is_error: bool,
        summary: &str,
        detail: &str,
        metadata: Value,
        input_tokens: Option<i32>,
        output_tokens: Option<i32>,
        cache_read_tokens: Option<i32>,
        cache_write_tokens: Option<i32>,
        main_seg: Option<i32>,
    ) -> Activity {
        Activity {
            id: 0,
            node_id,
            worker: worker.to_string(),
            kind: kind.to_string(),
            tool: tool.to_string(),
            tool_use_id: tool_use_id.to_string(),
            is_error,
            summary: summary.to_string(),
            detail: detail.to_string(),
            metadata,
            created_at: UNIX_EPOCH,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            source_task_id: 0,
            inherited: false,
            main_seg,
        }
    }
}

/// `utf8Clean`: strip NUL bytes (Rust strings are already valid UTF-8, so Go's
/// `ToValidUTF8` pass is a no-op here; PostgreSQL text still rejects a NUL).
pub(crate) fn utf8_clean(s: &str) -> String {
    s.replace('\0', "")
}

/// `scanActivity` (full list/page column set: 15 cols incl. metadata + main_seg).
fn scan_activity_full(row: &tokio_postgres::Row) -> Activity {
    Activity {
        id: row.get(0),
        node_id: row.get(1),
        worker: row.get(2),
        kind: row.get(3),
        tool: row.get(4),
        tool_use_id: row.get(5),
        is_error: row.get(6),
        summary: row.get(7),
        metadata: row.get(8),
        created_at: row.get(9),
        input_tokens: row.get(10),
        output_tokens: row.get(11),
        cache_read_tokens: row.get(12),
        cache_write_tokens: row.get(13),
        main_seg: row.get(14),
        ..Default::default()
    }
}

/// `scanActivity` (terminal/inherited set: 13 cols, no metadata / main_seg).
fn scan_activity_terminal(row: &tokio_postgres::Row) -> Activity {
    Activity {
        id: row.get(0),
        node_id: row.get(1),
        worker: row.get(2),
        kind: row.get(3),
        tool: row.get(4),
        tool_use_id: row.get(5),
        is_error: row.get(6),
        summary: row.get(7),
        created_at: row.get(8),
        input_tokens: row.get(9),
        output_tokens: row.get(10),
        cache_read_tokens: row.get(11),
        cache_write_tokens: row.get(12),
        ..Default::default()
    }
}

/// `scanActivity` (by-ids detail set: 6 cols incl. detail, no summary).
fn scan_activity_detail(row: &tokio_postgres::Row) -> Activity {
    Activity {
        id: row.get(0),
        node_id: row.get(1),
        kind: row.get(2),
        tool: row.get(3),
        is_error: row.get(4),
        detail: row.get(5),
        ..Default::default()
    }
}

const ACTIVITY_COLS_FULL: &str = "id, node_id, COALESCE(worker,''), COALESCE(kind,''), \
    COALESCE(tool,''), COALESCE(tool_use_id,''), is_error, COALESCE(summary,''), metadata, \
    created_at, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, main_seg";

impl ExplorationStore {
    /// `AppendActivity`: record one worker step and return its id. Empty text
    /// fields become NULL (`NULLIF(x,'')`); empty metadata becomes `{}`.
    pub async fn append_activity(&self, a: Activity) -> Result<i64> {
        let metadata = if a.metadata.is_null() {
            serde_json::json!({})
        } else {
            a.metadata
        };
        let row = self
            .db
            .query_one(
                "INSERT INTO activity(exploration_id, node_id, worker, kind, tool, tool_use_id, \
             is_error, summary, detail, metadata, input_tokens, output_tokens, \
             cache_read_tokens, cache_write_tokens, main_seg) \
             VALUES ($1,$2,NULLIF($3,''),NULLIF($4,''),NULLIF($5,''),NULLIF($6,''),$7,\
             NULLIF($8,''),NULLIF($9,''),$10,$11,$12,$13,$14,$15) RETURNING id",
                &[
                    &self.exp_id,
                    &a.node_id,
                    &utf8_clean(&a.worker),
                    &utf8_clean(&a.kind),
                    &utf8_clean(&a.tool),
                    &utf8_clean(&a.tool_use_id),
                    &a.is_error,
                    &utf8_clean(&a.summary),
                    &utf8_clean(&a.detail),
                    &metadata,
                    &a.input_tokens,
                    &a.output_tokens,
                    &a.cache_read_tokens,
                    &a.cache_write_tokens,
                    &a.main_seg,
                ],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `ActivityList`: steps after `since_id` (exclusive), optionally filtered by
    /// node. Returns the items and the new cursor (max id seen, >= since_id).
    pub async fn activity_list(
        &self,
        node_id: Option<i64>,
        since_id: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, i64)> {
        let limit = (if limit <= 0 { 300 } else { limit }) as i64;
        let sql = if node_id.is_some() {
            format!(
                "SELECT {ACTIVITY_COLS_FULL} FROM activity \
             WHERE exploration_id=$1 AND node_id=$2 AND id>$3 ORDER BY id LIMIT $4"
            )
        } else {
            format!(
                "SELECT {ACTIVITY_COLS_FULL} FROM activity \
             WHERE exploration_id=$1 AND id>$2 ORDER BY id LIMIT $3"
            )
        };
        let rows = if let Some(nid) = node_id {
            self.db
                .query(&sql, &[&self.exp_id, &nid, &since_id, &limit])
                .await?
        } else {
            self.db
                .query(&sql, &[&self.exp_id, &since_id, &limit])
                .await?
        };
        let mut out = Vec::with_capacity(rows.len());
        let mut cursor = since_id;
        for r in &rows {
            let a = scan_activity_full(r);
            if a.id > cursor {
                cursor = a.id;
            }
            out.push(a);
        }
        Ok((out, cursor))
    }

    /// `ActivityListForTerminalIntent`: the inherited incremental-read boundary —
    /// the intent state check and the read happen in one statement, so a source
    /// intent reopened between API-level checks cannot expose its new in-flight
    /// trace. Model reasoning (`thinking`) and accounting (`usage`) rows are never
    /// inherited.
    pub async fn activity_list_for_terminal_intent(
        &self,
        node_id: i64,
        since_id: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, i64)> {
        let limit = (if limit <= 0 { 300 } else { limit }) as i64;
        let rows = self
            .db
            .query(
                "SELECT a.id, a.node_id, COALESCE(a.worker,''), COALESCE(a.kind,''), \
             COALESCE(a.tool,''), COALESCE(a.tool_use_id,''), a.is_error, COALESCE(a.summary,''), \
             a.created_at, a.input_tokens, a.output_tokens, a.cache_read_tokens, a.cache_write_tokens \
             FROM activity a \
             JOIN exploration_nodes n ON n.id=a.node_id AND n.exploration_id=a.exploration_id \
             WHERE a.exploration_id=$1 AND a.node_id=$2 AND a.id>$3 \
             AND n.kind='intent' AND n.state IN ('done','blocked','exhausted','stopped') \
             AND a.kind NOT IN ('thinking','usage') \
             ORDER BY a.id LIMIT $4",
                &[&self.exp_id, &node_id, &since_id, &limit],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        let mut cursor = since_id;
        for r in &rows {
            let a = scan_activity_terminal(r);
            if a.id > cursor {
                cursor = a.id;
            }
            out.push(a);
        }
        Ok((out, cursor))
    }

    /// `ActivityMaxID`: the task's current maximum activity id (0 when empty). The
    /// task-level snapshot cursor handed to the SSE stream so history (id<=cursor)
    /// and the live tail (id>cursor) meet with no gap.
    pub async fn activity_max_id(&self) -> Result<i64> {
        let row = self
            .db
            .query_one(
                "SELECT MAX(id) FROM activity WHERE exploration_id=$1",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get::<_, Option<i64>>(0).unwrap_or(0))
    }

    /// `ActivityDetail`: lazily return the full detail blob for one step ("" when
    /// the row is missing or detail is NULL).
    pub async fn activity_detail(&self, id: i64) -> Result<String> {
        let row = self
            .db
            .query_opt(
                "SELECT detail FROM activity WHERE id=$1 AND exploration_id=$2",
                &[&id, &self.exp_id],
            )
            .await?;
        Ok(row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .unwrap_or_default())
    }

    /// `ActivityByIDs`: load full detail for specific step ids (the trace
    /// drill-down), scoped to this exploration. `thinking` rows are skipped. Order
    /// is ascending id, not input order. Empty input -> empty.
    pub async fn activity_by_ids(&self, ids: &[i64]) -> Result<Vec<Activity>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .db
            .query(
                "SELECT id, node_id, COALESCE(kind,''), COALESCE(tool,''), is_error, \
             COALESCE(detail,'') FROM activity \
             WHERE exploration_id=$1 AND kind<>'thinking' AND id = ANY($2::bigint[]) ORDER BY id",
                &[&self.exp_id, &ids],
            )
            .await?;
        Ok(rows.iter().map(scan_activity_detail).collect())
    }

    /// `ActivityByIDsForTerminalIntents`: the inherited-detail read boundary —
    /// excludes node-less planner/main rows, non-intent activities, live source
    /// work, and thinking/usage. Local callers needing legacy behavior use
    /// [`activity_by_ids`](Self::activity_by_ids).
    pub async fn activity_by_ids_for_terminal_intents(&self, ids: &[i64]) -> Result<Vec<Activity>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .db
            .query(
                "SELECT a.id, a.node_id, COALESCE(a.kind,''), COALESCE(a.tool,''), \
             a.is_error, COALESCE(a.detail,'') \
             FROM activity a \
             JOIN exploration_nodes n ON n.id=a.node_id AND n.exploration_id=a.exploration_id \
             WHERE a.exploration_id=$1 AND a.kind NOT IN ('thinking','usage') AND n.kind='intent' \
             AND n.state IN ('done','blocked','exhausted','stopped') \
             AND a.id = ANY($2::bigint[]) ORDER BY a.id",
                &[&self.exp_id, &ids],
            )
            .await?;
        Ok(rows.iter().map(scan_activity_detail).collect())
    }

    /// `ActivityPage`: one page for reverse (newest-first) pagination of a session
    /// — up to `limit` steps ending before id `before` (exclusive; before<=0 = the
    /// latest page), returned in ASCENDING id order for direct display. The bool
    /// reports whether still-older steps exist (so a client can stop on scroll-up).
    /// `node_id` == Some filters to one work; None matches the whole task.
    #[allow(clippy::too_many_arguments)]
    pub async fn activity_page(
        &self,
        node_id: Option<i64>,
        main: bool,
        main_seg: Option<i32>,
        worker: &str,
        before: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, bool)> {
        let limit = (if limit <= 0 { 200 } else { limit }) as i64;
        // Hoist owned bind values so they outlive the param slice.
        let node_owned: i64 = node_id.unwrap_or(0);
        let seg_owned: i32 = main_seg.unwrap_or(0);
        // Build the session-filter condition (exactly one field is meaningful).
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&self.exp_id];
        let mut cond = String::new();
        if node_id.is_some() {
            params.push(&node_owned);
            cond = format!(" AND node_id=${}", params.len());
        } else if main {
            if seg_owned == 0 {
                cond = " AND worker='mainagent' AND COALESCE(main_seg,0)=0".to_string();
            } else {
                params.push(&seg_owned);
                cond = format!(" AND worker='mainagent' AND main_seg=${}", params.len());
            }
        } else if !worker.is_empty() {
            params.push(&worker);
            cond = format!(" AND worker=${}", params.len());
        }
        let before_arg = params.len() + 1;
        params.push(&before);
        let limit1 = limit + 1; // one extra row to detect older history
        let limit_arg = params.len() + 1;
        params.push(&limit1);
        let sql = format!(
            "SELECT {ACTIVITY_COLS_FULL} FROM activity WHERE exploration_id=$1{cond} \
         AND (${before_arg} <= 0::bigint OR id < ${before_arg}) \
         ORDER BY id DESC LIMIT ${limit_arg}"
        );
        let rows = self.db.query(&sql, &params).await?;
        let mut desc: Vec<Activity> = rows.iter().map(scan_activity_full).collect();
        let has_more = desc.len() > limit as usize;
        if has_more {
            desc.truncate(limit as usize);
        }
        // Reverse the newest-first window into ascending id order for display.
        desc.reverse();
        Ok((desc, has_more))
    }

    /// `ActivityPageForTerminalIntent`: the inherited session-history boundary —
    /// ownership, terminal-state and row-kind checks in one query, closing the race
    /// where a completed source intent is reopened before paging.
    pub async fn activity_page_for_terminal_intent(
        &self,
        node_id: i64,
        before: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, bool)> {
        let limit = (if limit <= 0 { 200 } else { limit }) as i64;
        let limit1 = limit + 1;
        let rows = self
            .db
            .query(
                "SELECT a.id, a.node_id, COALESCE(a.worker,''), COALESCE(a.kind,''), \
             COALESCE(a.tool,''), COALESCE(a.tool_use_id,''), a.is_error, COALESCE(a.summary,''), \
             a.created_at, a.input_tokens, a.output_tokens, a.cache_read_tokens, a.cache_write_tokens \
             FROM activity a \
             JOIN exploration_nodes n ON n.id=a.node_id AND n.exploration_id=a.exploration_id \
             WHERE a.exploration_id=$1 AND a.node_id=$2 \
             AND n.kind='intent' AND n.state IN ('done','blocked','exhausted','stopped') \
             AND a.kind NOT IN ('thinking','usage') \
             AND ($3 <= 0::bigint OR a.id < $3) \
             ORDER BY a.id DESC LIMIT $4",
                &[&self.exp_id, &node_id, &before, &limit1],
            )
            .await?;
        let mut desc: Vec<Activity> = rows.iter().map(scan_activity_terminal).collect();
        let has_more = desc.len() > limit as usize;
        if has_more {
            desc.truncate(limit as usize);
        }
        desc.reverse();
        Ok((desc, has_more))
    }
}

// ---------------------------------------------------------------------------
// Activity trace + search (port of the `*ExplorationStore` trace methods in
// `db/exploration.go`). These return summary-only rows (no detail/metadata) for
// the worker-trace tools; detail is fetched separately via `activity_by_ids`.
// ---------------------------------------------------------------------------

const TRACE_COLS: &str = "id, node_id, COALESCE(worker,''), COALESCE(kind,''), \
    COALESCE(tool,''), is_error, COALESCE(summary,'')";

/// `scanTrace`: scan a batch of summary-only rows on [`TRACE_COLS`].
fn scan_trace(rows: &[tokio_postgres::Row]) -> Vec<Activity> {
    rows.iter()
        .map(|r| Activity {
            id: r.get(0),
            node_id: r.get(1),
            worker: r.get(2),
            kind: r.get(3),
            tool: r.get(4),
            is_error: r.get(5),
            summary: r.get(6),
            ..Default::default()
        })
        .collect()
}

impl ExplorationStore {
    /// `ActivityTrace`: list one work's steps (by intent/node id) for the trace
    /// tools — summaries only. `thinking`/`usage` rows are dropped so the caller
    /// sees the work's actions + observations, not the model's internal reasoning
    /// or token-accounting noise.
    pub async fn activity_trace(&self, node_id: i64, limit: i32) -> Result<Vec<Activity>> {
        let limit = (if limit <= 0 { 500 } else { limit }) as i64;
        let rows = self
            .db
            .query(
                &format!(
                    "SELECT {TRACE_COLS} FROM activity \
                 WHERE exploration_id=$1 AND node_id=$2 \
                 AND kind NOT IN ('thinking','usage') \
                 ORDER BY id LIMIT $3"
                ),
                &[&self.exp_id, &node_id, &limit],
            )
            .await?;
        Ok(scan_trace(&rows))
    }

    /// `ActivityTraceForTerminalIntent`: one inherited work trace only while its
    /// source intent is still terminal at query time.
    pub async fn activity_trace_for_terminal_intent(
        &self,
        node_id: i64,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let limit = (if limit <= 0 { 500 } else { limit }) as i64;
        let rows = self
            .db
            .query(
                "SELECT a.id, a.node_id, COALESCE(a.worker,''), COALESCE(a.kind,''), \
             COALESCE(a.tool,''), a.is_error, COALESCE(a.summary,'') \
             FROM activity a \
             JOIN exploration_nodes n ON n.id=a.node_id AND n.exploration_id=a.exploration_id \
             WHERE a.exploration_id=$1 AND a.node_id=$2 AND n.kind='intent' \
             AND n.state IN ('done','blocked','exhausted','stopped') \
             AND a.kind NOT IN ('thinking','usage') \
             ORDER BY a.id LIMIT $3",
                &[&self.exp_id, &node_id, &limit],
            )
            .await?;
        Ok(scan_trace(&rows))
    }

    /// `ActivityTraceSearch`: find steps whose summary OR detail matches `q`
    /// (case-insensitive), returning summaries only. `node_id` == Some scopes to
    /// one work; None searches across ALL works (node_id IS NOT NULL — planner/
    /// main steps have no node_id, so this cleanly means "worker traces").
    pub async fn activity_trace_search(
        &self,
        node_id: Option<i64>,
        q: &str,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let limit = (if limit <= 0 { 100 } else { limit }) as i64;
        let like = format!("%{q}%");
        let rows = if let Some(nid) = node_id {
            self.db
                .query(
                    &format!(
                        "SELECT {TRACE_COLS} FROM activity \
                     WHERE exploration_id=$1 AND node_id=$2 \
                     AND kind NOT IN ('thinking','usage') \
                     AND (summary ILIKE $3 OR detail ILIKE $3) ORDER BY id LIMIT $4"
                    ),
                    &[&self.exp_id, &nid, &like, &limit],
                )
                .await?
        } else {
            self.db
                .query(
                    &format!(
                        "SELECT {TRACE_COLS} FROM activity \
                     WHERE exploration_id=$1 AND node_id IS NOT NULL \
                     AND kind NOT IN ('thinking','usage') \
                     AND (summary ILIKE $2 OR detail ILIKE $2) ORDER BY id LIMIT $3"
                    ),
                    &[&self.exp_id, &like, &limit],
                )
                .await?
        };
        Ok(scan_trace(&rows))
    }

    /// `ActivityTraceSearchForTerminalIntent`: the scoped inherited search variant;
    /// terminal eligibility is evaluated by the same statement that reads the rows.
    pub async fn activity_trace_search_for_terminal_intent(
        &self,
        node_id: i64,
        q: &str,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let limit = (if limit <= 0 { 100 } else { limit }) as i64;
        let like = format!("%{q}%");
        let rows = self
            .db
            .query(
                "SELECT a.id, a.node_id, COALESCE(a.worker,''), COALESCE(a.kind,''), \
             COALESCE(a.tool,''), a.is_error, COALESCE(a.summary,'') \
             FROM activity a \
             JOIN exploration_nodes n ON n.id=a.node_id AND n.exploration_id=a.exploration_id \
             WHERE a.exploration_id=$1 AND a.node_id=$2 AND n.kind='intent' \
             AND n.state IN ('done','blocked','exhausted','stopped') \
             AND a.kind NOT IN ('thinking','usage') \
             AND (a.summary ILIKE $3 OR a.detail ILIKE $3) \
             ORDER BY a.id LIMIT $4",
                &[&self.exp_id, &node_id, &like, &limit],
            )
            .await?;
        Ok(scan_trace(&rows))
    }

    /// `ActivityTraceSearchTerminalIntents`: search inherited worker history
    /// without exposing planner/main rows or work that is still open/running.
    pub async fn activity_trace_search_terminal_intents(
        &self,
        q: &str,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let limit = (if limit <= 0 { 100 } else { limit }) as i64;
        let like = format!("%{q}%");
        let rows = self
            .db
            .query(
                "SELECT a.id, a.node_id, COALESCE(a.worker,''), COALESCE(a.kind,''), \
             COALESCE(a.tool,''), a.is_error, COALESCE(a.summary,'') \
             FROM activity a \
             JOIN exploration_nodes n ON n.id=a.node_id AND n.exploration_id=a.exploration_id \
             WHERE a.exploration_id=$1 AND n.kind='intent' \
             AND n.state IN ('done','blocked','exhausted','stopped') \
             AND a.kind NOT IN ('thinking','usage') \
             AND (a.summary ILIKE $2 OR a.detail ILIKE $2) \
             ORDER BY a.id LIMIT $3",
                &[&self.exp_id, &like, &limit],
            )
            .await?;
        Ok(scan_trace(&rows))
    }

    /// `ActivityTraceSearchExcluding`: keyword-search every node's steps in this
    /// exploration EXCEPT one node's own (`exclude_node_id`) — so a worker's
    /// `search_all_worker_traces` doesn't return its own in-progress trace (which
    /// is already in its context). `exclude_node_id`<=0 -> no exclusion.
    pub async fn activity_trace_search_excluding(
        &self,
        exclude_node_id: i64,
        q: &str,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        if exclude_node_id <= 0 {
            return self.activity_trace_search(None, q, limit).await;
        }
        let limit = (if limit <= 0 { 100 } else { limit }) as i64;
        let like = format!("%{q}%");
        let rows = self
            .db
            .query(
                &format!(
                    "SELECT {TRACE_COLS} FROM activity \
                 WHERE exploration_id=$1 AND node_id IS NOT NULL AND node_id <> $2 \
                 AND kind NOT IN ('thinking','usage') \
                 AND (summary ILIKE $3 OR detail ILIKE $3) ORDER BY id LIMIT $4"
                ),
                &[&self.exp_id, &exclude_node_id, &like, &limit],
            )
            .await?;
        Ok(scan_trace(&rows))
    }
}

// ---------------------------------------------------------------------------
// Main sessions + asset refs + standalone findings (port of the remaining
// `*ExplorationStore` methods in `db/exploration.go`).
// ---------------------------------------------------------------------------

/// `MainSession`: one resettable main-agent conversation segment. Segment 0 is the
/// implicit original session (never stored); further segments are created by
/// "New conversation" to start the main agent on a clean transcript while the
/// task's graph, assets and goal stay shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainSession {
    pub seq: i32,
    pub created_at: SystemTime,
}

/// `AssetRef`: a compact exploration node (intent/fact/finding) that references an
/// asset via an anchor — for the coverage graph's node drawer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRef {
    pub id: i64,
    pub kind: String,
    pub state: String,
    pub summary: String,
    pub source_task_id: i64,
    pub inherited: bool,
}

impl ExplorationStore {
    /// `CurrentMainSeg`: the highest main-session segment (0 when none created yet).
    pub async fn current_main_seg(&self) -> Result<i32> {
        let row = self
            .db
            .query_one(
                "SELECT COALESCE(MAX(seq),0) FROM main_sessions WHERE exploration_id=$1",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `ListMainSessions`: every main-session segment newest-first, always
    /// including the implicit original segment 0. Segment 0 carries no stored
    /// timestamp (defaults to UNIX_EPOCH).
    pub async fn list_main_sessions(&self) -> Result<Vec<MainSession>> {
        let rows = self
            .db
            .query(
                "SELECT seq, created_at FROM main_sessions WHERE exploration_id=$1 ORDER BY seq DESC",
                &[&self.exp_id],
            )
            .await?;
        let mut out: Vec<MainSession> = rows
            .iter()
            .map(|r| MainSession {
                seq: r.get(0),
                created_at: r.get(1),
            })
            .collect();
        // Implicit original session (oldest, no stored timestamp).
        out.push(MainSession {
            seq: 0,
            created_at: UNIX_EPOCH,
        });
        Ok(out)
    }

    /// `NewMainSession`: create the next main-session segment (seq = current+1) and
    /// return it. Touches only `main_sessions` — the task's graph/assets/goal are
    /// untouched, so the new session starts clean over the same task.
    pub async fn new_main_session(&self) -> Result<MainSession> {
        let row = self
            .db
            .query_one(
                "INSERT INTO main_sessions(exploration_id, seq) \
             VALUES ($1, COALESCE((SELECT MAX(seq) FROM main_sessions WHERE exploration_id=$1),0)+1) \
             RETURNING seq, created_at",
                &[&self.exp_id],
            )
            .await?;
        Ok(MainSession {
            seq: row.get(0),
            created_at: row.get(1),
        })
    }

    /// `AssetRefs`: the intents / facts / findings in THIS exploration anchored to
    /// the given asset id (newest first) — what this task tested / concluded about
    /// it. `asset_id`<=0 -> empty.
    pub async fn asset_refs(&self, asset_id: i64) -> Result<Vec<AssetRef>> {
        if asset_id <= 0 {
            return Ok(Vec::new());
        }
        let rows = self
            .db
            .query(
                "SELECT en.id, en.kind, en.state, en.payload \
             FROM exploration_anchors ea \
             JOIN exploration_nodes en ON en.id = ea.node_id \
             WHERE ea.asset_id = $1 AND en.exploration_id = $2 \
             AND en.kind IN ('intent','fact','finding') \
             ORDER BY en.id DESC",
                &[&asset_id, &self.exp_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let payload: Value = r.get(3);
                let summary = ["summary", "text"]
                    .iter()
                    .find_map(|k| {
                        payload
                            .get(*k)
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.trim().is_empty())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();
                AssetRef {
                    id: r.get(0),
                    kind: r.get(1),
                    state: r.get(2),
                    summary,
                    source_task_id: 0,
                    inherited: false,
                }
            })
            .collect())
    }

    /// `AddStandaloneFinding`: write a finding to the standalone findings table,
    /// which persists across task deletion (`task_id` / `node_id` become NULL when
    /// the task or exploration node is deleted). `task_id` and `node_id` may be 0
    /// (stored as NULL).
    #[allow(clippy::too_many_arguments)]
    pub async fn add_standalone_finding(
        &self,
        task_id: i64,
        node_id: i64,
        vulnclass: &str,
        name: &str,
        severity: &str,
        summary: &str,
        evidence: &str,
        worker: &str,
        asset_ids: &[i64],
    ) -> Result<i64> {
        self.db
            .add_finding(
                task_id, node_id, vulnclass, name, severity, summary, evidence, worker, asset_ids,
            )
            .await
    }
}

impl DB {
    /// `AddFinding`: insert a finding row (the standalone findings table). `task_id`
    /// and `node_id` <= 0 are stored as NULL. `asset_ids` is serialised as a JSONB
    /// array.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_finding(
        &self,
        task_id: i64,
        node_id: i64,
        vulnclass: &str,
        name: &str,
        severity: &str,
        summary: &str,
        evidence: &str,
        worker: &str,
        asset_ids: &[i64],
    ) -> Result<i64> {
        let tid: Option<i64> = if task_id > 0 { Some(task_id) } else { None };
        let nid: Option<i64> = if node_id > 0 { Some(node_id) } else { None };
        let aids = serde_json::json!(asset_ids);
        let row = self
            .query_one(
                "INSERT INTO findings \
             (task_id, node_id, vulnclass, name, severity, summary, evidence, worker, asset_ids) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
                &[
                    &tid, &nid, &vulnclass, &name, &severity, &summary, &evidence, &worker, &aids,
                ],
            )
            .await?;
        Ok(row.get(0))
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

// ---------------------------------------------------------------------------
// Cold-digest persistence (port of `db/digest.go`: BumpRound / RoundNo /
// ColdStamps / ApplyStampOps / ContentVersions / ActiveDigests / AddDigest /
// CoveredMembers / DigestMembers / SupersedeDigests / NodeAssets). These are the
// round counter, cold_since_round stamps, content versions, digest nodes and the
// covers edges that are the source of truth for "which digest folds node X".
// ---------------------------------------------------------------------------

/// `StampOp`: one `cold_since_round` change — `set` stamps `round`; `!set` clears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampOp {
    pub id: i64,
    pub set: bool,
    pub round: i64,
}

impl ExplorationStore {
    /// `BumpRound`: advance this exploration's planner-round counter, returning the
    /// new value (called once per planner wake-up).
    pub async fn bump_round(&self) -> Result<i64> {
        let row = self
            .db
            .query_one(
                "UPDATE explorations SET round_no = round_no + 1 WHERE id=$1 RETURNING round_no",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `RoundNo`: the current planner-round counter.
    pub async fn round_no(&self) -> Result<i64> {
        let row = self
            .db
            .query_one(
                "SELECT round_no FROM explorations WHERE id=$1",
                &[&self.exp_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `ColdStamps`: `cold_since_round` for every foldable node (intent/fact) as
    /// `id -> Some(round)` or `id -> None` (hot / unstamped). Used for the ≥R
    /// debounce and to know which stamps to set/clear this round.
    pub async fn cold_stamps(&self) -> Result<std::collections::HashMap<i64, Option<i64>>> {
        let rows = self
            .db
            .query(
                "SELECT id, cold_since_round FROM exploration_nodes \
             WHERE exploration_id=$1 AND kind IN ('intent','fact')",
                &[&self.exp_id],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            out.insert(r.get::<_, i64>(0), r.get::<_, Option<i64>>(1));
        }
        Ok(out)
    }

    /// `ApplyStampOps`: write a batch of `cold_since_round` changes in one
    /// transaction. Empty input is a no-op.
    pub async fn apply_stamp_ops(&self, ops: &[StampOp]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        for o in ops {
            if o.set {
                tx.execute(
                    "UPDATE exploration_nodes SET cold_since_round=$1 \
                 WHERE id=$2 AND exploration_id=$3",
                    &[&o.round, &o.id, &self.exp_id],
                )
                .await?;
            } else {
                tx.execute(
                    "UPDATE exploration_nodes SET cold_since_round=NULL \
                 WHERE id=$1 AND exploration_id=$2",
                    &[&o.id, &self.exp_id],
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// `ContentVersions`: `id -> content_version` for all nodes (the §5.3 digest
    /// signature).
    pub async fn content_versions(&self) -> Result<std::collections::HashMap<i64, i32>> {
        let rows = self
            .db
            .query(
                "SELECT id, content_version FROM exploration_nodes WHERE exploration_id=$1",
                &[&self.exp_id],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            out.insert(r.get::<_, i64>(0), r.get::<_, i32>(1));
        }
        Ok(out)
    }

    /// `ActiveDigests`: the exploration's live digest nodes (`state='active'`),
    /// oldest first.
    pub async fn active_digests(&self) -> Result<Vec<Node>> {
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes \
         WHERE exploration_id=$1 AND kind=$2 AND state=$3 ORDER BY id"
        );
        let rows = self
            .db
            .query(&sql, &[&self.exp_id, &KIND_DIGEST, &STATE_DIGEST_ACTIVE])
            .await?;
        Ok(scan_nodes(&rows))
    }

    /// `AddDigest`: write one digest node and its covers edges (digest->member) in
    /// a single transaction, returning the new digest id. `payload` is the digest
    /// body + member_ids + generation + signature (cold-digest §1).
    pub async fn add_digest(&self, payload: &Value, member_ids: &[i64]) -> Result<i64> {
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        let row = tx
            .query_one(
                "INSERT INTO exploration_nodes(exploration_id, kind, payload, priority, state, origin) \
             VALUES ($1, $2, $3, 0, $4, 'compactor') RETURNING id",
                &[&self.exp_id, &KIND_DIGEST, &payload, &STATE_DIGEST_ACTIVE],
            )
            .await?;
        let id: i64 = row.get(0);
        for m in member_ids {
            if *m == id {
                continue;
            }
            tx.execute(
                "INSERT INTO exploration_edges(exploration_id, src_id, rel, dst_id) \
             VALUES ($1,$2,$3,$4) ON CONFLICT (exploration_id, src_id, rel, dst_id) DO NOTHING",
                &[&self.exp_id, &id, &REL_COVERS, m],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    /// `CoveredMembers`: `member id -> covering digest id` for ACTIVE digests
    /// only. A node folded by two digests (a torn major write) resolves to the
    /// lowest digest id deterministically.
    pub async fn covered_members(&self) -> Result<std::collections::HashMap<i64, i64>> {
        let rows = self
            .db
            .query(
                "SELECT e.dst_id, e.src_id \
             FROM exploration_edges e \
             JOIN exploration_nodes d ON d.id=e.src_id AND d.exploration_id=e.exploration_id \
             WHERE e.exploration_id=$1 AND e.rel=$2 AND d.kind=$3 AND d.state=$4 \
             ORDER BY e.src_id",
                &[
                    &self.exp_id,
                    &REL_COVERS,
                    &KIND_DIGEST,
                    &STATE_DIGEST_ACTIVE,
                ],
            )
            .await?;
        let mut out = std::collections::HashMap::new();
        for r in &rows {
            let member: i64 = r.get(0);
            let digest: i64 = r.get(1);
            // First (lowest digest id) wins.
            out.entry(member).or_insert(digest);
        }
        Ok(out)
    }

    /// `DigestMembers`: the member ids a digest covers (covers edges), sorted.
    pub async fn digest_members(&self, digest_id: i64) -> Result<Vec<i64>> {
        let rows = self
            .db
            .query(
                "SELECT dst_id FROM exploration_edges \
             WHERE exploration_id=$1 AND src_id=$2 AND rel=$3",
                &[&self.exp_id, &digest_id, &REL_COVERS],
            )
            .await?;
        let mut ids: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
        ids.sort_unstable();
        Ok(ids)
    }

    /// `SupersedeDigests`: retire digest nodes (`state->superseded`) AND remove
    /// their covers edges atomically, so the active-coverage set never double-counts
    /// a member during a major recompaction. The digest node itself is kept.
    pub async fn supersede_digests(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        for id in ids {
            tx.execute(
                "DELETE FROM exploration_edges WHERE exploration_id=$1 AND src_id=$2 AND rel=$3",
                &[&self.exp_id, id, &REL_COVERS],
            )
            .await?;
            tx.execute(
                "UPDATE exploration_nodes SET state=$1 \
             WHERE id=$2 AND exploration_id=$3 AND kind=$4",
                &[&STATE_DIGEST_SUPERSEDED, id, &self.exp_id, &KIND_DIGEST],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// `NodeAssets`: `node id -> asset ids it is anchored to` for the given node
    /// ids (used to group cold digests by asset, §6.2). Empty input -> empty.
    pub async fn node_assets(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<i64>>> {
        let mut out: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let rows = self
            .db
            .query(
                "SELECT a.node_id, a.asset_id \
             FROM exploration_anchors a \
             JOIN exploration_nodes n ON n.id=a.node_id \
             WHERE n.exploration_id=$1 AND a.node_id = ANY($2::bigint[])",
                &[&self.exp_id, &ids],
            )
            .await?;
        for r in &rows {
            let node: i64 = r.get(0);
            let asset: i64 = r.get(1);
            out.entry(node).or_default().push(asset);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Admission rollback + operator constraints (port of `db/intent_admission.go`
// `DiscardOpenIntent` and `db/constraints.go`: Constraint / ListConstraints /
// AddConstraint / UpdateConstraint / DeleteConstraint). `created_at` is carried as
// RFC3339 text (`::text`), matching the other struct ports.
// ---------------------------------------------------------------------------

/// `Constraint`: one operator-authored operation constraint for a task
/// (`kind=allow` permitted ops / `kind=deny` forbidden ops, free text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Constraint {
    pub id: i64,
    pub kind: String,
    pub text: String,
    pub origin: String,
    pub created_at: String,
}

impl ExplorationStore {
    /// `DiscardOpenIntent`: compensate a follow-up creation when task admission
    /// fails. Deletes the node's activity, then the open intent node itself;
    /// errors unless exactly one open intent was removed. Edges/anchors cascade.
    pub async fn discard_open_intent(&self, id: i64) -> Result<()> {
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "DELETE FROM activity WHERE exploration_id=$1 AND node_id=$2",
            &[&self.exp_id, &id],
        )
        .await?;
        let removed = tx
            .execute(
                "DELETE FROM exploration_nodes \
             WHERE id=$1 AND exploration_id=$2 AND kind='intent' AND state='open'",
                &[&id, &self.exp_id],
            )
            .await?;
        if removed != 1 {
            return Err(Error::msg(format!(
                "open intent {id} was not available for admission rollback"
            )));
        }
        tx.commit().await?;
        Ok(())
    }

    /// `ListConstraints`: this exploration's constraints, allow before deny,
    /// oldest first within each group (stable render order).
    pub async fn list_constraints(&self) -> Result<Vec<Constraint>> {
        let rows = self
            .db
            .query(
                "SELECT id, kind, text, COALESCE(origin,''), created_at::text \
             FROM task_constraints WHERE exploration_id=$1 \
             ORDER BY (kind='deny'), id",
                &[&self.exp_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| Constraint {
                id: r.get(0),
                kind: r.get(1),
                text: r.get(2),
                origin: r.get(3),
                created_at: r.get(4),
            })
            .collect())
    }

    /// `AddConstraint`: insert one constraint (`kind` must be allow|deny; empty
    /// `origin` defaults to "system") and return its id.
    pub async fn add_constraint(&self, kind: &str, text: &str, origin: &str) -> Result<i64> {
        if kind != "allow" && kind != "deny" {
            return Err(Error::msg("kind must be allow or deny"));
        }
        let origin = if origin.is_empty() { "system" } else { origin };
        let row = self
            .db
            .query_one(
                "INSERT INTO task_constraints(exploration_id, kind, text, origin) \
             VALUES ($1, $2, $3, $4) RETURNING id",
                &[&self.exp_id, &kind, &text, &origin],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `UpdateConstraint`: rewrite a constraint's kind + text (scoped to this
    /// exploration); errors when no such constraint exists.
    pub async fn update_constraint(&self, id: i64, kind: &str, text: &str) -> Result<()> {
        if kind != "allow" && kind != "deny" {
            return Err(Error::msg("kind must be allow or deny"));
        }
        let n = self
            .db
            .execute(
                "UPDATE task_constraints SET kind=$1, text=$2, updated_at=now() \
             WHERE id=$3 AND exploration_id=$4",
                &[&kind, &text, &id, &self.exp_id],
            )
            .await?;
        if n == 0 {
            return Err(Error::msg("constraint does not exist"));
        }
        Ok(())
    }

    /// `DeleteConstraint`: remove a constraint (scoped to this exploration);
    /// errors when no such constraint exists.
    pub async fn delete_constraint(&self, id: i64) -> Result<()> {
        let n = self
            .db
            .execute(
                "DELETE FROM task_constraints WHERE id=$1 AND exploration_id=$2",
                &[&id, &self.exp_id],
            )
            .await?;
        if n == 0 {
            return Err(Error::msg("constraint does not exist"));
        }
        Ok(())
    }

    /// `SetIntentBlockedReason`: block an intent and record why (empty clears the
    /// reason). Scoped to this exploration; only intent-kind nodes are affected.
    pub async fn set_intent_blocked_reason(&self, id: i64, reason: &str) -> Result<()> {
        self.db
            .execute(
                "UPDATE exploration_nodes \
                 SET state='blocked', blocked_reason=NULLIF($1,''), completed_at=now() \
                 WHERE id=$2 AND exploration_id=$3 AND kind='intent'",
                &[&reason, &id, &self.exp_id],
            )
            .await?;
        Ok(())
    }

    /// `ReopenIntentsByBlockedReason`: reopen every blocked intent of this
    /// exploration that was blocked for `reason`; returns the count changed.
    pub async fn reopen_intents_by_blocked_reason(&self, reason: &str) -> Result<u64> {
        self.db
            .execute(
                "UPDATE exploration_nodes \
                 SET state='open', blocked_reason=NULL, completed_at=NULL \
                 WHERE exploration_id=$1 AND kind='intent' AND state='blocked' \
                 AND blocked_reason=$2",
                &[&self.exp_id, &reason],
            )
            .await
    }
}
