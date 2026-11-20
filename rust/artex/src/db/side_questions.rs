//! Side-question persistence — port of `db/side_questions.go`.
//!
//! An immutable main-agent checkpoint plus idempotent, generation-scoped Q&A
//! exchanges. A generation bump (history clear) makes late provider callbacks
//! unable to resurrect old history.

use serde_json::Value as Json;

use crate::db::error::{Error, Result};
use crate::db::DB;
use crate::sidequestion::{Exchange, Memory, Snapshot};

pub const ERR_SIDE_BUSY: &str = "the current session already has a side question being answered";
pub const ERR_SIDE_PARENT_GONE: &str =
    "the side question's parent session has been deleted or archived";

const SIDE_COLS: &str = "id,ordinal,session_key,generation,client_id,question,answer,status,error,model,snapshot_at,created_at,sequence,usage,context_info";

fn json_val<T: serde::Serialize>(v: &T) -> Result<Json> {
    serde_json::to_value(v).map_err(|e| Error::msg(e.to_string()))
}

fn nullable_side_id(id: i64) -> Option<i64> {
    if id == 0 {
        None
    } else {
        Some(id)
    }
}

/// UUID v4 from `rand` (Go uses `uuid.NewString()`); format is all that matters —
/// the value is an opaque primary key.
fn new_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Legacy rows store `usage = '{}'`; `Usage` has no serde defaults, so fill
/// fields individually (mirrors Go's zero-valued struct on unmarshal).
fn usage_from(v: &Json) -> crate::llm::Usage {
    let mut u = crate::llm::Usage::default();
    if let Some(x) = v.get("input_tokens").and_then(|x| x.as_i64()) {
        u.input_tokens = x;
    }
    if let Some(x) = v.get("output_tokens").and_then(|x| x.as_i64()) {
        u.output_tokens = x;
    }
    if let Some(x) = v.get("cache_read_tokens").and_then(|x| x.as_i64()) {
        u.cache_read_tokens = x;
    }
    if let Some(x) = v.get("cache_write_tokens").and_then(|x| x.as_i64()) {
        u.cache_write_tokens = x;
    }
    u
}

fn scan_side(row: &tokio_postgres::Row) -> Result<Exchange> {
    let model: Json = row.get(9);
    let usage: Json = row.get(13);
    let info: Json = row.get(14);
    let e = Exchange {
        id: row.get(0),
        ordinal: row.get(1),
        session_key: row.get(2),
        generation: row.get(3),
        client_id: row.get(4),
        question: row.get(5),
        answer: row.get(6),
        status: row.get(7),
        error: row.get(8),
        model: serde_json::from_value(model).map_err(|e| Error::msg(e.to_string()))?,
        snapshot_at: crate::sidequestion::GoTime::from_system_time(row.get(10)),
        created_at: crate::sidequestion::GoTime::from_system_time(row.get(11)),
        sequence: row.get(12),
        usage: usage_from(&usage),
        context: serde_json::from_value(info).map_err(|e| Error::msg(e.to_string()))?,
    };
    Ok(e)
}

/// Lock the real parent before the side session, also covering soft task/intent
/// deletion, so a delayed checkpoint cannot recreate data after archive cleanup.
async fn lock_side_parent(
    tx: &deadpool_postgres::Transaction<'_>,
    p: &crate::sidequestion::Parent,
) -> Result<()> {
    let row = if p.conversation_id > 0 {
        tx.query_opt(
            "SELECT id FROM conversations WHERE id=$1 FOR SHARE",
            &[&p.conversation_id],
        )
        .await?
    } else {
        let r = tx.query_opt(
            "SELECT id FROM tasks WHERE id=$1 AND exploration_id=$2 AND deleted_at IS NULL AND archived_at IS NULL FOR SHARE",
            &[&p.task_id, &p.exploration_id],
        ).await?;
        if r.is_some() && p.intent_id > 0 {
            tx.query_opt(
                "SELECT id FROM exploration_nodes WHERE id=$1 AND exploration_id=$2 AND kind='intent' AND state<>'stopped' FOR SHARE",
                &[&p.intent_id, &p.exploration_id],
            ).await?
        } else {
            r
        }
    };
    if row.is_none() {
        return Err(Error::sentinel(ERR_SIDE_PARENT_GONE, ERR_SIDE_PARENT_GONE));
    }
    Ok(())
}

impl DB {
    pub async fn save_side_snapshot(&self, s: &Snapshot) -> Result<()> {
        let body = json_val(s)?;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        lock_side_parent(&tx, &s.parent).await?;
        tx.execute(
            "INSERT INTO side_question_sessions(session_key,conversation_id,task_id,exploration_id,intent_id,run_id,version,snapshot)
VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(session_key) DO UPDATE SET run_id=EXCLUDED.run_id,version=EXCLUDED.version,snapshot=EXCLUDED.snapshot
WHERE (side_question_sessions.run_id,side_question_sessions.version)<(EXCLUDED.run_id,EXCLUDED.version)",
            &[
                &s.parent.key(), &nullable_side_id(s.parent.conversation_id), &nullable_side_id(s.parent.task_id),
                &nullable_side_id(s.parent.exploration_id), &nullable_side_id(s.parent.intent_id),
                &s.run_id, &s.version, &body,
            ],
        ).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn side_snapshot(&self, key: &str) -> Result<Option<Snapshot>> {
        let row = self
            .query_opt(
                "SELECT snapshot FROM side_question_sessions WHERE session_key=$1",
                &[&key],
            )
            .await?;
        let Some(row) = row else { return Ok(None) };
        let body: Json = row.get(0);
        Ok(Some(
            serde_json::from_value(body).map_err(|e| Error::msg(e.to_string()))?,
        ))
    }

    pub async fn side_request(&self, id: &str) -> Result<Option<Exchange>> {
        let row = self
            .query_opt(
                &format!("SELECT {SIDE_COLS} FROM side_question_requests WHERE id=$1"),
                &[&id],
            )
            .await?;
        row.as_ref().map(scan_side).transpose()
    }

    pub async fn current_side_request(&self, key: &str) -> Result<Option<Exchange>> {
        let row = self.query_opt(
            &format!("SELECT {SIDE_COLS} FROM side_question_requests WHERE session_key=$1 AND status='running'"),
            &[&key],
        ).await?;
        row.as_ref().map(scan_side).transpose()
    }

    pub async fn side_history(&self, key: &str, before: i64, limit: i64) -> Result<Vec<Exchange>> {
        let rows = self.query(
            &format!("SELECT {SIDE_COLS} FROM side_question_requests WHERE session_key=$1 AND ($2::bigint=0 OR ordinal<$2) ORDER BY ordinal DESC LIMIT $3"),
            &[&key, &before, &limit],
        ).await?;
        rows.iter().map(scan_side).collect()
    }

    pub async fn side_replay(&self, key: &str) -> Result<Vec<Exchange>> {
        let rows = self.query(
            &format!("SELECT {SIDE_COLS} FROM side_question_requests WHERE session_key=$1 AND status='completed' ORDER BY ordinal DESC LIMIT 20"),
            &[&key],
        ).await?;
        let mut out: Vec<Exchange> = rows.iter().map(scan_side).collect::<Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    pub async fn start_side_request(
        &self,
        s: &Snapshot,
        client_id: &str,
        question: &str,
    ) -> Result<(Exchange, bool)> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        lock_side_parent(&tx, &s.parent).await?;
        let key = s.parent.key();
        let gen_row = tx
            .query_opt(
                "SELECT generation FROM side_question_sessions WHERE session_key=$1 FOR UPDATE",
                &[&key],
            )
            .await?;
        let Some(gen_row) = gen_row else {
            return Err(Error::sentinel(ERR_SIDE_PARENT_GONE, ERR_SIDE_PARENT_GONE));
        };
        let generation: i64 = gen_row.get(0);
        let existing = tx.query_opt(
            &format!("SELECT {SIDE_COLS} FROM side_question_requests WHERE session_key=$1 AND generation=$2 AND client_id=$3"),
            &[&key, &generation, &client_id],
        ).await?;
        if let Some(row) = existing {
            let e = scan_side(&row)?;
            if e.question != question {
                return Err(Error::msg(
                    "the same request ID cannot be used for different questions",
                ));
            }
            tx.commit().await?;
            return Ok((e, false));
        }
        let busy: bool = tx.query_one(
            "SELECT EXISTS(SELECT 1 FROM side_question_requests WHERE session_key=$1 AND status='running')",
            &[&key],
        ).await?.get(0);
        if busy {
            return Err(Error::sentinel(ERR_SIDE_BUSY, ERR_SIDE_BUSY));
        }
        let model = json_val(&s.model)?;
        let snapshot_at = s
            .captured_at
            .as_system_time()
            .unwrap_or(std::time::UNIX_EPOCH);
        let id = new_id();
        let row = tx.query_one(
            &format!("INSERT INTO side_question_requests(id,session_key,generation,client_id,question,status,model,snapshot_at) VALUES($1,$2,$3,$4,$5,'running',$6,$7) RETURNING {SIDE_COLS}"),
            &[&id, &key, &generation, &client_id, &question, &model, &snapshot_at],
        ).await?;
        let e = scan_side(&row)?;
        tx.commit().await?;
        Ok((e, true))
    }

    pub async fn update_side_request(&self, e: &Exchange) -> Result<bool> {
        let usage = json_val(&e.usage)?;
        let info = json_val(&e.context)?;
        let n = self.execute(
            "UPDATE side_question_requests r SET answer=$2,status=$3,error=$4,sequence=$5,usage=$6,context_info=$7
WHERE r.id=$1 AND r.status='running' AND r.sequence<$5 AND EXISTS(SELECT 1 FROM side_question_sessions s WHERE s.session_key=r.session_key AND s.generation=r.generation)",
            &[&e.id, &e.answer, &e.status, &e.error, &e.sequence, &usage, &info],
        ).await?;
        Ok(n == 1)
    }

    pub async fn clear_side_history(&self, key: &str) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "UPDATE side_question_sessions SET generation=generation+1,memory='{}' WHERE session_key=$1",
            &[&key],
        ).await?;
        tx.execute(
            "DELETE FROM side_question_requests WHERE session_key=$1",
            &[&key],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn side_memory(&self, e: &Exchange) -> Result<Memory> {
        let row = self.query_one(
            "SELECT s.memory FROM side_question_sessions s JOIN side_question_requests r ON r.session_key=s.session_key
WHERE r.id=$1 AND r.generation=s.generation AND s.generation=$2 AND r.status='running'",
            &[&e.id, &e.generation],
        ).await?;
        let raw: Json = row.get(0);
        serde_json::from_value(raw).map_err(|e2| Error::msg(e2.to_string()))
    }

    pub async fn side_replay_page(&self, e: &Exchange, after: i64) -> Result<Vec<Exchange>> {
        let rows = self.query(
            &format!("SELECT {SIDE_COLS} FROM side_question_requests WHERE session_key=$1 AND generation=$2
AND status='completed' AND ordinal>$3 AND ordinal<$4 ORDER BY ordinal LIMIT 20"),
            &[&e.session_key, &e.generation, &after, &e.ordinal],
        ).await?;
        rows.iter().map(scan_side).collect()
    }

    pub async fn save_side_memory(&self, e: &Exchange, memory: &Memory) -> Result<()> {
        let raw = json_val(memory)?;
        let n = self.execute(
            "UPDATE side_question_sessions s SET memory=$3 WHERE s.session_key=$1 AND s.generation=$2
AND EXISTS(SELECT 1 FROM side_question_requests r WHERE r.id=$4 AND r.session_key=s.session_key AND r.generation=s.generation AND r.status='running')",
            &[&e.session_key, &e.generation, &raw, &e.id],
        ).await?;
        if n == 0 {
            return Err(Error::sentinel(ERR_SIDE_PARENT_GONE, ERR_SIDE_PARENT_GONE));
        }
        Ok(())
    }

    pub async fn interrupt_side_requests(&self) -> Result<()> {
        self.execute(
            "UPDATE side_question_requests SET status='interrupted',error='service restarted; the answer was interrupted',sequence=sequence+1 WHERE status='running'",
            &[],
        ).await?;
        Ok(())
    }
}
