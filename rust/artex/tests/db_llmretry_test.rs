//! Differential tests for the LLM retry policy — port of the Go spec
//! `db/llmretry_test.go::TestLLMRetryPolicyRoundTrip` plus pure clamp/or/interval
//! cases (the profile round-trip Go tests need the config group, ported later).
//!
//! The settings round-trip runs against `ARTEX_PG_DSN` behind the shared lock and
//! skips cleanly when the DSN is unset/unreachable; the pure cases need no DB.

use std::time::Duration;

use artex::db::llmretry::{LLMRetryPolicy, RetryOverride, RetryRule};
use artex::db::DB;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn base_dsn() -> Option<String> {
    std::env::var("ARTEX_PG_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn open(dsn: &str) -> Option<DB> {
    match DB::open(dsn).await {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: postgres unavailable ({e})");
            None
        }
    }
}

/// `TestLLMRetryPolicyRoundTrip`: the global policy round-trips through settings;
/// out-of-range values clamp; a missing key reads back as the zero policy.
#[tokio::test]
async fn llm_retry_policy_round_trip() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };

    const KEY: &str = "llm_retry_policy";
    // Snapshot the current value so the shared setting is restored.
    let (before, had_before) = d.get_setting(KEY).await.expect("get before");

    let want = LLMRetryPolicy {
        connect: RetryRule {
            attempts: 4,
            interval_ms: 1500,
        },
        breaker: RetryRule {
            attempts: 2,
            interval_ms: 90_000,
        },
        intent: RetryRule {
            attempts: -1,
            interval_ms: 0,
        },
        ..Default::default()
    };
    d.set_llm_retry_policy(want).await.expect("set");
    let got = d.llm_retry_policy().await;
    assert_eq!(got, want, "policy round-trip");
    assert_eq!(
        got.breaker.interval(),
        Duration::from_secs(90),
        "breaker interval"
    );

    // Out-of-range values clamp into range on the way in.
    d.set_llm_retry_policy(LLMRetryPolicy {
        stream: RetryRule {
            attempts: 999,
            interval_ms: 99_999_999,
        },
        ..Default::default()
    })
    .await
    .expect("set clamp");
    let stream = d.llm_retry_policy().await.stream;
    assert_eq!(
        (stream.attempts, stream.interval_ms),
        (20, 3_600_000),
        "stream clamped"
    );

    // Missing key = all defaults.
    d.execute("DELETE FROM settings WHERE key=$1", &[&KEY])
        .await
        .expect("delete key");
    assert_eq!(
        d.llm_retry_policy().await,
        LLMRetryPolicy::default(),
        "unset policy = zero"
    );

    // Restore the prior value (parity with the Go cleanup).
    if had_before {
        d.set_setting(KEY, &before).await.expect("restore");
    }
}

/// Pure `RetryRule` clamp/or/interval semantics.
#[test]
fn retry_rule_clamp_or_interval() {
    // Interval: <=0 → 0, else millis.
    assert_eq!(
        RetryRule {
            attempts: 0,
            interval_ms: 0
        }
        .interval(),
        Duration::ZERO
    );
    assert_eq!(
        RetryRule {
            attempts: 0,
            interval_ms: -5
        }
        .interval(),
        Duration::ZERO
    );
    assert_eq!(
        RetryRule {
            attempts: 0,
            interval_ms: 250
        }
        .interval(),
        Duration::from_millis(250)
    );

    // Clamped: attempts [-1,20], interval [0,1h].
    assert_eq!(
        RetryRule {
            attempts: -99,
            interval_ms: -5
        }
        .clamped(),
        RetryRule {
            attempts: -1,
            interval_ms: 0
        }
    );
    assert_eq!(
        RetryRule {
            attempts: 999,
            interval_ms: 99_999_999
        }
        .clamped(),
        RetryRule {
            attempts: 20,
            interval_ms: 3_600_000
        }
    );
    // -1 (explicit off) survives the clamp.
    assert_eq!(
        RetryRule {
            attempts: -1,
            interval_ms: 0
        }
        .clamped(),
        RetryRule {
            attempts: -1,
            interval_ms: 0
        }
    );

    // Or: each unset (==0) field inherits from fallback.
    let r = RetryRule {
        attempts: 3,
        interval_ms: 0,
    }
    .or(RetryRule {
        attempts: 5,
        interval_ms: 2000,
    });
    assert_eq!(
        r,
        RetryRule {
            attempts: 3,
            interval_ms: 2000
        }
    );
    // -1 is "set", so it does not inherit the fallback attempts.
    let off = RetryRule {
        attempts: -1,
        interval_ms: 0,
    }
    .or(RetryRule {
        attempts: 5,
        interval_ms: 2000,
    });
    assert_eq!(off.attempts, -1);
}

/// `RetryOverride::clamped` bounds each layer.
#[test]
fn retry_override_clamped() {
    let o = RetryOverride {
        connect: RetryRule {
            attempts: -99,
            interval_ms: -5,
        },
        ..Default::default()
    }
    .clamped();
    assert_eq!(
        o.connect,
        RetryRule {
            attempts: -1,
            interval_ms: 0
        }
    );
}
