//! Differential tests for the target exemption slice of `db/target_pins.go` —
//! list replace + change log + raw value. Runs against `ARTEX_PG_DSN`.

use artex::db::target_exemptions::TargetExemption;

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

#[tokio::test]
async fn exemption_replace_and_log() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let _ = d
        .execute("DELETE FROM target_gate_exemption_log", &[])
        .await;
    let _ = d
        .execute(
            "DELETE FROM settings WHERE key='target_gate_exempt_hosts'",
            &[],
        )
        .await;

    // Never stored -> empty list + raw.
    let (list, raw) = d.target_exemptions().await.unwrap();
    assert!(list.is_empty() && raw.is_empty());

    // First replace.
    let first = vec![
        TargetExemption {
            host: "api.example".to_string(),
            port: 443,
            note: "gateway".to_string(),
        },
        TargetExemption {
            host: "10.0.0.5".to_string(),
            port: 8080,
            note: String::new(),
        },
    ];
    d.replace_target_exemptions(&first, "tester", "digest-1")
        .await
        .unwrap();
    let (got, raw) = d.target_exemptions().await.unwrap();
    assert_eq!(got, first);
    assert!(!raw.is_empty());

    // Second replace appends a log row with the previous value as `before`.
    let second = vec![TargetExemption {
        host: "new.example".to_string(),
        port: 80,
        note: String::new(),
    }];
    d.replace_target_exemptions(&second, "admin", "digest-2")
        .await
        .unwrap();
    let (got, _) = d.target_exemptions().await.unwrap();
    assert_eq!(got, second);
    let log = d.list_target_exemption_log(10).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].digest, "digest-2", "newest first");
    assert_eq!(log[0].changed_by, "admin");
    assert_eq!(log[1].digest, "digest-1");
    assert!(log[0].before.is_array(), "before is the prior list");

    // Empty replace -> [] stored.
    d.replace_target_exemptions(&[], "tester", "digest-3")
        .await
        .unwrap();
    let (got, _) = d.target_exemptions().await.unwrap();
    assert!(got.is_empty());

    let _ = d
        .execute("DELETE FROM target_gate_exemption_log", &[])
        .await;
    let _ = d
        .execute(
            "DELETE FROM settings WHERE key='target_gate_exempt_hosts'",
            &[],
        )
        .await;
}
