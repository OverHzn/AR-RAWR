//! Differential tests for the LLM-profile slice of `db/config.go` — ports of
//! `TestPoolProfilesOrder`, `TestProfileMaxTokensRoundTrip/Defaults`,
//! `TestProfileRetryRoundTrip/ClampedOnSave`. Runs against `ARTEX_PG_DSN`.

use artex::db::config_profiles::LLMProfile;
use artex::db::llmretry::{RetryOverride, RetryRule};
use artex::db::secrets::{set_active_secret_box, SecretBox};

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<artex::db::DB> {
    match artex::db::DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

fn install_box() {
    let key = b"0123456789abcdef0123456789abcdef".to_vec();
    set_active_secret_box(SecretBox::new(&key).unwrap());
}

fn make_profile(name: &str, key: &str) -> LLMProfile {
    LLMProfile {
        name: name.to_string(),
        format: "openai".to_string(),
        model: "m".to_string(),
        api_key: key.to_string(),
        ..LLMProfile::default()
    }
}

#[tokio::test]
async fn pool_profiles_order_and_limits() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_box();
    let _ = d
        .execute("DELETE FROM llm_profiles WHERE name LIKE 't-pool-%'", &[])
        .await;
    let sfx = format!(
        "-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let mut p = make_profile(&format!("t-pool-lo{sfx}"), "k1");
    p.priority = 1;
    let lo = d.save_profile(&mut p).await.unwrap();
    let mut p = make_profile(&format!("t-pool-hi{sfx}"), "k2");
    p.priority = 9;
    let hi = d.save_profile(&mut p).await.unwrap();
    let mut p = make_profile(&format!("t-pool-excluded{sfx}"), "k3");
    p.priority = 99;
    p.pool_exclude = true;
    let _excluded = d.save_profile(&mut p).await.unwrap();
    let mut p = make_profile(&format!("t-pool-nokey{sfx}"), "");
    p.priority = 50;
    let _nokey = d.save_profile(&mut p).await.unwrap();

    let chain = d.pool_profiles().await.unwrap();
    let mut got = Vec::new();
    for prof in &chain {
        match prof.name.as_str() {
            n if n == format!("t-pool-lo{sfx}") => got.push(prof.id),
            n if n == format!("t-pool-hi{sfx}") => got.push(prof.id),
            n if n == format!("t-pool-excluded{sfx}") => panic!("excluded profile in pool"),
            n if n == format!("t-pool-nokey{sfx}") => panic!("keyless profile in pool"),
            _ => {}
        }
    }
    assert_eq!(got, vec![hi, lo], "active first, then priority DESC");

    let _ = d
        .execute("DELETE FROM llm_profiles WHERE name LIKE 't-pool-%'", &[])
        .await;
}

#[tokio::test]
async fn max_tokens_round_trip_and_defaults() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_box();
    let _ = d
        .execute(
            "DELETE FROM llm_profiles WHERE name='t-maxtok' OR name LIKE 't-maxtok-%'",
            &[],
        )
        .await;

    let mut p = make_profile("t-maxtok", "k");
    p.max_tokens = 8192;
    p.max_tokens_field = "max_completion_tokens".to_string();
    let id = d.save_profile(&mut p).await.unwrap();
    // Keyed load.
    let got = d.profile_by_id(id).await.unwrap().unwrap();
    assert_eq!(got.max_tokens, 8192);
    assert_eq!(got.max_tokens_field, "max_completion_tokens");
    // List load (different column list).
    let listed = d.list_profiles().await.unwrap();
    let lp = listed.iter().find(|x| x.id == id).unwrap();
    assert_eq!(lp.max_tokens, 8192);
    assert_eq!(lp.max_tokens_field, "max_completion_tokens");

    // Defaults.
    let mut def = make_profile("t-maxtok-default", "k");
    let did = d.save_profile(&mut def).await.unwrap();
    let got = d.profile_by_id(did).await.unwrap().unwrap();
    assert_eq!(got.max_tokens, 0);
    assert_eq!(got.max_tokens_field, "");

    let _ = d
        .execute(
            "DELETE FROM llm_profiles WHERE id=ANY($1::bigint[])",
            &[&vec![id, did]],
        )
        .await;
}

#[tokio::test]
async fn retry_round_trip_and_clamp_on_save() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    install_box();
    let sfx = format!(
        "-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let want = RetryOverride {
        connect: RetryRule {
            attempts: 5,
            interval_ms: 2000,
        },
        empty: RetryRule {
            attempts: -1,
            interval_ms: 0,
        },
        stream: RetryRule {
            attempts: 0,
            interval_ms: 250,
        },
    };
    let mut p = make_profile(&format!("t-retry{sfx}"), "k");
    p.retry = want;
    let id = d.save_profile(&mut p).await.unwrap();
    let got = d.profile_by_id(id).await.unwrap().unwrap();
    assert_eq!(got.retry, want, "retry round trip");

    // Clamped on save.
    let mut big = make_profile(&format!("t-retry-big{sfx}"), "k");
    big.retry = RetryOverride {
        connect: RetryRule {
            attempts: 999,
            interval_ms: 99_999_999,
        },
        empty: RetryRule {
            attempts: -50,
            interval_ms: 0,
        },
        stream: RetryRule {
            attempts: 1,
            interval_ms: 0,
        },
    };
    let bid = d.save_profile(&mut big).await.unwrap();
    let got = d.profile_by_id(bid).await.unwrap().unwrap();
    assert_eq!(got.retry, big.retry.clamped(), "clamped on save");

    // Empty API key on update keeps the existing key/row usable.
    let mut upd = make_profile(&format!("t-retry-upd{sfx}"), "secret-key-1234");
    let uid = d.save_profile(&mut upd).await.unwrap();
    // Keyless update uses the row as loaded (with its id), like the Go test.
    let mut loaded = d.profile_by_id(uid).await.unwrap().unwrap();
    loaded.api_key.clear();
    loaded.name = format!("t-retry-upd2{sfx}");
    d.save_profile(&mut loaded).await.unwrap();
    let got = d.profile_by_id(uid).await.unwrap().unwrap();
    assert_eq!(
        got.api_key, "secret-key-1234",
        "empty key on update keeps existing"
    );
    assert_eq!(got.name, format!("t-retry-upd2{sfx}"));

    let _ = d
        .execute(
            "DELETE FROM llm_profiles WHERE id=ANY($1::bigint[])",
            &[&vec![id, bid, uid]],
        )
        .await;
}
