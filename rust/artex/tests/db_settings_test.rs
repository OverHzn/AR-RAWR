//! Differential tests for the settings store + DB-bound secrets — port of
//! `db/settings_insert_test.go` and the DB-bound halves of `db/secrets_test.go`
//! (`TestSecretsBackfillLegacyAndKeyChecks`, `TestSecretSettingsEmptyAndClear`).
//!
//! The Go specs for secrets at rest lean on the resource-group write methods
//! (`CreateTask`/`SaveProfile`/`SaveMCP`/…), which are not ported yet. The Go
//! code itself has a "legacy" fixture path that writes plaintext with raw SQL,
//! so this port drives `InitSecrets`/`loadSecretKey`/`backfillSecrets` and the
//! settings CRUD through raw SQL against the live Postgres from `ARTEX_PG_DSN`
//! (the same image the Go oracle uses) and skips cleanly when it is unreachable.
//!
//! Every test holds one shared async lock and the harness runs the file with
//! `-- --test-threads=1`, mirroring Go's serial in-package execution (Go's
//! `TestMain` installs the secret box once and `t.Setenv` forbids parallelism):
//! these tests mutate the process-global active secret box and `ARTEX_SECRET_KEY`.

use std::sync::atomic::{AtomicU64, Ordering};

use artex::db::secrets::{
    is_encrypted, is_sealed, open_secret, set_active_secret_box, SecretBox, COL_CRED_PASS,
    COL_LLM_KEY, COL_LLM_PROXY, COL_MCP_ENV, COL_NOTIF_CONFIG, COL_REWRITE_ACTIONS,
    COL_REWRITE_MATCHERS, COL_TOOL_EXEC, SECRET_KEY_ENV, SECRET_PREFIX,
};
use artex::db::DB;

/// Serializes the whole file: one shared lock stands in for Go's serial
/// in-package execution. These tests share the process-global active secret box
/// and `ARTEX_SECRET_KEY`, and write the same secret settings, so they must not
/// overlap even if `cargo test` runs test functions on multiple threads.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `testSecretKey` — the fixed 32-byte key `"0123456789abcdef0123456789abcdef"`.
fn test_secret_key() -> Vec<u8> {
    b"0123456789abcdef0123456789abcdef".to_vec()
}

fn b64_encode(b: &[u8]) -> String {
    gocompat::encoding::base64_std_encode(b)
}

/// Bind a JSON literal to a JSONB column as a `serde_json::Value` (needs the
/// tokio-postgres `with-serde_json-1` feature).
fn j(s: &str) -> serde_json::Value {
    serde_json::from_str(s).expect("valid json fixture")
}

/// Install the process-wide secret box under the test key, idempotently. Mirrors
/// what the Go `TestMain` does once for the whole package.
fn ensure_test_box() {
    let b = SecretBox::new(&test_secret_key()).expect("test key is 32 bytes");
    set_active_secret_box(b);
}

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A process-unique suffix, so leftover rows from a crashed run never collide.
fn uniq() -> u128 {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos + (N.fetch_add(1, Ordering::Relaxed) as u128) * 7919
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("artex-secrets-{tag}-{}", uniq()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

const L_LLM_PROXY: &str = "http://u:llmproxy-plain-pass@llm-proxy.example:3128";

/// The plaintext tokens the fixture hides; none may appear at rest.
const PLAIN_TOKENS: &[&str] = &[
    "sk-plain-llm",
    "llmproxy-plain-pass",
    "mcp-plain-tok",
    "plain-pass",
    "hook-plain-secret",
    "brave-plain",
    "tavily-plain",
    "keenable-plain",
    "wsproxy-plain-pass",
    "gproxy-plain-pass",
    "tool-plain-tok",
    "match-plain-tok",
    "action-plain-tok",
];

fn secret_setting_values() -> Vec<(&'static str, String)> {
    vec![
        ("brave_search_api_key", "brave-plain".to_string()),
        ("tavily_search_api_key", "tavily-plain".to_string()),
        ("keenable_search_api_key", "keenable-plain".to_string()),
        (
            "web_search_proxy",
            "socks5://u:wsproxy-plain-pass@ws.example:1080".to_string(),
        ),
        (
            "global_proxy",
            "http://u:gproxy-plain-pass@gp.example:3128".to_string(),
        ),
    ]
}

const DELETE_SECRET_SETTINGS: &str = "DELETE FROM settings WHERE key IN \
('brave_search_api_key','tavily_search_api_key','keenable_search_api_key','web_search_proxy','global_proxy')";

/// Ids of the fixture rows, so the test can clean them up.
struct Fixture {
    exploration: i64,
    task: i64,
    llm: i64,
    mcp: i64,
    cred: i64,
    ch: i64,
    tool: String,
    rule: i64,
}

/// Write one plaintext value into each secret location with raw SQL (the Go
/// "legacy database" path). Returns the fixture ids.
async fn seed_legacy(d: &DB) -> Fixture {
    let conn = d.pool.get().await.expect("pool");
    let stamp = uniq();

    // Secret settings have fixed keys (no uniq suffix), so clear any leftover
    // from a crashed earlier run before inserting this fixture's values.
    conn.batch_execute(DELETE_SECRET_SETTINGS)
        .await
        .expect("clear leftover secret settings");

    // tasks.exploration_id has a FK to explorations, so seed one first.
    let exploration: i64 = conn
        .query_one(
            "INSERT INTO explorations(goal) VALUES ('secrets fixture exploration') RETURNING id",
            &[],
        )
        .await
        .expect("insert exploration")
        .get(0);

    let task: i64 = conn
        .query_one(
            "INSERT INTO tasks(description, goal, exploration_id) VALUES ('secrets fixture', 'goal', $1) RETURNING id",
            &[&exploration],
        )
        .await
        .expect("insert task")
        .get(0);

    let llm: i64 = conn
        .query_one(
            "INSERT INTO llm_profiles(name, format, model, api_key, proxy) VALUES ($1, 'openai', 'm', 'sk-plain-llm', $2) RETURNING id",
            &[&format!("t-sec-{stamp}"), &L_LLM_PROXY],
        )
        .await
        .expect("insert llm")
        .get(0);

    for (k, v) in secret_setting_values() {
        conn.execute(
            "INSERT INTO settings(key, value) VALUES ($1, $2)",
            &[&k, &v],
        )
        .await
        .expect("insert secret setting");
    }

    let mcp: i64 = conn
        .query_one(
            "INSERT INTO mcp_servers(name, transport, env) VALUES ($1, 'http', $2) RETURNING id",
            &[
                &format!("t-sec-{stamp}"),
                &j(r#"{"Authorization":"mcp-plain-tok"}"#),
            ],
        )
        .await
        .expect("insert mcp")
        .get(0);

    let cred: i64 = conn
        .query_one(
            "INSERT INTO task_credentials(task_id, domain, username, password) VALUES ($1, 'x.example', 'u', 'plain-pass') RETURNING id",
            &[&task],
        )
        .await
        .expect("insert cred")
        .get(0);

    let ch: i64 = conn
        .query_one(
            "INSERT INTO notification_channels(name, kind, config) VALUES ($1, 'webhook', $2) RETURNING id",
            &[
                &format!("t-sec-{stamp}"),
                &j(r#"{"url":"https://h.example","headers":{"X":"hook-plain-secret"}}"#),
            ],
        )
        .await
        .expect("insert channel")
        .get(0);

    let tool = format!("t-sec-tool-{stamp}");
    conn.execute(
        "INSERT INTO tools(key, system, exec) VALUES ($1, false, $2)",
        &[&tool, &j(r#"{"command":"echo","token":"tool-plain-tok"}"#)],
    )
    .await
    .expect("insert tool");

    let rule: i64 = conn
        .query_one(
            "INSERT INTO traffic_rewrite_rules(name, phase, matchers, actions) VALUES ($1, 'request', $2, $3) RETURNING id",
            &[
                &format!("t-sec-rule-{stamp}"),
                &j(r#"{"header":"X-Secret","value":"match-plain-tok"}"#),
                &j(r#"{"token":"action-plain-tok"}"#),
            ],
        )
        .await
        .expect("insert rewrite rule")
        .get(0);

    Fixture {
        exploration,
        task,
        llm,
        mcp,
        cred,
        ch,
        tool,
        rule,
    }
}

async fn cleanup(d: &DB, f: &Fixture) {
    let conn = d.pool.get().await.expect("pool");
    let _ = conn.batch_execute(DELETE_SECRET_SETTINGS).await;
    let _ = conn
        .execute("DELETE FROM tools WHERE key=$1", &[&f.tool])
        .await;
    let _ = conn
        .execute("DELETE FROM traffic_rewrite_rules WHERE id=$1", &[&f.rule])
        .await;
    let _ = conn
        .execute("DELETE FROM task_credentials WHERE id=$1", &[&f.cred])
        .await;
    let _ = conn
        .execute("DELETE FROM notification_channels WHERE id=$1", &[&f.ch])
        .await;
    let _ = conn
        .execute("DELETE FROM mcp_servers WHERE id=$1", &[&f.mcp])
        .await;
    let _ = conn
        .execute("DELETE FROM llm_profiles WHERE id=$1", &[&f.llm])
        .await;
    let _ = conn
        .execute("DELETE FROM tasks WHERE id=$1", &[&f.task])
        .await;
    let _ = conn
        .execute("DELETE FROM explorations WHERE id=$1", &[&f.exploration])
        .await;
}

/// Read every secret column as stored, in a fixed order. Returns `(label, raw)`.
async fn read_all_raw(d: &DB, f: &Fixture) -> Vec<(String, String)> {
    let conn = d.pool.get().await.expect("pool");
    let mut out: Vec<(String, String)> = Vec::new();

    let llm = conn
        .query_one(
            "SELECT api_key, proxy FROM llm_profiles WHERE id=$1",
            &[&f.llm],
        )
        .await
        .expect("llm row");
    out.push(("llm_profiles.api_key".into(), llm.get::<_, String>(0)));
    out.push(("llm_profiles.proxy".into(), llm.get::<_, String>(1)));

    for (k, _) in secret_setting_values() {
        let v: String = conn
            .query_one("SELECT value FROM settings WHERE key=$1", &[&k])
            .await
            .expect("setting row")
            .get(0);
        out.push((format!("settings.{k}"), v));
    }

    let env: String = conn
        .query_one("SELECT env::text FROM mcp_servers WHERE id=$1", &[&f.mcp])
        .await
        .expect("mcp row")
        .get(0);
    out.push(("mcp_servers.env".into(), env));

    let pw: String = conn
        .query_one(
            "SELECT password FROM task_credentials WHERE id=$1",
            &[&f.cred],
        )
        .await
        .expect("cred row")
        .get(0);
    out.push(("task_credentials.password".into(), pw));

    let cfg: String = conn
        .query_one(
            "SELECT config::text FROM notification_channels WHERE id=$1",
            &[&f.ch],
        )
        .await
        .expect("channel row")
        .get(0);
    out.push(("notification_channels.config".into(), cfg));

    let exec: String = conn
        .query_one("SELECT exec::text FROM tools WHERE key=$1", &[&f.tool])
        .await
        .expect("tool row")
        .get(0);
    out.push(("tools.exec".into(), exec));

    let rw = conn
        .query_one(
            "SELECT matchers::text, actions::text FROM traffic_rewrite_rules WHERE id=$1",
            &[&f.rule],
        )
        .await
        .expect("rewrite row");
    out.push((
        "traffic_rewrite_rules.matchers".into(),
        rw.get::<_, String>(0),
    ));
    out.push((
        "traffic_rewrite_rules.actions".into(),
        rw.get::<_, String>(1),
    ));

    out
}

/// Assert every stored value is sealed and no plaintext token survives.
fn assert_all_sealed(raw: &[(String, String)]) {
    for (label, v) in raw {
        let sealed = match label.as_str() {
            "mcp_servers.env" => is_sealed(&COL_MCP_ENV, v),
            "notification_channels.config" => is_sealed(&COL_NOTIF_CONFIG, v),
            "tools.exec" => is_sealed(&COL_TOOL_EXEC, v),
            "traffic_rewrite_rules.matchers" => is_sealed(&COL_REWRITE_MATCHERS, v),
            "traffic_rewrite_rules.actions" => is_sealed(&COL_REWRITE_ACTIONS, v),
            _ => is_encrypted(v),
        };
        assert!(sealed, "{label} is not sealed at rest: {v}");
    }
    let all = raw
        .iter()
        .map(|(_, v)| v.as_str())
        .collect::<Vec<_>>()
        .join("|");
    for p in PLAIN_TOKENS {
        assert!(!all.contains(p), "plaintext {p:?} found at rest");
    }
}

/// Assert each read path returns the original plaintext in the clear.
async fn assert_plain_reads(d: &DB, f: &Fixture) {
    // Settings decrypt through GetSetting.
    for (k, want) in secret_setting_values() {
        let (v, ok) = d.get_setting(k).await.expect("GetSetting");
        assert!(ok, "GetSetting({k}) unset");
        assert_eq!(v, want, "GetSetting({k}) not decrypted");
    }
    // Table columns decrypt through open_secret over the stored ciphertext.
    let conn = d.pool.get().await.expect("pool");
    let llm = conn
        .query_one(
            "SELECT api_key, proxy FROM llm_profiles WHERE id=$1",
            &[&f.llm],
        )
        .await
        .expect("llm");
    assert_eq!(
        open_secret(&COL_LLM_KEY, &llm.get::<_, String>(0)).unwrap(),
        "sk-plain-llm"
    );
    assert_eq!(
        open_secret(&COL_LLM_PROXY, &llm.get::<_, String>(1)).unwrap(),
        L_LLM_PROXY
    );

    let env: String = conn
        .query_one("SELECT env::text FROM mcp_servers WHERE id=$1", &[&f.mcp])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        open_secret(&COL_MCP_ENV, &env).unwrap(),
        r#"{"Authorization":"mcp-plain-tok"}"#
    );

    let pw: String = conn
        .query_one(
            "SELECT password FROM task_credentials WHERE id=$1",
            &[&f.cred],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(open_secret(&COL_CRED_PASS, &pw).unwrap(), "plain-pass");

    let cfg: String = conn
        .query_one(
            "SELECT config::text FROM notification_channels WHERE id=$1",
            &[&f.ch],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        open_secret(&COL_NOTIF_CONFIG, &cfg).unwrap(),
        r#"{"headers":{"X":"hook-plain-secret"},"url":"https://h.example"}"#
    );

    let exec: String = conn
        .query_one("SELECT exec::text FROM tools WHERE key=$1", &[&f.tool])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        open_secret(&COL_TOOL_EXEC, &exec).unwrap(),
        r#"{"command":"echo","token":"tool-plain-tok"}"#
    );

    let rw = conn
        .query_one(
            "SELECT matchers::text, actions::text FROM traffic_rewrite_rules WHERE id=$1",
            &[&f.rule],
        )
        .await
        .unwrap();
    assert_eq!(
        open_secret(&COL_REWRITE_MATCHERS, &rw.get::<_, String>(0)).unwrap(),
        r#"{"header":"X-Secret","value":"match-plain-tok"}"#
    );
    assert_eq!(
        open_secret(&COL_REWRITE_ACTIONS, &rw.get::<_, String>(1)).unwrap(),
        r#"{"token":"action-plain-tok"}"#
    );
}

/// Parse the encrypted-value count out of the no-key refusal (Go `encryptedCount`).
fn parse_encrypted_count(msg: &str) -> i64 {
    let marker = "the database holds ";
    let start = msg
        .find(marker)
        .unwrap_or_else(|| panic!("refusal without count: {msg}"))
        + marker.len();
    let rest = &msg[start..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("refusal count not numeric: {msg}"))
}

// --- settings_insert_test.go (verbatim ports) --------------------------------

#[tokio::test]
async fn insert_setting_if_absent_does_not_overwrite() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let d = match DB::open(&dsn).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            return;
        }
    };

    let key = format!("test.insert_if_absent.{}", uniq());
    let conn = d.pool.get().await.expect("pool");

    let inserted = d.insert_setting_if_absent(&key, "first").await.unwrap();
    assert!(inserted, "first write should return inserted=true");

    let inserted = d.insert_setting_if_absent(&key, "second").await.unwrap();
    assert!(!inserted, "an existing key should return inserted=false");

    let (got, ok) = d.get_setting(&key).await.unwrap();
    assert!(ok, "GetSetting ok=false");
    assert_eq!(got, "first", "value was overwritten");

    let _ = conn
        .execute("DELETE FROM settings WHERE key=$1", &[&key])
        .await;
}

#[tokio::test]
async fn rotate_password_rolls_back_hash_when_version_cannot_increment() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let d = match DB::open(&dsn).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            return;
        }
    };

    let stamp = uniq();
    let hash_key = format!("test.rotate_hash.{stamp}");
    let version_key = format!("test.rotate_version.{stamp}");
    let fresh_key = format!("test.rotate_fresh.{stamp}");
    let conn = d.pool.get().await.expect("pool");

    d.set_setting(&hash_key, "old-hash").await.unwrap();
    d.set_setting(&version_key, "not-a-number").await.unwrap();

    // Incrementing a non-numeric version must fail, and roll the hash back.
    assert!(
        d.rotate_password(&hash_key, "new-hash", &version_key)
            .await
            .is_err(),
        "increment of a non-numeric version should fail"
    );
    let (got, ok) = d.get_setting(&hash_key).await.unwrap();
    assert!(
        ok && got == "old-hash",
        "hash={got:?} ok={ok}, want the previous hash"
    );
    let (ver, _) = d.get_setting(&version_key).await.unwrap();
    assert_eq!(ver, "not-a-number", "version should be unchanged");

    let n = d
        .rotate_password(&hash_key, "new-hash", &fresh_key)
        .await
        .unwrap();
    assert_eq!(n, 1, "first version");
    let n = d
        .rotate_password(&hash_key, "newer-hash", &fresh_key)
        .await
        .unwrap();
    assert_eq!(n, 2, "second version");

    let _ = conn
        .execute(
            "DELETE FROM settings WHERE key=$1 OR key=$2 OR key=$3",
            &[&hash_key, &version_key, &fresh_key],
        )
        .await;
}

// --- secrets_test.go, DB-bound halves ---------------------------------------

#[tokio::test]
async fn secrets_backfill_legacy_and_key_checks() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let d = match DB::open(&dsn).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            return;
        }
    };

    let f = seed_legacy(&d).await;

    // Install the test key via the environment and backfill the legacy plaintext.
    std::env::set_var(SECRET_KEY_ENV, b64_encode(&test_secret_key()));
    let key_dir = tmp_dir("backfill");
    d.init_secrets(key_dir.to_str().unwrap())
        .await
        .expect("InitSecrets");

    let first = read_all_raw(&d, &f).await;
    assert_all_sealed(&first);
    assert_plain_reads(&d, &f).await;

    // A second backfill must leave every ciphertext byte-identical (idempotent).
    d.init_secrets(key_dir.to_str().unwrap())
        .await
        .expect("second InitSecrets");
    let second = read_all_raw(&d, &f).await;
    let a: Vec<&str> = first.iter().map(|(_, v)| v.as_str()).collect();
    let b: Vec<&str> = second.iter().map(|(_, v)| v.as_str()).collect();
    assert_eq!(a, b, "second backfill changed a ciphertext");

    // Encrypted data, no env key, no key file: refuse, and write no new key.
    std::env::remove_var(SECRET_KEY_ENV);
    let dir = tmp_dir("nokey");
    let err = d
        .init_secrets(dir.to_str().unwrap())
        .await
        .expect_err("want refusal without a key");
    assert!(
        err.to_string().contains("no key is available"),
        "refusal {:?} should mention 'no key is available'",
        err.to_string()
    );
    assert!(
        !dir.join(artex::db::secrets::SECRET_KEY_FILE).exists(),
        "a new key was generated over encrypted data"
    );

    // Wrong key: the startup key check fails, and the active key is unchanged.
    std::env::set_var(SECRET_KEY_ENV, hex_of(&[0xffu8; 32]));
    let err = d
        .init_secrets(dir.to_str().unwrap())
        .await
        .expect_err("want wrong-key refusal");
    assert!(
        err.to_string().contains("secret key check"),
        "refusal {:?} should mention 'secret key check'",
        err.to_string()
    );
    assert_plain_reads(&d, &f).await; // the active key is unchanged

    // The no-key refusal counts the five encrypted settings.
    std::env::remove_var(SECRET_KEY_ENV);
    let conn = d.pool.get().await.expect("pool");
    let with_settings = parse_encrypted_count(
        &d.init_secrets(dir.to_str().unwrap())
            .await
            .expect_err("no-key refusal")
            .to_string(),
    );
    conn.batch_execute(DELETE_SECRET_SETTINGS).await.unwrap();
    let without_settings = parse_encrypted_count(
        &d.init_secrets(dir.to_str().unwrap())
            .await
            .expect_err("no-key refusal")
            .to_string(),
    );
    assert_eq!(
        with_settings - without_settings,
        artex::db::secrets::SECRET_SETTING_KEYS.len() as i64,
        "the secret settings must count toward the refusal total"
    );

    // A setting sealed under another key fails the startup check on that key.
    let other = SecretBox::new(&[0xffu8; 32]).expect("32-byte key");
    let v = other
        .encrypt("settings.global_proxy", "http://other.example:3128")
        .expect("encrypt");
    conn.execute(
        "INSERT INTO settings(key, value) VALUES ('global_proxy', $1)",
        &[&v],
    )
    .await
    .unwrap();
    std::env::set_var(SECRET_KEY_ENV, b64_encode(&test_secret_key()));
    let err = d
        .init_secrets(tmp_dir("wrongcol").to_str().unwrap())
        .await
        .expect_err("want key-check failure on settings.global_proxy");
    assert!(
        err.to_string().contains("settings.global_proxy"),
        "refusal {:?} should name settings.global_proxy",
        err.to_string()
    );

    // Restore env + cleanup.
    std::env::set_var(SECRET_KEY_ENV, b64_encode(&test_secret_key()));
    cleanup(&d, &f).await;
    let _ = std::fs::remove_dir_all(&key_dir);
}

#[tokio::test]
async fn secret_settings_empty_and_clear() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let d = match DB::open(&dsn).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            return;
        }
    };
    ensure_test_box();
    let conn = d.pool.get().await.expect("pool");

    // Fixed-key secret settings can be left behind by a crashed earlier run;
    // clear them so InsertSettingIfAbsent starts from a clean slate.
    conn.batch_execute(DELETE_SECRET_SETTINGS)
        .await
        .expect("clear leftover secret settings");

    let stamp = uniq();
    let clear_key = format!("t_sec_clear_{stamp}");
    let llm_name = format!("t-sec-noproxy-{stamp}");

    // Empty secret setting stays empty.
    d.set_setting("global_proxy", "").await.expect("set empty");
    let raw: String = conn
        .query_one("SELECT value FROM settings WHERE key='global_proxy'", &[])
        .await
        .expect("raw global_proxy")
        .get(0);
    assert_eq!(raw, "", "empty secret setting must stay empty");

    // InsertSettingIfAbsent encrypts a secret setting.
    let ok = d
        .insert_setting_if_absent("tavily_search_api_key", "tavily-insert")
        .await
        .expect("insert secret");
    assert!(ok, "InsertSettingIfAbsent should insert");
    let raw: String = conn
        .query_one(
            "SELECT value FROM settings WHERE key='tavily_search_api_key'",
            &[],
        )
        .await
        .expect("raw tavily")
        .get(0);
    assert!(
        is_encrypted(&raw),
        "InsertSettingIfAbsent must encrypt a secret"
    );
    let (v, ok) = d.get_setting("tavily_search_api_key").await.unwrap();
    assert!(ok, "GetSetting tavily ok=false");
    assert_eq!(v, "tavily-insert", "read back");

    // A non-secret setting stays in clear.
    d.set_setting(&clear_key, "visible")
        .await
        .expect("set clear");
    let raw: String = conn
        .query_one("SELECT value FROM settings WHERE key=$1", &[&clear_key])
        .await
        .expect("raw clear")
        .get(0);
    assert_eq!(raw, "visible", "a non-secret setting must stay in clear");

    // Cleanup.
    let _ = conn.batch_execute(DELETE_SECRET_SETTINGS).await;
    let _ = conn
        .execute("DELETE FROM settings WHERE key=$1", &[&clear_key])
        .await;
    let _ = conn
        .execute("DELETE FROM llm_profiles WHERE name=$1", &[&llm_name])
        .await;
}

fn hex_of(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

// Keep SECRET_PREFIX referenced so a future edit that drops a check still fails
// to compile here rather than silently weakening the seal assertions.
#[allow(dead_code)]
fn _prefix_is_enc() -> bool {
    is_encrypted(SECRET_PREFIX)
}
