//! Differential tests for `agent/tools.go` stagnation detection — port of
//! `TestDetectStagnation`.

use std::time::UNIX_EPOCH;

use artex::agent::tools::{detect_stagnation, normalize_direction};
use artex::db::exploration::Node;

fn node(id: i64, kind: &str, summary: &str) -> Node {
    Node {
        id,
        kind: kind.to_string(),
        payload: serde_json::json!({ "summary": summary }),
        state: "done".to_string(),
        created_at: UNIX_EPOCH,
        ..Node {
            finding_id: 0,
            finding_node_id: 0,
            traffic_count: 0,
            id: 0,
            kind: String::new(),
            payload: serde_json::Value::Null,
            priority: 0,
            state: String::new(),
            origin: String::new(),
            owner: String::new(),
            blocked_reason: String::new(),
            delete_reason: String::new(),
            anchors: Vec::new(),
            created_at: UNIX_EPOCH,
            source_task_id: 0,
            inherited: false,
        }
    }
}

#[test]
fn detect_stagnation_cases() {
    assert_eq!(
        normalize_direction("Enumerate endpoints on https://target.example.com/api/v2 "),
        "enumerate endpoints on https://target.example.com/api/v#"
    );

    let done = vec![
        node(
            10,
            "intent",
            "Enumerate endpoints on https://target.example.com/api/v2",
        ),
        node(
            22,
            "intent",
            "Enumerate endpoints on https://target.example.com/api/v3",
        ),
        node(
            35,
            "intent",
            "enumerate endpoints on https://target.example.com/api/v9",
        ),
        node(40, "intent", "Review authentication flow for token replay"),
    ];
    let got = detect_stagnation(&done, &[]);
    assert_eq!(got.len(), 1, "expected 1 stagnating direction: {got:?}");
    assert_eq!(got[0].repeats, 3);
    assert_eq!(got[0].since, 10);

    // A finding produced after the first repeat disarms the direction.
    let findings = vec![node(25, "finding", "IDOR found")];
    assert!(detect_stagnation(&done, &findings).is_empty());

    // Below the repeat threshold: nothing.
    assert!(detect_stagnation(&done[..2], &[]).is_empty());
}
