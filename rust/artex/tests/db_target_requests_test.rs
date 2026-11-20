//! Differential tests for target request/decision DB methods — ports of Go
//! `TestTargetRequestFindOrCreateIsIdempotent`, `TestTargetRequestNewRowAfterDecision`,
//! `TestResolveTargetRequestIsAtomic`, `TestTargetPinVersioningExtends`,
//! `TestLogTargetDecision`, `TestTargetPinRefusesEmptyAddressList`.

use artex::db::target_pins::{
    target_key, TargetDecision, TargetRequest, ERR_TARGET_PIN_EMPTY, TARGET_PIN_SUPERSEDED,
};
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

fn req(conv: i64, host: &str, port: i32, _client: &str) -> TargetRequest {
    TargetRequest {
        scope: artex::db::target_pins::TargetScopeRef {
            conversation_id: conv,
            ..Default::default()
        },
        host: host.to_string(),
        port,
        agent_name: "agent".into(),
        tool_name: "Bash".into(),
        tool_input: br#"{"command":"nmap x"}"#.to_vec(),
        reason: "new_ip".into(),
        audit: Some(serde_json::json!({
            "correlation": "exact",
            "targets": [{
                "host": host, "port": port, "kind": "hostname",
                "resolved_ips": ["203.0.113.7"], "resolver": "fixture",
                "reason": "new_ip", "outcome": "requested"
            }]
        })),
        ..TargetRequest::default()
    }
}

#[tokio::test]
async fn find_or_create_and_resolve_target_request() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let conv = d
        .create_conversation("mainagent", &format!("target req {tag}"), None)
        .await
        .unwrap();
    let host = format!("rt-{tag}.example.test");
    let port = 443;

    let (id1, created) = d
        .find_or_create_target_request(&req(conv.id, &host, port, "c0"))
        .await
        .unwrap();
    assert!(created && id1 > 0);
    let (id2, created2) = d
        .find_or_create_target_request(&req(conv.id, &host, port, "c1"))
        .await
        .unwrap();
    assert!(!created2 && id2 == id1, "idempotent find-or-create");
    assert_eq!(
        d.target_request_key(id1).await.unwrap(),
        target_key(&host, port)
    );

    // Approve: pin created, row settled.
    let (pin, ok) = d
        .resolve_target_request(id1, true, "alice", "127.0.0.1")
        .await
        .unwrap();
    assert!(ok);
    let pin = pin.expect("approved pin");
    assert_eq!(pin.host, host);
    assert_eq!(pin.version, 1);
    assert_eq!(pin.pinned_ips, vec!["203.0.113.7".to_string()]);
    assert_eq!(pin.decided_by, "alice");

    // A new request for the same key after the decision gets a fresh row, and
    // approval extends the pin to version 2 (superseding version 1).
    let (id3, created3) = d
        .find_or_create_target_request(&req(conv.id, &host, port, "c2"))
        .await
        .unwrap();
    assert!(created3 && id3 != id1);
    let (pin2, ok) = d
        .resolve_target_request(id3, true, "bob", "127.0.0.2")
        .await
        .unwrap();
    assert!(ok);
    let pin2 = pin2.expect("pin v2");
    assert_eq!(pin2.version, 2);
    assert_eq!(pin2.pinned_ips, vec!["203.0.113.7".to_string()]);
    let old = d
        .list_target_pins(&artex::db::target_pins::TargetScopeRef {
            conversation_id: conv.id,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(old
        .iter()
        .any(|p| p.id == pin.id && p.status == TARGET_PIN_SUPERSEDED));

    // Denial pins nothing, but settles the row.
    let (id4, _) = d
        .find_or_create_target_request(&req(conv.id, &host, port + 1, "c3"))
        .await
        .unwrap();
    let (denied_pin, ok) = d
        .resolve_target_request(id4, false, "carol", "")
        .await
        .unwrap();
    assert!(ok && denied_pin.is_none());

    let _ = d.delete_conversation(conv.id).await;
}

#[tokio::test]
async fn log_decision_and_empty_addresses() {
    let Some(dsn) = base_dsn() else {
        eprintln!("skipping: ARTEX_PG_DSN unset");
        return;
    };
    let _g = DB_LOCK.lock().await;
    let Some(d) = open(&dsn).await else { return };
    let tag = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let conv = d
        .create_conversation("mainagent", &format!("target log {tag}"), None)
        .await
        .unwrap();

    let id = d
        .log_target_decision(&TargetDecision {
            scope: artex::db::target_pins::TargetScopeRef {
                conversation_id: conv.id,
                ..Default::default()
            },
            agent_name: "agent".into(),
            tool_name: "Bash".into(),
            tool_input: br#"{"command":"x"}"#.to_vec(),
            status: "denied".into(),
            reason: "rule blocked".into(),
            audit: None,
        })
        .await
        .unwrap();
    assert!(id > 0);
    let key = d.target_request_key(id).await.unwrap();
    assert_eq!(key, ""); // classic decided row, not a target approval

    // An approval whose audit shows no resolved address must be refused.
    let mut empty = req(conv.id, &format!("empty-{tag}.example.test"), 80, "c4");
    empty.audit = Some(serde_json::json!({"correlation": "exact", "targets": [{
        "host": empty.host.clone(), "port": 80, "resolved_ips": [], "outcome": "requested"
    }]}));
    let (rid, _) = d.find_or_create_target_request(&empty).await.unwrap();
    let err = d
        .resolve_target_request(rid, true, "alice", "")
        .await
        .unwrap_err();
    assert!(err.is(ERR_TARGET_PIN_EMPTY), "got {err}");
    // The row stays pending after the failed pin write.
    let (again, created) = d.find_or_create_target_request(&empty).await.unwrap();
    assert!(!created && again == rid);

    let _ = d.delete_conversation(conv.id).await;
}
