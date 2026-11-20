//! Traffic index foundation — SQLite-backed exchange store (Go `traffic.go`).
//! Bodies live inline up to 256 KiB; larger ones spill to a content-addressed
//! blob store while a preview stays in SQLite and FTS indexes the full text.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::db::error::{Error, Result};
use crate::traffic::pure_helpers::{binary_tag, is_binary_body, truncate_utf8};
use crate::traffic::sqlite::{Conn, Value};
use sha2::Digest;

pub const MAX_INLINE_BODY: usize = 256 * 1024;
pub const BLOB_PREVIEW: usize = 8 * 1024;
pub const MAX_INDEX_BODY: usize = 4 * 1024 * 1024;
pub const MIN_TRIGRAM: usize = 3;

pub const INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS exchanges (
  id TEXT PRIMARY KEY, ts INTEGER, host TEXT, method TEXT, url_template TEXT,
  url TEXT, status INTEGER, content_type TEXT, req_len INTEGER, resp_len INTEGER, path TEXT
);
CREATE INDEX IF NOT EXISTS idx_ex_host ON exchanges(host);
CREATE INDEX IF NOT EXISTS idx_ex_tmpl ON exchanges(host, url_template);
CREATE INDEX IF NOT EXISTS idx_ex_ts ON exchanges(ts);
CREATE INDEX IF NOT EXISTS idx_ex_status ON exchanges(status);
CREATE INDEX IF NOT EXISTS idx_ex_resp ON exchanges(resp_len);
CREATE TABLE IF NOT EXISTS exchange_bodies (
  id TEXT PRIMARY KEY, req_head TEXT NOT NULL DEFAULT '', req_body BLOB, req_blob TEXT,
  resp_head TEXT NOT NULL DEFAULT '', resp_body BLOB, resp_blob TEXT
);
CREATE TABLE IF NOT EXISTS blob_refs (
  hash TEXT NOT NULL, exchange_id TEXT NOT NULL, PRIMARY KEY (hash, exchange_id)
);
CREATE INDEX IF NOT EXISTS idx_blob_refs_ex ON blob_refs(exchange_id);
CREATE TABLE IF NOT EXISTS stream_messages (
  exchange_id TEXT NOT NULL, seq INTEGER NOT NULL, ts INTEGER NOT NULL,
  direction TEXT NOT NULL, kind INTEGER NOT NULL, event TEXT NOT NULL DEFAULT '',
  event_id TEXT NOT NULL DEFAULT '', retry INTEGER NOT NULL DEFAULT 0,
  size INTEGER NOT NULL, payload BLOB, payload_blob TEXT, truncated INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (exchange_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_stream_messages_ex ON stream_messages(exchange_id, seq);
CREATE TABLE IF NOT EXISTS exchange_rewrites (
  exchange_id TEXT NOT NULL, seq INTEGER NOT NULL, phase TEXT NOT NULL,
  rule_id INTEGER, rule_snapshot TEXT, action TEXT, field TEXT,
  outcome TEXT NOT NULL, reason TEXT NOT NULL DEFAULT '', before_hash TEXT, after_hash TEXT,
  after_inline BLOB, after_blob TEXT, PRIMARY KEY (exchange_id, seq)
);
"#;

pub const FTS_SCHEMA: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS ex_fts USING fts5(content, tokenize='trigram', content='', contentless_delete=1);";

/// One body after the inline/spill decision.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredBody {
    pub inline: Vec<u8>,
    pub hash: String,
    pub index: String,
}

/// Metadata needed to insert one exchange row.
#[derive(Debug, Clone, Default)]
pub struct ExchangeMeta {
    pub id: String,
    pub ts: i64,
    pub host: String,
    pub method: String,
    pub url_template: String,
    pub url: String,
    pub status: i32,
    pub content_type: String,
    pub req_len: i64,
    pub resp_len: i64,
}

pub struct TrafficStore {
    dir: PathBuf,
    db: Mutex<Conn>,
    seq: AtomicU64,
    fts: bool,
}

fn null_if_empty(s: &str) -> Value {
    if s.is_empty() {
        Value::Null
    } else {
        Value::Text(s.to_string())
    }
}

impl TrafficStore {
    /// `Open(dir)`: create directory layout, apply pragmas/schema on one pinned
    /// connection, and initialize FTS (degrades to fts=false if unavailable).
    pub fn open(dir: &Path) -> Result<TrafficStore> {
        for d in [
            dir.to_path_buf(),
            dir.join("_index"),
            dir.join("_blobs"),
            dir.join("_ca"),
        ] {
            std::fs::create_dir_all(&d).map_err(|e| Error::msg(e.to_string()))?;
        }
        let index = dir.join("_index").join("index.sqlite");
        let db = Conn::open(&index.to_string_lossy())?;
        // auto_vacuum + VACUUM must run before any table exists, on this same
        // connection, so the setting is written into the file header.
        if !index.exists()
            || std::fs::metadata(&index)
                .map(|m| m.len() == 0)
                .unwrap_or(false)
        {
            db.exec("PRAGMA auto_vacuum=incremental")?;
            db.exec("VACUUM")?;
        }
        db.exec("PRAGMA busy_timeout=5000")?;
        db.exec("PRAGMA journal_mode=WAL")?;
        db.exec(INDEX_SCHEMA)?;
        let fts = db.exec(FTS_SCHEMA).is_ok();
        Ok(TrafficStore {
            dir: dir.to_path_buf(),
            db: Mutex::new(db),
            seq: AtomicU64::new(0),
            fts,
        })
    }

    pub fn open_temp() -> Result<TrafficStore> {
        let dir = std::env::temp_dir()
            .join(format!("artex-traffic-{}", std::process::id()))
            .join(format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        TrafficStore::open(&dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn fts(&self) -> bool {
        self.fts
    }

    pub fn new_exchange_id(&self, now_secs: i64) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        crate::traffic::query_helpers::exchange_id(now_secs, n)
    }

    /// `spillAt`: small bodies stay inline; large ones are written to
    /// `_blobs/sha256/<2>/<hash>.bin` with a readable preview kept inline.
    pub fn spill_at(&self, body: &[u8], content_type: &str, max_inline: usize) -> StoredBody {
        if body.is_empty() {
            return StoredBody::default();
        }
        let text = !is_binary_body(content_type, body);
        let index_text = || {
            if text {
                String::from_utf8_lossy(&body[..body.len().min(MAX_INDEX_BODY)]).into_owned()
            } else {
                String::new()
            }
        };
        if body.len() <= max_inline {
            return StoredBody {
                inline: body.to_vec(),
                index: index_text(),
                ..StoredBody::default()
            };
        }
        let sum = sha2::Sha256::digest(body);
        let h: String = sum.iter().map(|b| format!("{b:02x}")).collect();
        let blob_dir = self.dir.join("_blobs").join("sha256").join(&h[..2]);
        let blob_path = blob_dir.join(format!("{h}.bin"));
        if let Err(e) = std::fs::create_dir_all(&blob_dir) {
            eprintln!("[traffic] blob directory: {e}");
            return StoredBody {
                inline: body[..body.len().min(BLOB_PREVIEW)].to_vec(),
                index: index_text(),
                ..StoredBody::default()
            };
        }
        if !blob_path.exists() && std::fs::write(&blob_path, body).is_err() {
            return StoredBody {
                inline: body[..body.len().min(BLOB_PREVIEW)].to_vec(),
                index: index_text(),
                ..StoredBody::default()
            };
        }
        let inline = if text {
            truncate_utf8(body, BLOB_PREVIEW).into_bytes()
        } else {
            binary_tag(content_type, body).into_bytes()
        };
        StoredBody {
            hash: h,
            inline,
            index: index_text(),
        }
    }

    pub fn spill(&self, body: &[u8], content_type: &str) -> StoredBody {
        self.spill_at(body, content_type, MAX_INLINE_BODY)
    }

    pub fn blob_path(&self, hash: &str) -> Option<PathBuf> {
        let h = hash.trim().to_lowercase();
        if !h.chars().all(|c| c.is_ascii_hexdigit()) || h.len() != 64 {
            return None;
        }
        let current = self
            .dir
            .join("_blobs")
            .join("sha256")
            .join(&h[..2])
            .join(format!("{h}.bin"));
        if current.exists() {
            return Some(current);
        }
        let legacy = self
            .dir
            .join("_blobs")
            .join("sha256")
            .join(&h[..2])
            .join(&h[2..4])
            .join(format!("{h}.bin"));
        if legacy.exists() {
            return Some(legacy);
        }
        None
    }

    /// `insert_exchange`: metadata + bodies + blob refs + trace + FTS in one tx.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_exchange(
        &self,
        meta: &ExchangeMeta,
        req_head: &str,
        req_body: &StoredBody,
        resp_head: &str,
        resp_body: &StoredBody,
        req_len: usize,
        resp_len: usize,
        fts_text: &str,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.exec("BEGIN")?;
        let result = (|| -> Result<()> {
            db.execute(
                "DELETE FROM exchange_rewrites WHERE exchange_id=?",
                &[Value::Text(meta.id.clone())],
            )?;
            db.execute(
                "INSERT OR REPLACE INTO exchanges(id,ts,host,method,url_template,url,status,content_type,req_len,resp_len,path) VALUES(?,?,?,?,?,?,?,?,?,?,'')",
                &[
                    Value::Text(meta.id.clone()), Value::Int(meta.ts), Value::Text(meta.host.clone()),
                    Value::Text(meta.method.clone()), Value::Text(meta.url_template.clone()),
                    Value::Text(meta.url.clone()), Value::Int(meta.status as i64),
                    null_if_empty(&meta.content_type), Value::Int(req_len as i64), Value::Int(resp_len as i64),
                ],
            )?;
            let rowid = db.last_insert_rowid();
            db.execute(
                "INSERT OR REPLACE INTO exchange_bodies(id,req_head,req_body,req_blob,resp_head,resp_body,resp_blob) VALUES(?,?,?,?,?,?,?)",
                &[
                    Value::Text(meta.id.clone()), Value::Text(req_head.to_string()),
                    Value::Blob(req_body.inline.clone()), null_if_empty(&req_body.hash),
                    Value::Text(resp_head.to_string()), Value::Blob(resp_body.inline.clone()),
                    null_if_empty(&resp_body.hash),
                ],
            )?;
            for h in [&req_body.hash, &resp_body.hash] {
                if !h.is_empty() {
                    db.execute(
                        "INSERT OR IGNORE INTO blob_refs(hash,exchange_id) VALUES(?,?)",
                        &[Value::Text(h.clone()), Value::Text(meta.id.clone())],
                    )?;
                }
            }
            if self.fts {
                db.execute(
                    "INSERT INTO ex_fts(rowid,content) VALUES(?,?)",
                    &[Value::Int(rowid), Value::Text(fts_text.to_string())],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                db.exec("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = db.exec("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<crate::traffic::sqlite::Row>> {
        let db = self.db.lock().unwrap();
        db.query(sql, params)
    }

    pub fn count(&self) -> Result<i64> {
        let db = self.db.lock().unwrap();
        Ok(db.query("SELECT COUNT(*) FROM exchanges", &[])?[0].int(0))
    }

    /// `Get`: rebuild raw request/response text from head + inline + blob marker.
    pub fn get(&self, id: &str) -> Result<(String, String)> {
        let db = self.db.lock().unwrap();
        let rows = db.query(
            "SELECT b.req_head,b.req_body,b.req_blob,b.resp_head,b.resp_body,b.resp_blob,e.req_len,e.resp_len \
             FROM exchange_bodies b JOIN exchanges e ON e.id=b.id WHERE b.id=?",
            &[Value::Text(id.to_string())],
        )?;
        if let Some(r) = rows.first() {
            return Ok((
                assemble_raw(&r.text(0), &r.blob(1), r.opt_text(2), r.int(6)),
                assemble_raw(&r.text(3), &r.blob(4), r.opt_text(5), r.int(7)),
            ));
        }
        let legacy = db.query(
            "SELECT path FROM exchanges WHERE id=?",
            &[Value::Text(id.to_string())],
        )?;
        let Some(r) = legacy.first() else {
            return Err(Error::msg(format!("exchange {id} has no body record")));
        };
        let rel = r.text(0);
        if rel.trim().is_empty() {
            return Err(Error::msg(format!("exchange {id} has no body record")));
        }
        let req =
            std::fs::read_to_string(self.dir.join(&rel).join("request.http")).unwrap_or_default();
        let resp =
            std::fs::read_to_string(self.dir.join(&rel).join("response.http")).unwrap_or_default();
        Ok((req, resp))
    }
}

/// `assembleRaw`: head + blank line + body, plus a blob pointer when spilled.
fn assemble_raw(head: &str, body: &[u8], blob: Option<String>, total: i64) -> String {
    let mut out = String::new();
    out.push_str(head);
    out.push('\n');
    out.push_str(&String::from_utf8_lossy(body));
    if let Some(h) = blob {
        if !h.is_empty() {
            out.push_str(&format!("\n…[truncated] @blob sha256:{h} (len={total})"));
        }
    }
    out
}
