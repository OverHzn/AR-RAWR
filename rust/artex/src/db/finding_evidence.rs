//! Traffic-evidence primitives — the foundation slice of `db/finding_traffic.go`.
//!
//! A finding's proof-of-exploitation is backed by immutable traffic-evidence
//! snapshots (request/response heads, hashes, lengths) referenced by ordered
//! bindings. This module ports the self-contained primitives that have no
//! dependency on the task-archive state machine or the notification-event writer:
//!   - sentinels (`ErrEvidenceConflict` / `ErrFindingNotFound` / `ErrEvidenceNotFound`),
//!   - `ArchiveFailed` + `ErrTaskArchiveState` (consumed by the archive-state lock
//!     in a later slice; defined here so the whole cluster shares one source),
//!   - types (`TrafficRef`, `TrafficEvidenceSnapshot`, `FindingTrafficBinding`,
//!     `FindingTraffic`, `PreparedTrafficEvidence`),
//!   - `valid_traffic_role`, `normalize_traffic_refs`,
//!     `TrafficEvidenceSnapshot::normalize` + `traffic_snapshot_id`,
//!   - `with_evidence_tx`, `insert_evidence_snapshot_tx` + `open_heads`,
//!     `bump_evidence_version_tx`, `add_finding_traffic_tx`.
//!
//! `lock_task_evidence_tx` / `lock_finding_evidence_tx` / `finding_traffic_tx` /
//! `get_finding_traffic` / `edit_finding_traffic` (D-5b) and
//! `record_finding_tx` / `record_finding` / `finding_id_by_node_id` /
//! `set_finding_report_version_by_node_id` (D-5c) land in later slices.

use crate::db::error::{Error, Result};
use crate::db::secrets::{
    open_secret, seal_secret, SecretColumn, COL_EV_REQ_HEAD, COL_EV_RESP_HEAD,
};
use crate::db::DB;

/// `ErrEvidenceConflict`: the traffic evidence changed under us; refresh + retry.
pub const ERR_EVIDENCE_CONFLICT: &str = "traffic evidence has changed, please refresh and retry";
/// `ErrFindingNotFound`: the finding does not exist.
pub const ERR_FINDING_NOT_FOUND: &str = "finding does not exist";
/// `ErrEvidenceNotFound`: the traffic-evidence binding does not exist.
pub const ERR_EVIDENCE_NOT_FOUND: &str = "traffic evidence does not exist";

/// `findingEvidenceLockKey`: the advisory-lock key covering the evidence
/// filesystem as well as its SQL references (shared by every process — readers,
/// exports and GC included).
pub const FINDING_EVIDENCE_LOCK_KEY: i64 = 7_337_741_004;

/// `ArchiveFailed`: the terminal archive state; only from this state may a task
/// acquire new evidence (see `lock_task_evidence_tx`).
pub const ARCHIVE_FAILED: &str = "archive_failed";
/// `ErrTaskArchiveState`: the task's archive state forbids this operation.
pub const ERR_TASK_ARCHIVE_STATE: &str = "task archive state does not allow this operation";

/// `TrafficRef`: a reference from a finding to one captured traffic snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrafficRef {
    pub traffic_id: String,
    pub role: String,
    pub note: String,
}

/// `validTrafficRole`: is `role` a known binding role?
pub fn valid_traffic_role(role: &str) -> bool {
    matches!(role, "baseline" | "proof" | "verification" | "supporting")
}

/// `NormalizeTrafficRefs`: trim ids, drop empty-id refs (error), default the role
/// to `supporting`, validate the role, and dedupe by traffic id (first wins).
pub fn normalize_traffic_refs(refs: &[TrafficRef]) -> Result<Vec<TrafficRef>> {
    let mut out: Vec<TrafficRef> = Vec::with_capacity(refs.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in refs {
        let mut r = r.clone();
        r.traffic_id = r.traffic_id.trim().to_string();
        if r.traffic_id.is_empty() {
            return Err(Error::msg("traffic_id must not be empty"));
        }
        if r.role.is_empty() {
            r.role = "supporting".to_string();
        }
        if !valid_traffic_role(&r.role) {
            return Err(Error::msg(format!("invalid traffic role {:?}", r.role)));
        }
        if seen.insert(r.traffic_id.clone()) {
            out.push(r);
        }
    }
    Ok(out)
}

/// `TrafficEvidenceSnapshot`: one immutable captured request/response snapshot.
/// Text head columns are sealed (encrypted) at rest; `traffic_snapshot_id` hashes
/// the cleartext normalized form.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct TrafficEvidenceSnapshot {
    pub id: String,
    pub source_traffic_id: String,
    pub captured_at: i64,
    pub url: String,
    pub method: String,
    pub status: i32,
    pub content_type: String,
    pub req_head: String,
    pub resp_head: String,
    pub req_hash: String,
    pub resp_hash: String,
    pub req_len: i64,
    pub resp_len: i64,
    /// epoch when zero (Go `time.Time{}`); RFC3339 when read back.
    #[serde(deserialize_with = "de_created_at")]
    pub created_at: std::time::SystemTime,
}

fn de_created_at<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<std::time::SystemTime, D::Error> {
    let s = <Option<String> as serde::Deserialize>::deserialize(d)?;
    Ok(s.and_then(|s| crate::db::gotime::parse_rfc3339(&s))
        .unwrap_or(std::time::UNIX_EPOCH))
}

impl Default for TrafficEvidenceSnapshot {
    fn default() -> Self {
        TrafficEvidenceSnapshot {
            id: String::new(),
            source_traffic_id: String::new(),
            captured_at: 0,
            url: String::new(),
            method: String::new(),
            status: 0,
            content_type: String::new(),
            req_head: String::new(),
            resp_head: String::new(),
            req_hash: String::new(),
            resp_hash: String::new(),
            req_len: 0,
            resp_len: 0,
            created_at: std::time::SystemTime::UNIX_EPOCH,
        }
    }
}

impl Default for FindingTrafficBinding {
    fn default() -> Self {
        FindingTrafficBinding {
            id: 0,
            finding_id: 0,
            snapshot_id: String::new(),
            role: String::new(),
            note: String::new(),
            position: 0,
            created_at: std::time::SystemTime::UNIX_EPOCH,
            snapshot: TrafficEvidenceSnapshot::default(),
        }
    }
}

impl TrafficEvidenceSnapshot {
    /// `Normalize`: scrub the wire-sourced text columns so a non-UTF-8 header can't
    /// abort the INSERT. Applied before hashing so the ID matches the stored bytes.
    pub fn normalize(&self) -> TrafficEvidenceSnapshot {
        let mut s = self.clone();
        s.source_traffic_id = crate::db::exploration::utf8_clean(&s.source_traffic_id);
        s.url = crate::db::exploration::utf8_clean(&s.url);
        s.method = crate::db::exploration::utf8_clean(&s.method);
        s.content_type = crate::db::exploration::utf8_clean(&s.content_type);
        s.req_head = crate::db::exploration::utf8_clean(&s.req_head);
        s.resp_head = crate::db::exploration::utf8_clean(&s.resp_head);
        s
    }
}

/// `FindingTrafficBinding`: one ordered link from a finding to a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingTrafficBinding {
    pub id: i64,
    pub finding_id: i64,
    pub snapshot_id: String,
    pub role: String,
    pub note: String,
    pub position: i32,
    pub created_at: std::time::SystemTime,
    pub snapshot: TrafficEvidenceSnapshot,
}

/// `FindingTraffic`: a finding's evidence version + report version + bindings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FindingTraffic {
    pub finding_id: i64,
    pub version: i64,
    pub report_version: i64,
    pub bindings: Vec<FindingTrafficBinding>,
}

/// `PreparedTrafficEvidence`: a normalized ref paired with its ready-to-store
/// snapshot (id already computed over the normalized cleartext).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreparedTrafficEvidence {
    pub reference: TrafficRef,
    pub snapshot: TrafficEvidenceSnapshot,
}

/// `trafficSnapshotID`: the stable content hash of a snapshot. Hashes the
/// normalized cleartext with `id` and `created_at` cleared, matching Go's
/// `json.Marshal` of the struct with `ID=""` and `CreatedAt=time.Time{}`. The two
/// head fields carry `omitempty`, so an empty head is dropped from the JSON.
pub fn traffic_snapshot_id(snapshot: &TrafficEvidenceSnapshot) -> String {
    let s = snapshot.normalize();
    // Manual JSON writer to reproduce Go's encoding/json field order + omitempty
    // exactly so the digest is bit-identical.
    let mut json = String::from("{\"id\":\"\",\"source_traffic_id\":");
    json.push_str(&serde_json::to_string(&s.source_traffic_id).unwrap());
    json.push_str(&format!(",\"captured_at\":{}", s.captured_at));
    json.push_str(",\"url\":");
    json.push_str(&serde_json::to_string(&s.url).unwrap());
    json.push_str(",\"method\":");
    json.push_str(&serde_json::to_string(&s.method).unwrap());
    json.push_str(&format!(",\"status\":{}", s.status));
    json.push_str(",\"content_type\":");
    json.push_str(&serde_json::to_string(&s.content_type).unwrap());
    if !s.req_head.is_empty() {
        json.push_str(",\"req_head\":");
        json.push_str(&serde_json::to_string(&s.req_head).unwrap());
    }
    if !s.resp_head.is_empty() {
        json.push_str(",\"resp_head\":");
        json.push_str(&serde_json::to_string(&s.resp_head).unwrap());
    }
    json.push_str(",\"req_hash\":");
    json.push_str(&serde_json::to_string(&s.req_hash).unwrap());
    json.push_str(",\"resp_hash\":");
    json.push_str(&serde_json::to_string(&s.resp_hash).unwrap());
    json.push_str(&format!(",\"req_len\":{}", s.req_len));
    json.push_str(&format!(",\"resp_len\":{}", s.resp_len));
    json.push_str(",\"created_at\":\"0001-01-01T00:00:00Z\"}");

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    to_hex(&hasher.finalize())
}

/// Lowercase hex encoding (replaces Go `hex.EncodeToString` of a SHA-256 sum).
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

impl DB {
    /// `WithEvidenceTx`: run `f` inside a transaction holding the evidence advisory
    /// lock (serialises writers, readers, exports and GC across processes).
    pub async fn with_evidence_tx<F, Fut>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&deadpool_postgres::Transaction<'_>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        f(&tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// `InsertEvidenceSnapshotTx`: store one snapshot, sealing the request/response
    /// heads. Verifies the id matches the content hash; `ON CONFLICT DO NOTHING`
    /// makes re-inserts idempotent.
    pub async fn insert_evidence_snapshot_tx(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        snapshot: &TrafficEvidenceSnapshot,
    ) -> Result<()> {
        if snapshot.id != traffic_snapshot_id(snapshot) {
            return Err(Error::msg("evidence snapshot metadata hash mismatch"));
        }
        // The id hashed the normalized cleartext; store those same bytes.
        let id = snapshot.id.clone();
        let mut s = snapshot.normalize();
        s.id = id;
        if s.created_at == std::time::SystemTime::UNIX_EPOCH {
            s.created_at = std::time::SystemTime::now();
        }
        let req_head = seal(&COL_EV_REQ_HEAD, &s.req_head)?;
        let resp_head = seal(&COL_EV_RESP_HEAD, &s.resp_head)?;
        tx.execute(
            "INSERT INTO traffic_evidence_snapshots \
             (id,source_traffic_id,captured_at,url,method,status,content_type,req_head,resp_head,\
             req_hash,resp_hash,req_len,resp_len,created_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT(id) DO NOTHING",
            &[
                &s.id,
                &s.source_traffic_id,
                &s.captured_at,
                &s.url,
                &s.method,
                &s.status,
                &s.content_type,
                &req_head,
                &resp_head,
                &s.req_hash,
                &s.resp_hash,
                &s.req_len,
                &s.resp_len,
                &s.created_at,
            ],
        )
        .await?;
        Ok(())
    }

    /// `bumpEvidenceVersionTx`: increment a finding's evidence version.
    pub async fn bump_evidence_version_tx(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        finding_id: i64,
    ) -> Result<()> {
        tx.execute(
            "UPDATE findings SET evidence_version=evidence_version+1 WHERE id=$1",
            &[&finding_id],
        )
        .await?;
        Ok(())
    }

    /// `AddFindingTrafficTx`: attach prepared evidence to a finding inside a tx —
    /// insert (idempotent) each snapshot, clear its `unreferenced_at`, add the
    /// ordered binding, and bump the evidence version if anything changed.
    pub async fn add_finding_traffic_tx(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        finding_id: i64,
        prepared: &[PreparedTrafficEvidence],
    ) -> Result<()> {
        let mut position: i32 = tx
            .query_one(
                "SELECT COALESCE(MAX(position)+1,0) FROM finding_traffic_bindings WHERE finding_id=$1",
                &[&finding_id],
            )
            .await?
            .get::<_, i32>(0);
        let mut changed = false;
        for item in prepared {
            self.insert_evidence_snapshot_tx(tx, &item.snapshot).await?;
            tx.execute(
                "UPDATE traffic_evidence_snapshots SET unreferenced_at=NULL WHERE id=$1",
                &[&item.snapshot.id],
            )
            .await?;
            let n = tx
                .execute(
                    "INSERT INTO finding_traffic_bindings(finding_id,snapshot_id,role,note,position) \
                     VALUES($1,$2,$3,$4,$5) ON CONFLICT(finding_id,snapshot_id) DO NOTHING",
                    &[
                        &finding_id,
                        &item.snapshot.id,
                        &item.reference.role,
                        &item.reference.note,
                        &position,
                    ],
                )
                .await?;
            if n > 0 {
                changed = true;
                position += 1;
            }
        }
        if changed {
            self.bump_evidence_version_tx(tx, finding_id).await?;
        }
        Ok(())
    }
}

/// `sealSecret` wrapper mapping the secretbox error string into `db::Error`.
fn seal(c: &SecretColumn, v: &str) -> Result<String> {
    seal_secret(c, v).map_err(Error::msg)
}

/// `TrafficEvidenceSnapshot::openHeads`: decrypt the two head columns in place.
/// A legacy cleartext value passes through; a value sealed under another key errors.
pub fn open_snapshot_heads(s: &mut TrafficEvidenceSnapshot) -> Result<()> {
    s.req_head = open_secret(&COL_EV_REQ_HEAD, &s.req_head).map_err(Error::msg)?;
    s.resp_head = open_secret(&COL_EV_RESP_HEAD, &s.resp_head).map_err(Error::msg)?;
    Ok(())
}

/// `LockTaskEvidenceTx`: serialize writes with archive queueing (which locks the
/// same task row). Once a task is archived (or soft-deleted), its snapshot must
/// not acquire new evidence. `task_id == 0` is a no-op.
pub async fn lock_task_evidence_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    task_id: i64,
) -> Result<()> {
    if task_id == 0 {
        return Ok(());
    }
    let row = tx
        .query_opt(
            "SELECT deleted_at FROM tasks WHERE id=$1 FOR UPDATE",
            &[&task_id],
        )
        .await?;
    let Some(row) = row else {
        // Go: QueryRow.Scan returns sql.ErrNoRows when the task is gone. The
        // original returns that raw error (not the archive-state sentinel).
        return Err(Error::msg("no rows in result set"));
    };
    let deleted: Option<std::time::SystemTime> = row.get(0);
    if deleted.is_some() {
        return Err(Error::sentinel(
            ERR_TASK_ARCHIVE_STATE,
            ERR_TASK_ARCHIVE_STATE,
        ));
    }
    let state = tx
        .query_opt(
            "SELECT state FROM task_archives WHERE task_id=$1",
            &[&task_id],
        )
        .await?;
    if let Some(row) = state {
        let s: String = row.get(0);
        if s != ARCHIVE_FAILED {
            return Err(Error::sentinel(
                ERR_TASK_ARCHIVE_STATE,
                ERR_TASK_ARCHIVE_STATE,
            ));
        }
    }
    Ok(())
}

/// `LockFindingEvidenceTx`: look up the finding's task, lock it against archive
/// queueing, then `FOR UPDATE` the finding row and (optionally) assert its
/// evidence version is unchanged. `version == None` skips the optimistic check.
pub async fn lock_finding_evidence_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    finding_id: i64,
    version: Option<i64>,
) -> Result<()> {
    let task_row = tx
        .query_opt("SELECT task_id FROM findings WHERE id=$1", &[&finding_id])
        .await?;
    let Some(task_row) = task_row else {
        return Err(Error::sentinel(
            ERR_FINDING_NOT_FOUND,
            ERR_FINDING_NOT_FOUND,
        ));
    };
    let task_id: Option<i64> = task_row.get(0);
    lock_task_evidence_tx(tx, task_id.unwrap_or(0)).await?;

    let cur = tx
        .query_opt(
            "SELECT evidence_version FROM findings WHERE id=$1 FOR UPDATE",
            &[&finding_id],
        )
        .await?;
    let Some(cur) = cur else {
        return Err(Error::sentinel(
            ERR_FINDING_NOT_FOUND,
            ERR_FINDING_NOT_FOUND,
        ));
    };
    let current: i64 = cur.get(0);
    if let Some(v) = version {
        if v != current {
            return Err(Error::sentinel(
                ERR_EVIDENCE_CONFLICT,
                ERR_EVIDENCE_CONFLICT,
            ));
        }
    }
    Ok(())
}

/// `FindingTrafficTx`: read a finding's evidence version, report version, and
/// ordered bindings (each with its decrypted snapshot) inside a tx.
pub(crate) async fn finding_traffic_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    finding_id: i64,
) -> Result<FindingTraffic> {
    let mut out = FindingTraffic {
        finding_id,
        bindings: Vec::new(),
        ..FindingTraffic::default()
    };
    let head = tx
        .query_opt(
            "SELECT evidence_version,report_evidence_version FROM findings WHERE id=$1",
            &[&finding_id],
        )
        .await?;
    let Some(head) = head else {
        return Err(Error::sentinel(
            ERR_FINDING_NOT_FOUND,
            ERR_FINDING_NOT_FOUND,
        ));
    };
    out.version = head.get(0);
    out.report_version = head.get(1);

    let rows = tx
        .query(
            "SELECT b.id,b.finding_id,b.snapshot_id,b.role,b.note,b.position,b.created_at,\
             s.source_traffic_id,s.captured_at,s.url,s.method,s.status,s.content_type,\
             s.req_head,s.resp_head,s.req_hash,s.resp_hash,s.req_len,s.resp_len,s.created_at \
             FROM finding_traffic_bindings b \
             JOIN traffic_evidence_snapshots s ON s.id=b.snapshot_id \
             WHERE b.finding_id=$1 ORDER BY b.position,b.id",
            &[&finding_id],
        )
        .await?;
    for r in &rows {
        let mut snap = TrafficEvidenceSnapshot {
            id: r.get::<_, String>(2),
            source_traffic_id: r.get(7),
            captured_at: r.get(8),
            url: r.get(9),
            method: r.get(10),
            status: r.get(11),
            content_type: r.get(12),
            req_head: r.get(13),
            resp_head: r.get(14),
            req_hash: r.get(15),
            resp_hash: r.get(16),
            req_len: r.get(17),
            resp_len: r.get(18),
            created_at: r.get(19),
        };
        open_snapshot_heads(&mut snap)?;
        out.bindings.push(FindingTrafficBinding {
            id: r.get(0),
            finding_id: r.get(1),
            snapshot_id: r.get(2),
            role: r.get(3),
            note: r.get(4),
            position: r.get(5),
            created_at: r.get(6),
            snapshot: snap,
        });
    }
    Ok(out)
}

impl DB {
    /// `GetFindingTraffic`: read a finding's traffic evidence (versions + ordered
    /// decrypted bindings) under the evidence advisory lock.
    pub async fn get_finding_traffic(&self, finding_id: i64) -> Result<FindingTraffic> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        let out = finding_traffic_tx(&tx, finding_id).await;
        tx.commit().await?;
        out
    }

    /// `EditFindingTraffic`: edit one finding's evidence bindings under the evidence
    /// lock. `order` (when present) reorders every binding (must be a permutation
    /// of the current binding ids); otherwise a single binding is removed
    /// (`remove`) or has its role/note patched (`COALESCE` keeps NULL = unchanged).
    /// `version` is the optimistic-concurrency guard.
    #[allow(clippy::too_many_arguments)]
    pub async fn edit_finding_traffic(
        &self,
        finding_id: i64,
        binding_id: i64,
        version: i64,
        role: Option<&str>,
        note: Option<&str>,
        remove: bool,
        order: Option<&[i64]>,
    ) -> Result<()> {
        if let Some(r) = role {
            if !valid_traffic_role(r) {
                return Err(Error::msg("invalid traffic role"));
            }
        }
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        let result = async {
            lock_finding_evidence_tx(&tx, finding_id, Some(version)).await?;
            if let Some(order) = order {
                let current = finding_traffic_tx(&tx, finding_id).await?;
                if order.len() != current.bindings.len() {
                    return Err(Error::sentinel(ERR_EVIDENCE_CONFLICT, ERR_EVIDENCE_CONFLICT));
                }
                let mut ids: std::collections::HashSet<i64> =
                    current.bindings.iter().map(|b| b.id).collect();
                for (position, &id) in order.iter().enumerate() {
                    if !ids.remove(&id) {
                        return Err(Error::sentinel(
                            ERR_EVIDENCE_CONFLICT,
                            ERR_EVIDENCE_CONFLICT,
                        ));
                    }
                    let position = position as i32;
                    tx.execute(
                        "UPDATE finding_traffic_bindings SET position=$1 WHERE finding_id=$2 AND id=$3",
                        &[&position, &finding_id, &id],
                    )
                    .await?;
                }
            } else {
                let n = if remove {
                    tx.execute(
                        "DELETE FROM finding_traffic_bindings WHERE finding_id=$1 AND id=$2",
                        &[&finding_id, &binding_id],
                    )
                    .await?
                } else {
                    tx.execute(
                        "UPDATE finding_traffic_bindings SET role=COALESCE($3,role),note=COALESCE($4,note) \
                         WHERE finding_id=$1 AND id=$2",
                        &[&finding_id, &binding_id, &role, &note],
                    )
                    .await?
                };
                if n == 0 {
                    return Err(Error::sentinel(ERR_EVIDENCE_NOT_FOUND, ERR_EVIDENCE_NOT_FOUND));
                }
            }
            self.bump_evidence_version_tx(&tx, finding_id).await
        }
        .await;
        match result {
            Ok(()) => {
                tx.commit().await?;
                Ok(())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}

impl DB {
    /// `FindingIDByNodeID`: the standalone finding id anchored to `node_id`
    /// (ErrNoRows-style error when the node has no finding row).
    pub async fn finding_id_by_node_id(&self, node_id: i64) -> Result<i64> {
        let row = self
            .query_opt("SELECT id FROM findings WHERE node_id=$1", &[&node_id])
            .await?;
        match row {
            Some(r) => Ok(r.get::<_, i64>(0)),
            None => Err(Error::msg("no rows in result set")),
        }
    }

    /// `SetFindingReportVersionByNodeID`: write a report body onto the finding
    /// anchored to `node_id`, under the evidence lock + optimistic version guard.
    /// The `report_evidence_version` pins the body to the evidence it describes:
    /// `version` (when given) is asserted equal, else the body is marked against
    /// the current evidence (`0` if none, `-1` if evidence changed since).
    /// Returns rows affected (0 when the node has no finding).
    pub async fn set_finding_report_version_by_node_id(
        &self,
        node_id: i64,
        report: &str,
        version: Option<i64>,
    ) -> Result<i64> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        let result = async {
            let id = tx
                .query_opt("SELECT id FROM findings WHERE node_id=$1", &[&node_id])
                .await?;
            let Some(id) = id else {
                return Ok(0);
            };
            let id: i64 = id.get(0);
            lock_finding_evidence_tx(&tx, id, version).await?;
            let n = tx
                .execute(
                    "UPDATE findings SET report=$2,\
                     report_evidence_version=COALESCE($3::bigint,CASE WHEN evidence_version=0 THEN 0 ELSE -1 END) \
                     WHERE id=$1",
                    &[&id, &report, &version],
                )
                .await?;
            Ok(n as i64)
        }
        .await;
        match result {
            Ok(n) => {
                tx.commit().await?;
                Ok(n)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `PopulateFindingTrafficIDs`: enrich already-visible finding nodes with their
    /// standalone-row finding id + traffic-binding count. No discovery; the legacy
    /// node id is left unchanged.
    pub async fn populate_finding_traffic_ids(
        &self,
        nodes: &mut [crate::db::exploration::Node],
    ) -> Result<()> {
        // Collect the distinct finding-node ids in one IN list.
        let mut ids: Vec<i64> = Vec::new();
        let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for n in nodes.iter() {
            if n.kind == crate::db::constants::KIND_FINDING && seen.insert(n.id) {
                ids.push(n.id);
            }
        }
        if ids.is_empty() {
            return Ok(());
        }
        let rows = self
            .query(
                "SELECT f.id,f.node_id,(SELECT count(*) FROM finding_traffic_bindings b WHERE b.finding_id=f.id) \
                 FROM findings f WHERE f.node_id = ANY($1::bigint[])",
                &[&ids],
            )
            .await?;
        // Index nodes by id for O(1) lookup.
        let mut by_id: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            by_id.insert(n.id, i);
        }
        for r in &rows {
            let finding_id: i64 = r.get(0);
            let node_id: i64 = r.get(1);
            let count: i64 = r.get(2);
            if let Some(&i) = by_id.get(&node_id) {
                nodes[i].finding_id = finding_id;
                nodes[i].finding_node_id = node_id;
                nodes[i].traffic_count = count;
            }
        }
        Ok(())
    }
}

/// `RecordFindingInput`: the inputs to the atomic finding recorder.
#[derive(Debug, Clone, Default)]
pub struct RecordFindingInput {
    pub task_id: i64,
    pub exploration_id: i64,
    pub intent_id: i64,
    pub vulnclass: String,
    pub name: String,
    pub severity: String,
    pub summary: String,
    pub evidence: String,
    pub worker: String,
    /// `""` = not given (both are reporter-supplied reasoning).
    pub counterevidence: String,
    pub confidence_rationale: String,
    pub asset_ids: Vec<i64>,
}

/// `RecordedFinding`: the result of recording a finding — its standalone row id,
/// the anchored finding-node id, and the (decrypted) traffic evidence.
#[derive(Debug, Clone, Default)]
pub struct RecordedFinding {
    pub finding_id: i64,
    pub node_id: i64,
    pub traffic: FindingTraffic,
}

impl DB {
    /// `RecordFindingTx`: atomically record a finding (node + anchors + optional
    /// intent edge + findings row + a same-transaction notification event + any
    /// prepared traffic evidence) inside a caller's transaction.
    pub async fn record_finding_tx(
        &self,
        tx: &deadpool_postgres::Transaction<'_>,
        input: &RecordFindingInput,
        prepared: &[PreparedTrafficEvidence],
    ) -> Result<RecordedFinding> {
        lock_task_evidence_tx(tx, input.task_id).await?;
        if input.task_id > 0 {
            let row = tx
                .query_opt(
                    "SELECT exploration_id FROM tasks WHERE id=$1",
                    &[&input.task_id],
                )
                .await?;
            let exp_id: i64 = row
                .ok_or_else(|| Error::msg("no rows in result set"))?
                .get(0);
            if exp_id != input.exploration_id {
                return Err(Error::msg(
                    "the finding's task does not match the exploration record",
                ));
            }
        }
        if input.intent_id > 0 {
            let ok: bool = tx
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM exploration_nodes WHERE id=$1 AND exploration_id=$2 AND kind='intent')",
                    &[&input.intent_id, &input.exploration_id],
                )
                .await?
                .get(0);
            if !ok {
                return Err(Error::msg(
                    "intent_id must be an intent of this task (intents of associated tasks are read-only)",
                ));
            }
        }
        let payload = serde_json::json!({
            "vulnclass": input.vulnclass,
            "name": input.name,
            "severity": input.severity,
            "summary": input.summary,
            "evidence": {"by": input.worker, "poc": input.evidence},
        });
        let mut out = RecordedFinding::default();
        let node_id: i64 = tx
            .query_one(
                "INSERT INTO exploration_nodes(exploration_id,kind,payload,priority,state,origin) \
                 VALUES($1,'finding',$2,9,'confirmed',$3) RETURNING id",
                &[&input.exploration_id, &payload, &input.worker],
            )
            .await?
            .get(0);
        out.node_id = node_id;
        for asset in &input.asset_ids {
            tx.execute(
                "INSERT INTO exploration_anchors(node_id,asset_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
                &[&node_id, asset],
            )
            .await?;
        }
        if input.intent_id > 0 {
            tx.execute(
                "INSERT INTO exploration_edges(exploration_id,src_id,rel,dst_id) VALUES($1,$2,$3,$4)",
                &[
                    &input.exploration_id,
                    &input.intent_id,
                    &crate::db::constants::REL_YIELDS,
                    &node_id,
                ],
            )
            .await?;
        }
        let aids = serde_json::json!(input.asset_ids);
        let task_id = input.task_id;
        let finding_id: i64 = tx
            .query_one(
                "INSERT INTO findings(task_id,node_id,vulnclass,name,severity,summary,evidence,worker,asset_ids,counterevidence,confidence_rationale) \
                 VALUES(NULLIF($1::bigint,0),$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING id",
                &[
                    &task_id,
                    &node_id,
                    &input.vulnclass,
                    &input.name,
                    &input.severity,
                    &input.summary,
                    &input.evidence,
                    &input.worker,
                    &aids,
                    &input.counterevidence,
                    &input.confidence_rationale,
                ],
            )
            .await?
            .get(0);
        out.finding_id = finding_id;

        // Same-transaction notification event: best-effort, isolated on a savepoint.
        let snap = crate::notify::Snapshot {
            kind: crate::notify::EVENT_FINDING_CREATED.to_string(),
            finding_id,
            task_id: input.task_id,
            vulnclass: input.vulnclass.clone(),
            name: input.name.clone(),
            severity: input.severity.clone(),
            summary: input.summary.clone(),
            asset_ids: input.asset_ids.clone(),
            from_status: String::new(),
            to_status: String::new(),
        };
        crate::db::notification::record_notification_event_tx(
            tx,
            crate::notify::EVENT_FINDING_CREATED,
            finding_id,
            &snap,
        )
        .await;

        self.add_finding_traffic_tx(tx, finding_id, prepared)
            .await?;
        out.traffic = finding_traffic_tx(tx, finding_id).await?;
        Ok(out)
    }

    /// `RecordFinding`: standalone-transaction wrapper (no pre-prepared evidence),
    /// binding the exploration id from `exp_id`.
    pub async fn record_finding(
        &self,
        exp_id: i64,
        mut input: RecordFindingInput,
    ) -> Result<RecordedFinding> {
        input.exploration_id = exp_id;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&FINDING_EVIDENCE_LOCK_KEY],
        )
        .await?;
        let result = self.record_finding_tx(&tx, &input, &[]).await;
        match result {
            Ok(out) => {
                tx.commit().await?;
                Ok(out)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}

impl crate::db::exploration::ExplorationStore {
    /// `RecordFinding`: the legacy/no-recorder path used by tools and tests —
    /// records a finding atomically under the evidence lock, no pre-prepared traffic.
    pub async fn record_finding(&self, input: RecordFindingInput) -> Result<RecordedFinding> {
        self.db().record_finding(self.id(), input).await
    }
}
