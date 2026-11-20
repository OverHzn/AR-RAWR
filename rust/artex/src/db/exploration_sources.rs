//! Source-aware exploration reads — port of `db/exploration_sources.go`.
//!
//! These helpers layer a task's direct source relations over the existing
//! exploration read methods so the agent sees inherited (terminal-only) context
//! tagged with its owning task. All mutations stay on the receiver store.

use crate::db::constants::KIND_INTENT;
use crate::db::error::Result;
use crate::db::exploration::{scan_nodes, Activity, AssetRef, ExplorationStore, NODE_COLS};
use crate::db::task_context::TaskSource;
use crate::db::DB;

/// `DirectSourceStore`: one directly related source task bound to its exploration
/// store. Read-side only; the receiver store remains the mutation owner.
pub struct DirectSourceStore {
    pub task: TaskSource,
    pub store: ExplorationStore,
}

fn mark_inherited_node(n: &mut crate::db::exploration::Node, task_id: i64) {
    n.source_task_id = task_id;
    n.inherited = true;
}

fn mark_inherited_activities(in_acts: &mut [Activity], task_id: i64) {
    for a in in_acts {
        a.source_task_id = task_id;
        a.inherited = true;
    }
}

fn inherited_intent_terminal(state: &str) -> bool {
    matches!(state, "done" | "blocked" | "exhausted" | "stopped")
}

impl ExplorationStore {
    /// `DirectSourceStores`: resolve live, direct task relations on every call —
    /// a deleted source must drop out immediately; sources of a source are never
    /// expanded.
    pub async fn direct_source_stores(&self) -> Result<Vec<DirectSourceStore>> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&self.id()];
        let rows = self
            .db()
            .query(
                "SELECT source.id, source.exploration_id, source.description, source.goal, source.status \
                 FROM tasks owner \
                 JOIN task_relations relation ON relation.task_id=owner.id \
                 JOIN tasks source ON source.id=relation.source_task_id AND source.deleted_at IS NULL \
                 WHERE owner.exploration_id=$1 AND owner.deleted_at IS NULL \
                 ORDER BY relation.created_at, source.id",
                &params,
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let task = TaskSource {
                task_id: r.get(0),
                exploration_id: r.get(1),
                description: r.get(2),
                goal: r.get(3),
                status: r.get(4),
            };
            out.push(DirectSourceStore {
                store: self.db().exploration(task.exploration_id),
                task,
            });
        }
        Ok(out)
    }

    /// `TaskID`: the live task bound to this exploration (0 when none).
    pub async fn task_id(&self) -> Result<i64> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&self.id()];
        let row = self
            .db()
            .query_opt(
                "SELECT id FROM tasks WHERE exploration_id=$1 AND deleted_at IS NULL",
                &params,
            )
            .await?;
        Ok(row.map(|r| r.get::<_, i64>(0)).unwrap_or(0))
    }

    /// `listByKindPageFiltered`: newest-first page (id < before) with an optional
    /// payload summary ILIKE filter, fetching `limit+1` for has-more detection.
    pub(crate) async fn list_by_kind_page_filtered(
        &self,
        kind: &str,
        before: i64,
        limit: i32,
        q: &str,
    ) -> Result<Vec<crate::db::exploration::Node>> {
        let mut conds =
            String::from("exploration_id=$1 AND kind=$2 AND ($3::bigint <= 0 OR id < $3)");
        let exp_id = self.id();
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&exp_id, &kind, &before];
        let pat;
        if !q.is_empty() {
            pat = format!("%{q}%");
            params.push(&pat);
            conds.push_str(&format!(" AND payload->>'summary' ILIKE ${}", params.len()));
        }
        let fetch = limit + 1;
        params.push(&fetch);
        let sql = format!(
            "SELECT {NODE_COLS} FROM exploration_nodes WHERE {conds} ORDER BY id DESC LIMIT ${}",
            params.len()
        );
        let rows = self.db().query(&sql, &params).await?;
        Ok(scan_nodes(&rows))
    }

    /// `countByKindFiltered`: filtered node count for one exploration/kind.
    pub(crate) async fn count_by_kind_filtered(&self, kind: &str, q: &str) -> Result<i64> {
        let mut conds = String::from("exploration_id=$1 AND kind=$2");
        let exp_id = self.id();
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&exp_id, &kind];
        let pat;
        if !q.is_empty() {
            pat = format!("%{q}%");
            params.push(&pat);
            conds.push_str(&format!(" AND payload->>'summary' ILIKE ${}", params.len()));
        }
        let sql = format!("SELECT COUNT(*) FROM exploration_nodes WHERE {conds}");
        let row = self.db().query_one(&sql, &params).await?;
        Ok(row.get::<_, i64>(0))
    }

    /// `ListByKindWithSources`: local nodes followed by terminal-only inherited
    /// nodes from each source, with the per-exploration limit preserved.
    pub async fn list_by_kind_with_sources(
        &self,
        kind: &str,
        limit: i32,
    ) -> Result<Vec<crate::db::exploration::Node>> {
        let mut out = self.list_by_kind(kind, limit).await?;
        for source in self.direct_source_stores().await? {
            let nodes = source.store.list_by_kind(kind, limit).await?;
            for mut node in nodes {
                if kind == KIND_INTENT && !inherited_intent_terminal(&node.state) {
                    continue;
                }
                mark_inherited_node(&mut node, source.task.task_id);
                out.push(node);
            }
        }
        Ok(out)
    }

    /// `ListByKindPageWithSources`: one global newest-first page spanning this
    /// exploration and its direct sources, plus hasMore and the filtered total.
    pub async fn list_by_kind_page_with_sources(
        &self,
        kind: &str,
        before: i64,
        limit: i32,
        q: &str,
    ) -> Result<(Vec<crate::db::exploration::Node>, bool, i64)> {
        let limit = if limit <= 0 { 20 } else { limit };
        let mut merged = self
            .list_by_kind_page_filtered(kind, before, limit, q)
            .await?;
        let mut total = self.count_by_kind_filtered(kind, q).await?;
        for source in self.direct_source_stores().await? {
            let mut page = source
                .store
                .list_by_kind_page_filtered(kind, before, limit, q)
                .await?;
            for node in &mut page {
                mark_inherited_node(node, source.task.task_id);
            }
            merged.extend(page);
            total += source.store.count_by_kind_filtered(kind, q).await?;
        }
        merged.sort_by_key(|a| std::cmp::Reverse(a.id));
        let has_more = merged.len() > limit as usize;
        if has_more {
            merged.truncate(limit as usize);
        }
        Ok((merged, has_more, total))
    }

    /// `GetNodeWithSources`: read a node from this exploration or a direct source.
    /// Inherited non-terminal intents are excluded.
    pub async fn get_node_with_sources(
        &self,
        id: i64,
    ) -> Result<Option<crate::db::exploration::Node>> {
        if let Some(node) = self.get_node(id).await? {
            return Ok(Some(node));
        }
        for source in self.direct_source_stores().await? {
            if let Some(mut node) = source.store.get_node(id).await? {
                if node.kind == KIND_INTENT && !inherited_intent_terminal(&node.state) {
                    continue;
                }
                mark_inherited_node(&mut node, source.task.task_id);
                return Ok(Some(node));
            }
        }
        Ok(None)
    }

    /// `FindingIntentsWithSources`: finding→intent lineage for this exploration
    /// plus terminal-only lineage from each direct source.
    pub async fn finding_intents_with_sources(
        &self,
    ) -> Result<std::collections::BTreeMap<i64, i64>> {
        let mut out = self.finding_intents().await?;
        for source in self.direct_source_stores().await? {
            for (fid, iid) in source.store.finding_intents_terminal().await? {
                out.insert(fid, iid);
            }
        }
        Ok(out)
    }

    /// `ActivityTraceWithSources`: local trace, else a terminal-intent trace from
    /// the (unique) owning direct source.
    pub async fn activity_trace_with_sources(
        &self,
        node_id: i64,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let acts = self.activity_trace(node_id, limit).await?;
        if !acts.is_empty() {
            return Ok(acts);
        }
        for source in self.direct_source_stores().await? {
            let Some(node) = source.store.get_node(node_id).await? else {
                continue;
            };
            if node.kind != KIND_INTENT {
                continue;
            }
            let mut acts = source
                .store
                .activity_trace_for_terminal_intent(node_id, limit)
                .await?;
            if !acts.is_empty() {
                mark_inherited_activities(&mut acts, source.task.task_id);
                return Ok(acts);
            }
        }
        Ok(Vec::new())
    }

    /// `ActivityListWithSources`: node ids are global, so the first owning
    /// exploration (local or a source) is unambiguous.
    pub async fn activity_list_with_sources(
        &self,
        node_id: i64,
        since_id: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, i64)> {
        if self.get_node(node_id).await?.is_some() {
            return self.activity_list(Some(node_id), since_id, limit).await;
        }
        for source in self.direct_source_stores().await? {
            let Some(node) = source.store.get_node(node_id).await? else {
                continue;
            };
            if node.kind != KIND_INTENT {
                continue;
            }
            let (mut acts, cursor) = source
                .store
                .activity_list_for_terminal_intent(node_id, since_id, limit)
                .await?;
            mark_inherited_activities(&mut acts, source.task.task_id);
            return Ok((acts, cursor));
        }
        Ok((Vec::new(), since_id))
    }

    /// `ActivityDetailWithSources`: local detail first; inherited details only
    /// from terminal worker intents (via `ActivityByIDsWithSources`).
    pub async fn activity_detail_with_sources(&self, id: i64) -> Result<String> {
        let detail = self.activity_detail(id).await?;
        if !detail.is_empty() {
            return Ok(detail);
        }
        let acts = self.activity_by_ids_with_sources(&[id]).await?;
        Ok(acts.first().map(|a| a.detail.clone()).unwrap_or_default())
    }

    /// `ActivityTraceSearchWithSources`: scoped worker-trace search.
    pub async fn activity_trace_search_with_sources(
        &self,
        node_id: i64,
        q: &str,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let acts = self.activity_trace_search(Some(node_id), q, limit).await?;
        if !acts.is_empty() {
            return Ok(acts);
        }
        for source in self.direct_source_stores().await? {
            let Some(node) = source.store.get_node(node_id).await? else {
                continue;
            };
            if node.kind != KIND_INTENT {
                continue;
            }
            let mut acts = source
                .store
                .activity_trace_search_for_terminal_intent(node_id, q, limit)
                .await?;
            if !acts.is_empty() {
                mark_inherited_activities(&mut acts, source.task.task_id);
                return Ok(acts);
            }
        }
        Ok(Vec::new())
    }

    /// `ActivityTraceSearchAllWithSources`: local search plus every source's
    /// terminal intents, merged by id ascending and capped at `limit`.
    pub async fn activity_trace_search_all_with_sources(
        &self,
        exclude_node_id: i64,
        q: &str,
        limit: i32,
    ) -> Result<Vec<Activity>> {
        let limit = if limit <= 0 { 100 } else { limit };
        let mut out = self
            .activity_trace_search_excluding(exclude_node_id, q, limit)
            .await?;
        for source in self.direct_source_stores().await? {
            let mut acts = source
                .store
                .activity_trace_search_terminal_intents(q, limit)
                .await?;
            mark_inherited_activities(&mut acts, source.task.task_id);
            out.extend(acts);
        }
        out.sort_by_key(|a| a.id);
        if out.len() > limit as usize {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    /// `ActivityByIDsWithSources`: local details plus source rows restricted to
    /// terminal intents, sorted by id ascending.
    pub async fn activity_by_ids_with_sources(&self, ids: &[i64]) -> Result<Vec<Activity>> {
        let mut out = self.activity_by_ids(ids).await?;
        for source in self.direct_source_stores().await? {
            let mut acts = source
                .store
                .activity_by_ids_for_terminal_intents(ids)
                .await?;
            mark_inherited_activities(&mut acts, source.task.task_id);
            out.extend(acts);
        }
        out.sort_by_key(|a| a.id);
        Ok(out)
    }

    /// `AssetRefsWithSources`: anchored nodes from this exploration + direct
    /// sources (terminal intents only), sorted by id desc.
    pub async fn asset_refs_with_sources(&self, asset_id: i64) -> Result<Vec<AssetRef>> {
        let mut out = self.asset_refs(asset_id).await?;
        for source in self.direct_source_stores().await? {
            let mut refs = source.store.asset_refs(asset_id).await?;
            for r in &mut refs {
                if r.kind == KIND_INTENT && !inherited_intent_terminal(&r.state) {
                    continue;
                }
                r.source_task_id = source.task.task_id;
                r.inherited = true;
                out.push(r.clone());
            }
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.id));
        Ok(out)
    }
}

// Keep `DB` imported for readers of this module's doc links.
#[allow(dead_code)]
fn _db_type(_: &DB) {}
