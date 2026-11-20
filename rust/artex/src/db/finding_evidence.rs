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
#[derive(Debug, Clone, PartialEq)]
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
    pub created_at: std::time::SystemTime,
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
