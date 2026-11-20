//! Differential tests for the `db/target_pins.go` slice — scope/key helpers +
//! pin precedence + revoke. Runs against `ARTEX_PG_DSN`; helpers are pure.

use artex::db::target_pins::{
    target_key, TargetScopeRef, TARGET_PIN_ACTIVE, TARGET_PIN_REVOKED_BY_OPERATOR,
};

#[test]
fn scope_and_key_helpers() {
    assert_eq!(target_key("example.com", 443), "example.com:443");
    assert_eq!(target_key("2001:db8::1", 0), "2001:db8::1:0");
    let task = TargetScopeRef {
        task_id: 7,
        conversation_id: 0,
    };
    assert!(task.check().is_ok());
    assert_eq!(task.pending_scope(), (Some("7".to_string()), None));
    let conv = TargetScopeRef {
        task_id: 0,
        conversation_id: 9,
    };
    assert_eq!(conv.pending_scope(), (None, Some(9)));
    assert!(TargetScopeRef {
        task_id: 0,
        conversation_id: 0
    }
    .check()
    .is_err());
    assert!(TargetScopeRef {
        task_id: 7,
        conversation_id: 9
    }
    .check()
    .is_err());
}

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

async fn seed_pin(d: &artex::db::DB, task_id: i64, host: &str, port: i32, ips: &str) -> i64 {
    d.query_one(
        "INSERT INTO target_pins(task_id, host, port, version, pinned_ips, resolver, resolved_at, status, decided_by, decided_from) \
         VALUES ($1,$2,$3,1,$4,'test',now(),'active','human','operator') RETURNING id",
        &[&task_id, &host, &port, &vec![ips.to_string()]],
    ).await.unwrap().get::<_, i64>(0)
}

#[tokio::test]
async fn pin_precedence_and_revoke() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else {
        return;
    };
    let tid = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64
        % 1_000_000_000;
    let task_id = 900_000_000 + tid; // synthetic task id (no FK on target_pins.task_id)
    let _ = d
        .execute("DELETE FROM target_pins WHERE task_id=$1", &[&task_id])
        .await;
    let scope = TargetScopeRef {
        task_id,
        conversation_id: 0,
    };

    let p0 = seed_pin(&d, task_id, "app.example.com", 0, "203.0.113.7").await;
    let p443 = seed_pin(&d, task_id, "app.example.com", 443, "203.0.113.8").await;

    // Exact port first, then port 0.
    let pins = d
        .active_target_pins(&scope, "app.example.com", 443)
        .await
        .unwrap();
    assert_eq!(pins.len(), 2);
    assert_eq!(pins[0].port, 443, "exact port first");
    assert_eq!(pins[1].port, 0);

    // Port 0 target: only the wildcard pin covers it.
    let pins0 = d
        .active_target_pins(&scope, "app.example.com", 0)
        .await
        .unwrap();
    assert_eq!(pins0.len(), 1);
    assert_eq!(pins0[0].port, 0);

    // List any state, newest first.
    let all = d.list_target_pins(&scope).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].id > all[1].id);

    // Revoke the exact-port pin: no longer active; wildcard still covers.
    let revoked = d.revoke_target_pin(&scope, p443).await.unwrap().unwrap();
    assert_eq!(revoked.status, "revoked");
    assert_eq!(revoked.revoked_reason, TARGET_PIN_REVOKED_BY_OPERATOR);
    let pins = d
        .active_target_pins(&scope, "app.example.com", 443)
        .await
        .unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].port, 0);
    // Idempotent revoke (no longer active).
    let again = d.revoke_target_pin(&scope, p443).await.unwrap().unwrap();
    assert_eq!(again.status, "revoked");
    // Revoking a pin outside scope -> None.
    let other = TargetScopeRef {
        task_id: task_id + 1,
        conversation_id: 0,
    };
    assert!(d.revoke_target_pin(&other, p0).await.unwrap().is_none());
    let _ = TARGET_PIN_ACTIVE;

    let _ = d
        .execute("DELETE FROM target_pins WHERE task_id=$1", &[&task_id])
        .await;
}
