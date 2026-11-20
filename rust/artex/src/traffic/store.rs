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
    /// Non-empty only for legacy rows whose bodies live on disk.
    pub path: String,
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

/// Optional filters for `page` (all combined with AND).
#[derive(Debug, Clone)]
pub struct PageQuery {
    pub host: String,
    pub method: String,
    pub query: String,
    pub body: String,
    pub path: String,
    pub status: String,
    /// -1 skips the bound.
    pub resp_min: i64,
    /// -1 skips the bound.
    pub resp_max: i64,
    pub sort: String,
    pub order: String,
}

impl Default for PageQuery {
    fn default() -> Self {
        PageQuery {
            host: String::new(),
            method: String::new(),
            query: String::new(),
            body: String::new(),
            path: String::new(),
            status: String::new(),
            resp_min: -1,
            resp_max: -1,
            sort: String::new(),
            order: String::new(),
        }
    }
}

fn scan_meta(r: &crate::traffic::sqlite::Row) -> ExchangeMeta {
    ExchangeMeta {
        id: r.text(0),
        ts: r.int(1),
        host: r.text(2),
        method: r.text(3),
        url_template: r.text(4),
        url: r.text(5),
        status: r.int(6) as i32,
        content_type: r.text(7),
        resp_len: r.int(8),
        path: r.text(9),
        req_len: 0,
    }
}

impl TrafficStore {
    /// `search`: legacy host-filtered newest-first listing.
    pub fn search(&self, host: &str, page: i64, size: i64) -> Result<Vec<ExchangeMeta>> {
        let size = if size <= 0 || size > 500 { 100 } else { size };
        let db = self.db.lock().unwrap();
        let (sql, params): (&str, Vec<Value>) = if host.is_empty() {
            ("SELECT id,ts,host,method,url_template,url,status,content_type,resp_len,path FROM exchanges ORDER BY ts DESC LIMIT ? OFFSET ?",
             vec![Value::Int(size), Value::Int(page * size)])
        } else {
            ("SELECT id,ts,host,method,url_template,url,status,content_type,resp_len,path FROM exchanges WHERE host=? ORDER BY ts DESC LIMIT ? OFFSET ?",
             vec![Value::Text(host.to_string()), Value::Int(size), Value::Int(page * size)])
        };
        Ok(db.query(sql, &params)?.iter().map(scan_meta).collect())
    }

    /// `page`: filtered, sorted, stable pagination with a total count.
    pub fn page(&self, f: &PageQuery, page: i64, size: i64) -> Result<(Vec<ExchangeMeta>, i64)> {
        let size = if size <= 0 || size > 500 { 100 } else { size };
        let page = page.max(0);
        let mut conds: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        let mut add = |cond: &str, vs: Vec<Value>| {
            conds.push(cond.to_string());
            args.extend(vs);
        };
        if let Some(h) = Some(f.host.trim()).filter(|s| !s.is_empty()) {
            add("host LIKE ?", vec![Value::Text(format!("%{h}%"))]);
        }
        if let Some(m) = Some(f.method.trim()).filter(|s| !s.is_empty()) {
            add("method=?", vec![Value::Text(m.to_uppercase())]);
        }
        if let Some(p) = Some(f.path.trim()).filter(|s| !s.is_empty()) {
            add("url_template LIKE ?", vec![Value::Text(format!("%{p}%"))]);
        }
        if let Some((cond, vs)) = crate::traffic::query_helpers::status_filter(&f.status) {
            add(cond, vs.into_iter().map(|v| Value::Int(v as i64)).collect());
        }
        if f.resp_min >= 0 {
            add("resp_len>=?", vec![Value::Int(f.resp_min)]);
        }
        if f.resp_max >= 0 {
            add("resp_len<=?", vec![Value::Int(f.resp_max)]);
        }

        const META: &str = "host LIKE ? OR url LIKE ? OR url_template LIKE ? OR method LIKE ? OR content_type LIKE ? OR CAST(status AS TEXT) LIKE ?";
        if let Some(s) = Some(f.query.trim()).filter(|s| !s.is_empty()) {
            let like = Value::Text(format!("%{s}%"));
            if let Some((cond, arg)) = crate::traffic::query_helpers::fts_filter(self.fts, s) {
                let mut vs = vec![like.clone(); 6];
                vs.push(Value::Text(arg));
                add(&format!("(({META}) OR {cond})"), vs);
            } else {
                add(&format!("({META})"), vec![like; 6]);
            }
        }
        if let Some(b) = Some(f.body.trim()).filter(|s| !s.is_empty()) {
            if let Some((cond, arg)) = crate::traffic::query_helpers::fts_filter(self.fts, b) {
                add(&cond, vec![Value::Text(arg)]);
            }
        }
        let where_sql = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };

        let db = self.db.lock().unwrap();
        let total =
            db.query(&format!("SELECT COUNT(*) FROM exchanges{where_sql}"), &args)?[0].int(0);
        let col = crate::traffic::query_helpers::sort_column(&f.sort.to_lowercase());
        let dir = if f.order.trim().eq_ignore_ascii_case("asc") {
            "ASC"
        } else {
            "DESC"
        };
        let mut qargs = args;
        qargs.push(Value::Int(size));
        qargs.push(Value::Int(page * size));
        let sql = format!(
            "SELECT id,ts,host,method,url_template,url,status,content_type,resp_len,path FROM exchanges{where_sql} ORDER BY {col} {dir}, id DESC LIMIT ? OFFSET ?"
        );
        let rows = db.query(&sql, &qargs)?.iter().map(scan_meta).collect();
        Ok((rows, total))
    }

    /// `BlobRange`: read a bounded slice of a content-addressed blob.
    pub fn blob_range(&self, hash: &str, offset: i64, length: i64) -> Result<(Vec<u8>, i64)> {
        let path = self
            .blob_path(hash)
            .ok_or_else(|| Error::msg(format!("blob {hash:?} not found")))?;
        let size = std::fs::metadata(&path)
            .map_err(|e| Error::msg(e.to_string()))?
            .len() as i64;
        let (off, n, empty) = crate::traffic::pure_helpers::clamp_blob_range(offset, length, size);
        if empty {
            return Ok((Vec::new(), size));
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path).map_err(|e| Error::msg(e.to_string()))?;
        f.seek(SeekFrom::Start(off as u64))
            .map_err(|e| Error::msg(e.to_string()))?;
        let mut buf = vec![0u8; n as usize];
        let mut read = 0usize;
        while read < buf.len() {
            let k = f
                .read(&mut buf[read..])
                .map_err(|e| Error::msg(e.to_string()))?;
            if k == 0 {
                break;
            }
            read += k;
        }
        Ok((buf[..read].to_vec(), size))
    }
}

impl TrafficStore {
    /// Delete all rows matching `where` (and their child rows) in one tx.
    /// `where` is a caller-owned fixed SQL fragment with `?` placeholders.
    fn delete_where(&self, where_sql: &str, params: &[Value]) -> Result<i64> {
        let db = self.db.lock().unwrap();
        db.exec("BEGIN")?;
        let result = (|| -> Result<i64> {
            if self.fts {
                db.execute(&format!("DELETE FROM ex_fts WHERE rowid IN (SELECT rowid FROM exchanges WHERE {where_sql})"), params)?;
            }
            for table in [
                "exchange_bodies",
                "stream_messages",
                "exchange_rewrites",
                "blob_refs",
            ] {
                let key = if table == "exchange_bodies" {
                    "id"
                } else {
                    "exchange_id"
                };
                db.execute(&format!("DELETE FROM {table} WHERE {key} IN (SELECT id FROM exchanges WHERE {where_sql})"), params)?;
            }
            Ok(db.execute(&format!("DELETE FROM exchanges WHERE {where_sql}"), params)? as i64)
        })();
        match result {
            Ok(n) => {
                db.exec("COMMIT")?;
                Ok(n)
            }
            Err(e) => {
                let _ = db.exec("ROLLBACK");
                Err(e)
            }
        }
    }

    /// `DeleteHost`: remove every exchange whose host contains `host`.
    pub fn delete_host(&self, host: &str) -> Result<i64> {
        let n = self.delete_where("host LIKE ?", &[Value::Text(format!("%{host}%"))])?;
        if n > 0 {
            self.gc_blobs()?;
        }
        Ok(n)
    }

    /// `DeleteHostsExact`: remove exchanges whose host is one of `hosts`.
    pub fn delete_hosts_exact(&self, hosts: &[String]) -> Result<i64> {
        if hosts.is_empty() {
            return Ok(0);
        }
        let conds = vec!["host=?"; hosts.len()].join(" OR ");
        let params: Vec<Value> = hosts.iter().cloned().map(Value::Text).collect();
        let n = self.delete_where(&conds, &params)?;
        if n > 0 {
            self.gc_blobs()?;
        }
        Ok(n)
    }

    /// `DeleteAll`: purge every exchange and reclaim disk space.
    pub fn delete_all(&self) -> Result<(i64, i64)> {
        let before = self.index_bytes();
        let n = self.delete_where("1=1", &[])?;
        self.gc_blobs()?;
        let db = self.db.lock().unwrap();
        if self.fts {
            let _ = db.exec("INSERT INTO ex_fts(ex_fts) VALUES('optimize')");
        }
        db.exec("PRAGMA wal_checkpoint(TRUNCATE)")?;
        db.exec("VACUUM")?;
        let after = self.index_bytes();
        Ok((n, before.saturating_sub(after) as i64))
    }

    pub fn index_bytes(&self) -> u64 {
        self.dir
            .join("_index")
            .join("index.sqlite")
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// `gcBlobs`: delete unreferenced blobs and empty bucket directories.
    pub fn gc_blobs(&self) -> Result<()> {
        let mut refs = std::collections::HashSet::new();
        for row in self.query("SELECT DISTINCT hash FROM blob_refs", &[])? {
            if let Some(h) = row.opt_text(0) {
                refs.insert(h);
            }
        }
        // Legacy .http files may still point at blobs.
        let legacy: i64 =
            self.query("SELECT COUNT(*) FROM exchanges WHERE path<>''", &[])?[0].int(0);
        if legacy > 0 {
            let re = regex::Regex::new(r"@blob sha256:([0-9a-f]{64})").unwrap();
            for entry in walk_dir(&self.dir) {
                let name = entry
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if name != "request.http" && name != "response.http" {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(&entry) {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        for cap in re.captures_iter(text) {
                            if let Some(m) = cap.get(1) {
                                refs.insert(m.as_str().to_string());
                            }
                        }
                    }
                }
            }
        }
        let root = self.dir.join("_blobs").join("sha256");
        let mut buckets = Vec::new();
        for entry in walk_dir(&root) {
            if entry.is_dir() {
                if entry != root {
                    buckets.push(entry);
                }
                continue;
            }
            let h = entry
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .trim_end_matches(".bin")
                .to_string();
            if !refs.contains(&h) {
                let _ = std::fs::remove_file(&entry);
            }
        }
        buckets.sort_by_key(|b| std::cmp::Reverse(b.to_string_lossy().len()));
        for b in buckets {
            let _ = std::fs::remove_dir(&b);
        }
        Ok(())
    }
}

fn walk_dir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_dir(&p));
        }
        out.push(p);
    }
    out
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
