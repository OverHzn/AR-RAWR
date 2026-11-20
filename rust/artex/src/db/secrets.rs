//! At-rest secret encryption — port of `db/secrets.go`.
//!
//! Secrets are sealed with AES-256-GCM, a fresh random nonce per value, and AAD
//! `"<table>.<column>"` (`"settings.<key>"` for a secret setting). A `TEXT` column
//! stores `"enc:v1:<base64(nonce||ciphertext||tag)>"`; a JSONB column stores
//! `{"__artex_enc":"enc:v1:..."}` over the canonical JSON. Empty values stay
//! empty. A value that fails to decrypt is an error, never returned as-is.
//!
//! This is the self-contained crypto core (no DB access): the secret box, the
//! column model, the global active box, and the key-derivation helpers. The
//! DB-bound pieces (`InitSecrets`, `loadSecretKey`, `backfillSecrets`) land with
//! the connection layer.

use std::sync::{Arc, RwLock};

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm,
};
use serde_json::Value;

/// `secretPrefix` — a sealed `TEXT` value starts with this.
pub const SECRET_PREFIX: &str = "enc:v1:";
/// `sealedJSONKey` — the lone key of a sealed JSONB envelope.
pub const SEALED_JSON_KEY: &str = "__artex_enc";
/// `secretKeyEnv` — the env var holding the base64/hex key.
pub const SECRET_KEY_ENV: &str = "ARTEX_SECRET_KEY";
/// `secretKeyFile` — the key file name under `keyDir`.
pub const SECRET_KEY_FILE: &str = "secret.key";
/// `trafficTraceDomain` — HKDF/HMAC domain separating the rewrite-trace key.
pub const TRAFFIC_TRACE_DOMAIN: &str = "artex-traffic-trace";

/// `secretColumn` — one encrypted column: a table column, or a `settings` row
/// whose `setting` names its key (`""` for a plain table column).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretColumn {
    pub table: &'static str,
    pub col: &'static str,
    pub json: bool,
    pub setting: &'static str,
}

impl SecretColumn {
    /// `aad` — the additional-authenticated-data string for this column.
    pub fn aad(&self) -> String {
        if !self.setting.is_empty() {
            return format!("settings.{}", self.setting);
        }
        format!("{}.{}", self.table, self.col)
    }

    /// `rowFilter` — limits a `settings` entry to its own row; `tools.exec`
    /// covers the user-created tools only. Returns "" for ordinary columns.
    pub fn row_filter(&self) -> String {
        if !self.setting.is_empty() {
            return format!(" AND key='{}'", self.setting);
        }
        if *self == COL_TOOL_EXEC {
            return " AND system=false".to_string();
        }
        String::new()
    }

    /// `idCol` — the row key: settings and tools rows are keyed by name, the rest
    /// by id.
    pub fn id_col(&self) -> &'static str {
        if !self.setting.is_empty() || *self == COL_TOOL_EXEC {
            "key"
        } else {
            "id"
        }
    }

    /// `valueExpr` — the SQL text that holds the ciphertext, if any.
    pub fn value_expr(&self) -> String {
        if self.json {
            format!("{}->>'{}'", self.col, SEALED_JSON_KEY)
        } else {
            self.col.to_string()
        }
    }
}

/// `colLLMKey` etc. — the encrypted table columns.
pub const COL_LLM_KEY: SecretColumn = SecretColumn {
    table: "llm_profiles",
    col: "api_key",
    json: false,
    setting: "",
};
pub const COL_LLM_PROXY: SecretColumn = SecretColumn {
    table: "llm_profiles",
    col: "proxy",
    json: false,
    setting: "",
};
pub const COL_MCP_ENV: SecretColumn = SecretColumn {
    table: "mcp_servers",
    col: "env",
    json: true,
    setting: "",
};
pub const COL_CRED_PASS: SecretColumn = SecretColumn {
    table: "task_credentials",
    col: "password",
    json: false,
    setting: "",
};
pub const COL_NOTIF_CONFIG: SecretColumn = SecretColumn {
    table: "notification_channels",
    col: "config",
    json: true,
    setting: "",
};
pub const COL_TOOL_EXEC: SecretColumn = SecretColumn {
    table: "tools",
    col: "exec",
    json: true,
    setting: "",
};
pub const COL_EV_REQ_HEAD: SecretColumn = SecretColumn {
    table: "traffic_evidence_snapshots",
    col: "req_head",
    json: false,
    setting: "",
};
pub const COL_EV_RESP_HEAD: SecretColumn = SecretColumn {
    table: "traffic_evidence_snapshots",
    col: "resp_head",
    json: false,
    setting: "",
};
pub const COL_REWRITE_MATCHERS: SecretColumn = SecretColumn {
    table: "traffic_rewrite_rules",
    col: "matchers",
    json: true,
    setting: "",
};
pub const COL_REWRITE_ACTIONS: SecretColumn = SecretColumn {
    table: "traffic_rewrite_rules",
    col: "actions",
    json: true,
    setting: "",
};

/// `secretSettingKeys` — settings stored encrypted, AAD `"settings.<key>"`.
pub const SECRET_SETTING_KEYS: &[&str] = &[
    "brave_search_api_key",
    "tavily_search_api_key",
    "keenable_search_api_key",
    "web_search_proxy",
    "global_proxy",
];

/// `secretColumns` — the table columns plus the secret settings, in order.
pub fn secret_columns() -> Vec<SecretColumn> {
    let mut out = vec![
        COL_LLM_KEY,
        COL_LLM_PROXY,
        COL_MCP_ENV,
        COL_CRED_PASS,
        COL_NOTIF_CONFIG,
        COL_TOOL_EXEC,
        COL_EV_REQ_HEAD,
        COL_EV_RESP_HEAD,
        COL_REWRITE_MATCHERS,
        COL_REWRITE_ACTIONS,
    ];
    for k in SECRET_SETTING_KEYS {
        out.push(SecretColumn {
            table: "settings",
            col: "value",
            json: false,
            setting: k,
        });
    }
    out
}

/// `secretSetting` — the secret column of a settings key, `None` when the key is
/// stored in clear.
pub fn secret_setting(key: &str) -> Option<SecretColumn> {
    secret_columns()
        .into_iter()
        .find(|c| !c.setting.is_empty() && c.setting == key)
}

/// `secretBox` — an AES-256-GCM key holder.
#[derive(Clone)]
pub struct SecretBox {
    aead: Aes256Gcm,
}

impl SecretBox {
    /// `newSecretBox` — `key` must be 32 bytes (see [`parse_secret_key`]).
    #[allow(clippy::result_unit_err)]
    pub fn new(key: &[u8]) -> Result<SecretBox, ()> {
        let aead = Aes256Gcm::new_from_slice(key).map_err(|_| ())?;
        Ok(SecretBox { aead })
    }

    /// `encrypt` — seal one `TEXT` value. Empty stays empty. Returns
    /// `"enc:v1:" + base64(nonce || ciphertext || tag)`.
    pub fn encrypt(&self, aad: &str, plain: &str) -> Result<String, String> {
        if plain.is_empty() {
            return Ok(String::new());
        }
        let nonce = random_nonce();
        let ct = self
            .aead
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plain.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| format!("encrypt {aad}: seal failed"))?;
        let mut raw = nonce.to_vec();
        raw.extend_from_slice(&ct);
        Ok(format!(
            "{SECRET_PREFIX}{}",
            gocompat::encoding::base64_std_encode(&raw)
        ))
    }

    /// `decrypt` — open one `TEXT` value.
    pub fn decrypt(&self, aad: &str, v: &str) -> Result<String, String> {
        let raw = gocompat::encoding::base64_std_decode(v.strip_prefix(SECRET_PREFIX).unwrap_or(v))
            .map_err(|_| format!("decrypt {aad}: malformed ciphertext"))?;
        let n = self.nonce_size();
        if raw.len() < n {
            return Err(format!("decrypt {aad}: malformed ciphertext"));
        }
        let plain = self
            .aead
            .decrypt(
                (&raw[..n]).into(),
                Payload {
                    msg: &raw[n..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                format!("decrypt {aad}: authentication failed (wrong key or altered value)")
            })?;
        String::from_utf8(plain).map_err(|_| format!("decrypt {aad}: plaintext is not UTF-8"))
    }

    /// `seal` — encrypt one column value. JSON columns are canonicalized first;
    /// an empty object or null stays as is, and a top-level key named like the
    /// envelope is refused.
    pub fn seal(&self, c: &SecretColumn, v: &str) -> Result<String, String> {
        if !c.json {
            return self.encrypt(&c.aad(), v);
        }
        let parsed: Value =
            serde_json::from_str(v).map_err(|_| format!("{}: invalid JSON", c.aad()))?;
        if let Some(m) = parsed.as_object() {
            if m.contains_key(SEALED_JSON_KEY) {
                return Err(format!(
                    "{}: key \"{SEALED_JSON_KEY}\" is reserved",
                    c.aad()
                ));
            }
        }
        // null or {}: nothing to hide.
        if parsed.is_null() || matches!(parsed.as_object(), Some(m) if m.is_empty()) {
            return Ok(v.to_string());
        }
        let canon = serde_json::to_string(&parsed)
            .map_err(|e| format!("{}: canonicalize: {e}", c.aad()))?;
        let enc = self.encrypt(&c.aad(), &canon)?;
        serde_json::to_string(&serde_json::Map::from_iter([(
            SEALED_JSON_KEY.to_string(),
            Value::String(enc),
        )]))
        .map_err(|e| format!("{}: encode envelope: {e}", c.aad()))
    }

    /// `open` — decrypt one column value (JSON columns unwrap the envelope).
    pub fn open(&self, c: &SecretColumn, v: &str) -> Result<String, String> {
        let v = if c.json {
            sealed_json(v).unwrap_or_default()
        } else {
            v.to_string()
        };
        self.decrypt(&c.aad(), &v)
    }

    /// Nonce size in bytes (GCM standard = 12).
    fn nonce_size(&self) -> usize {
        12
    }
}

/// `isEncrypted` — does the value carry the sealed `TEXT` prefix?
pub fn is_encrypted(v: &str) -> bool {
    v.starts_with(SECRET_PREFIX)
}

/// `sealedJSON` — if `raw` is a `{"__artex_enc":"enc:v1:..."}` envelope, the
/// ciphertext; `None` otherwise.
pub fn sealed_json(raw: &str) -> Option<String> {
    if !raw.contains(&format!("\"{SEALED_JSON_KEY}\"")) {
        return None;
    }
    let m: Value = serde_json::from_str(raw).ok()?;
    let obj = m.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let s = obj.get(SEALED_JSON_KEY)?.as_str()?;
    if is_encrypted(s) {
        Some(s.to_string())
    } else {
        None
    }
}

/// `isSealed` — is the column value currently encrypted?
pub fn is_sealed(c: &SecretColumn, v: &str) -> bool {
    if c.json {
        sealed_json(v).is_some()
    } else {
        is_encrypted(v)
    }
}

/// `errNoSecretKey` — "secrets at rest: key not initialized".
pub const ERR_NO_SECRET_KEY: &str = "secrets at rest: key not initialized";

static ACTIVE_SECRET_BOX: RwLock<Option<Arc<SecretBox>>> = RwLock::new(None);
static TRAFFIC_TRACE_KEY: RwLock<Option<Vec<u8>>> = RwLock::new(None);

/// `activeSecretBox.Store` — install the process-wide secret box (called once by
/// `InitSecrets`).
pub fn set_active_secret_box(b: SecretBox) {
    *ACTIVE_SECRET_BOX.write().unwrap() = Some(Arc::new(b));
}

/// `sealSecret` — encrypt a value before it is written to column `c`. Errors when
/// no key is initialized.
pub fn seal_secret(c: &SecretColumn, v: &str) -> Result<String, String> {
    let guard = ACTIVE_SECRET_BOX.read().unwrap();
    match guard.as_ref() {
        Some(b) => b.seal(c, v),
        None => Err(ERR_NO_SECRET_KEY.to_string()),
    }
}

/// `openSecret` — decrypt a value read from column `c`. A value that is not
/// sealed is a legacy plaintext row (the startup backfill encrypts those) and
/// passes through.
pub fn open_secret(c: &SecretColumn, v: &str) -> Result<String, String> {
    if !is_sealed(c, v) {
        return Ok(v.to_string());
    }
    let guard = ACTIVE_SECRET_BOX.read().unwrap();
    match guard.as_ref() {
        Some(b) => b.open(c, v),
        None => Err(ERR_NO_SECRET_KEY.to_string()),
    }
}

/// `TrafficTraceKey` — the instance sub-key that fingerprints the rewrite trace
/// (HMAC-SHA256 of the domain under the secret key). Empty until `InitSecrets`.
pub fn traffic_trace_key() -> Vec<u8> {
    TRAFFIC_TRACE_KEY
        .read()
        .unwrap()
        .clone()
        .unwrap_or_default()
}

/// `trafficTraceKey.Store` — install the derived sub-key.
pub fn set_traffic_trace_key(k: Vec<u8>) {
    *TRAFFIC_TRACE_KEY.write().unwrap() = Some(k);
}

/// `parseSecretKey` — accept 32 bytes encoded as base64 or hex.
pub fn parse_secret_key(s: &str) -> Result<Vec<u8>, String> {
    let s = gocompat::strings::go_trim_space(s);
    if let Ok(k) = gocompat::encoding::base64_std_decode(s) {
        if k.len() == 32 {
            return Ok(k);
        }
    }
    if let Some(k) = hex_decode(s) {
        if k.len() == 32 {
            return Ok(k);
        }
    }
    Err("must be 32 bytes, base64 or hex encoded".to_string())
}

/// `deriveSubKey` — HMAC-SHA256(key, domain).
pub fn derive_sub_key(key: &[u8], domain: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(domain.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn random_nonce() -> [u8; 12] {
    use rand::RngCore;
    let mut n = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut n);
    n
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i + 1 < b.len() {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// DB-bound half — `InitSecrets` / `loadSecretKey` / `backfillSecrets`.
//
// These need a live connection and the schema advisory lock; they live here
// (not in a bridge crate) and operate on a `&mut Client` handed in by
// `with_schema_migration_lock`, exactly as the Go originals take a `*sql.Conn`.

use crate::db::error::Error;
use crate::db::with_schema_migration_lock;
use crate::db::DB;
use std::future::Future;
use std::pin::Pin;
use tokio_postgres::Client;

impl DB {
    /// `InitSecrets`: load the key (`ARTEX_SECRET_KEY`, else `keyDir/secret.key`,
    /// else a new one — refused when encrypted data exists) and encrypt legacy
    /// plaintext, under the schema advisory lock so concurrent instances
    /// backfill once.
    pub async fn init_secrets(&self, key_dir: &str) -> crate::db::Result<()> {
        let key_dir = key_dir.to_string();
        let mut client = self.pool.get().await?;
        with_schema_migration_lock(&mut client, move |client| {
            Box::pin(async move {
                let key = load_secret_key(client, &key_dir).await?;
                let secret_box =
                    SecretBox::new(&key).map_err(|_| Error::msg("new secret box: key size"))?;
                // A key other than the one that sealed the data fails here, at
                // startup: try to decrypt one stored value per secret column.
                for c in secret_columns() {
                    let value_expr = c.value_expr();
                    let sql = format!(
                        "SELECT {} FROM {} WHERE {} LIKE 'enc:v1:%'{} LIMIT 1",
                        value_expr,
                        c.table,
                        value_expr,
                        c.row_filter()
                    );
                    let found = client.query_opt(&sql, &[]).await?;
                    if let Some(row) = found {
                        let v: String = row.get(0);
                        secret_box
                            .decrypt(&c.aad(), &v)
                            .map_err(|e| Error::msg(format!("secret key check: {}", e)))?;
                    }
                }
                let n = backfill_secrets(client, &secret_box).await?;
                eprintln!(
                    "[secrets] at-rest encryption ready; backfill encrypted {} plaintext value(s)",
                    n
                );
                set_active_secret_box(secret_box);
                let sub = derive_sub_key(&key, TRAFFIC_TRACE_DOMAIN);
                set_traffic_trace_key(sub);
                Ok(())
            }) as Pin<Box<dyn Future<Output = crate::db::Result<()>> + '_>>
        })
        .await
    }
}

/// `loadSecretKey`: resolve the key from the environment, else a key file, else
/// generate a fresh one — refusing to generate when the database already holds
/// encrypted values (which would strand them under an unknown key).
async fn load_secret_key(client: &Client, key_dir: &str) -> crate::db::Result<Vec<u8>> {
    if let Ok(v) = std::env::var(SECRET_KEY_ENV) {
        let v = gocompat::strings::go_trim_space(&v);
        if !v.is_empty() {
            return parse_secret_key(v)
                .map_err(|e| Error::msg(format!("{} {}", SECRET_KEY_ENV, e)));
        }
    }

    let path = std::path::Path::new(key_dir).join(SECRET_KEY_FILE);
    match std::fs::read_to_string(&path) {
        Ok(data) => {
            return parse_secret_key(&data)
                .map_err(|e| Error::msg(format!("secret key {} {}", path.display(), e)));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::msg(format!("read secret key: {}", e))),
    }

    // No env key and no file: count encrypted values across all secret columns.
    let parts: Vec<String> = secret_columns()
        .iter()
        .map(|c| {
            format!(
                "(SELECT count(*) FROM {} WHERE {} LIKE 'enc:v1:%'{})",
                c.table,
                c.value_expr(),
                c.row_filter()
            )
        })
        .collect();
    let row = client
        .query_one(&format!("SELECT {}", parts.join("+")), &[])
        .await?;
    let n: i64 = row.get(0);
    if n > 0 {
        return Err(Error::msg(format!(
            "the database holds {} encrypted secret(s) but no key is available: set {} or restore {} (refusing to generate a new key over encrypted data)",
            n,
            SECRET_KEY_ENV,
            path.display()
        )));
    }

    // Generate a fresh 32-byte key and write it 0600, never overwriting a file.
    let key = random_key_32();
    create_secret_key_dir(key_dir)?;
    let write_result = write_new_secret_key(&path, &key);
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&path);
        return Err(Error::msg(format!("write secret key: {}", e)));
    }
    eprintln!("[secrets] new secret key written to {}", path.display());
    Ok(key)
}

/// `os.MkdirAll(keyDir, 0o700)` — create the key directory (private).
fn create_secret_key_dir(key_dir: &str) -> crate::db::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(key_dir)
            .map_err(|e| Error::msg(format!("create secret key dir: {}", e)))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(key_dir)
            .map_err(|e| Error::msg(format!("create secret key dir: {}", e)))
    }
}

/// `os.OpenFile(path, O_WRONLY|O_CREATE|O_EXCL, 0o600)` then write the base64
/// key + newline. `create_new` is the `O_EXCL` guarantee (never overwrite).
fn write_new_secret_key(path: &std::path::Path, key: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let payload = format!("{}\n", gocompat::encoding::base64_std_encode(key));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(payload.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        f.write_all(payload.as_bytes())?;
    }
    Ok(())
}

/// `backfillSecrets`: encrypt every plaintext value once; sealed values are
/// skipped, so it is idempotent. Each `UPDATE` matches the value it read, so a
/// concurrent write is never overwritten.
async fn backfill_secrets(client: &Client, secret_box: &SecretBox) -> crate::db::Result<i64> {
    let mut total: i64 = 0;
    for c in secret_columns() {
        // Collect (id, old) for every non-NULL value, sealing plaintext ones.
        let select_sql = format!(
            "SELECT {}::text, {}::text FROM {} WHERE {} IS NOT NULL{}",
            c.id_col(),
            c.col,
            c.table,
            c.col,
            c.row_filter()
        );
        let rows = client.query(&select_sql, &[]).await?;

        // id is BIGINT for table columns and TEXT (the key) for settings/tools.
        // The SELECT casts it to text so one string path covers both (matching
        // Go's `var id any` scan); the UPDATE compares `id::text=$2`.
        let mut ids: Vec<String> = Vec::new();
        let mut olds: Vec<String> = Vec::new();
        let mut news: Vec<String> = Vec::new();
        for row in &rows {
            let id: String = row.get(0);
            let old: String = row.get(1);
            if is_sealed(&c, &old) {
                continue;
            }
            let nv = secret_box
                .seal(&c, &old)
                .map_err(|e| Error::msg(format!("backfill {} id={}: {}", c.aad(), id, e)))?;
            if is_sealed(&c, &nv) {
                // empty values stay as they are
                ids.push(id);
                olds.push(old);
                news.push(nv);
            }
        }

        // `$1::text::jsonb` for JSON columns: the innermost cast fixes the
        // parameter type as TEXT (so a String binds without the serde_json
        // ToSql feature), and Postgres then casts text → jsonb.
        let cast = if c.json { "::text::jsonb" } else { "" };
        for (i, id) in ids.iter().enumerate() {
            let update_sql = format!(
                "UPDATE {} SET {}=$1{} WHERE {}::text=$2 AND {}::text=$3",
                c.table,
                c.col,
                cast,
                c.id_col(),
                c.col
            );
            let res = client
                .execute(&update_sql, &[&news[i], &id, &olds[i]])
                .await;
            match res {
                Ok(n) => {
                    if n > 0 {
                        total += 1;
                    }
                }
                Err(e) => {
                    return Err(Error::msg(format!("backfill {} id={}: {}", c.aad(), id, e)));
                }
            }
        }
    }
    Ok(total)
}

fn random_key_32() -> Vec<u8> {
    use rand::RngCore;
    let mut k = vec![0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut k);
    k
}
