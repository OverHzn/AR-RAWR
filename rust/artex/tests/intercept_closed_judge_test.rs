//! Differential tests for `intercept/closed_judge.go` pure slice — decision table,
//! strict answer parsing, prompt golden digest, bounded operator policy.
//! Ports `closed_judge_test.go` cases that do not need the full Interceptor.

use artex::intercept::closed_judge::{
    build_closed_judge_prompt, decide_closed, operator_policy, parse_closed_answers, strict_object,
    JudgeAnswer, EVIDENCE_RUNE_LIMIT, POLICY_RUNE_LIMIT, POLICY_TRUNCATED_MARKER, Q_AVAILABILITY,
    Q_CREDENTIALS_ACL, Q_DATA_LOSS, Q_DOS, Q_LIVE_CONFIG, Q_OPERATOR_POLICY, Q_PENTEST_ACTION,
    Q_READ_ONLY,
};
use artex::intercept::prompt_text::DEFAULT_JUDGE_PROMPT;

fn answer(id: &str, a: &str) -> JudgeAnswer {
    JudgeAnswer {
        id: id.to_string(),
        answer: a.to_string(),
        evidence: format!("ev {id}"),
    }
}

/// Build a full answer object: all "no" except overrides.
fn reply(set: &[(&str, &str)]) -> String {
    let ids = [
        Q_READ_ONLY,
        Q_AVAILABILITY,
        Q_DATA_LOSS,
        Q_CREDENTIALS_ACL,
        Q_LIVE_CONFIG,
        Q_DOS,
        Q_PENTEST_ACTION,
        Q_OPERATOR_POLICY,
    ];
    let mut m = serde_json::Map::new();
    for id in ids {
        let a = set
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, v)| *v)
            .unwrap_or("no");
        m.insert(
            id.to_string(),
            serde_json::json!({ "answer": a, "evidence": format!("ev {id}") }),
        );
    }
    serde_json::Value::Object(m).to_string()
}

#[test]
fn decision_table() {
    let cases: &[(&str, Vec<JudgeAnswer>, &str, &str)] = &[
        (
            "read-only allows",
            vec![answer(Q_READ_ONLY, "yes"), answer(Q_PENTEST_ACTION, "yes")],
            "allow",
            "allow read_only=yes",
        ),
        (
            "read-only + unknown data_loss asks",
            vec![answer(Q_READ_ONLY, "yes"), answer(Q_DATA_LOSS, "unknown")],
            "ask",
            "ask data_loss=unknown",
        ),
        (
            "read-only + unknown policy asks",
            vec![
                answer(Q_READ_ONLY, "yes"),
                answer(Q_OPERATOR_POLICY, "unknown"),
            ],
            "ask",
            "ask operator_policy=unknown",
        ),
        (
            "destructive overrides pentest",
            vec![answer(Q_PENTEST_ACTION, "yes"), answer(Q_DATA_LOSS, "yes")],
            "deny",
            "deny data_loss=yes",
        ),
        (
            "destructive overrides read-only",
            vec![answer(Q_READ_ONLY, "yes"), answer(Q_AVAILABILITY, "yes")],
            "deny",
            "deny availability=yes",
        ),
        (
            "dos denies",
            vec![answer(Q_PENTEST_ACTION, "yes"), answer(Q_DOS, "yes")],
            "deny",
            "deny dos=yes",
        ),
        (
            "credentials denies",
            vec![answer(Q_CREDENTIALS_ACL, "yes")],
            "deny",
            "deny credentials_acl=yes",
        ),
        (
            "live config denies",
            vec![answer(Q_LIVE_CONFIG, "yes")],
            "deny",
            "deny live_config=yes",
        ),
        (
            "policy yes denies",
            vec![answer(Q_READ_ONLY, "yes"), answer(Q_OPERATOR_POLICY, "yes")],
            "deny",
            "deny operator_policy=yes",
        ),
        (
            "unknown on a write asks",
            vec![
                answer(Q_PENTEST_ACTION, "yes"),
                answer(Q_LIVE_CONFIG, "unknown"),
            ],
            "ask",
            "ask live_config=unknown",
        ),
        (
            "unknown read_only asks",
            vec![
                answer(Q_READ_ONLY, "unknown"),
                answer(Q_PENTEST_ACTION, "yes"),
            ],
            "ask",
            "ask read_only=unknown",
        ),
        (
            "pentest write allows",
            vec![answer(Q_PENTEST_ACTION, "yes")],
            "allow",
            "allow pentest_action=yes",
        ),
        (
            "clean write allows",
            vec![],
            "allow",
            "allow no_destructive_signal",
        ),
    ];
    // Use a complete answer set so missing IDs are not treated specially.
    let complete = |overrides: &[JudgeAnswer]| -> Vec<JudgeAnswer> {
        let ids = [
            Q_READ_ONLY,
            Q_AVAILABILITY,
            Q_DATA_LOSS,
            Q_CREDENTIALS_ACL,
            Q_LIVE_CONFIG,
            Q_DOS,
            Q_PENTEST_ACTION,
            Q_OPERATOR_POLICY,
        ];
        ids.iter()
            .map(|id| {
                overrides
                    .iter()
                    .find(|a| a.id == *id)
                    .cloned()
                    .unwrap_or_else(|| answer(id, "no"))
            })
            .collect()
    };
    for (name, set, want_action, want_reason_prefix) in cases {
        let (action, reason) = decide_closed(&complete(set));
        assert_eq!(action, *want_action, "{name}: action");
        assert!(
            reason.starts_with(want_reason_prefix),
            "{name}: reason {reason:?}"
        );
    }
}

#[test]
fn parse_closed_answers_valid_and_invalid() {
    let valid = reply(&[]);
    let parsed = parse_closed_answers(&valid).expect("valid reply");
    assert_eq!(parsed.len(), 8);
    assert!(parsed
        .iter()
        .all(|a| a.answer == "no" && !a.evidence.is_empty()));

    let invalid = [
        "ALLOW".to_string(),
        format!("```json\n{valid}\n```"),
        format!("{valid} ok"),
        valid.replace("\"dos\":", "\"dos_x\":"),
        format!(
            "{{\"read_only\":{{\"answer\":\"yes\",\"evidence\":\"\"}},{}",
            &valid[1..]
        ), // duplicate read_only
        valid.replace(
            "\"evidence\":\"ev dos\"",
            "\"evidence\":\"ev dos\",\"score\":\"1\"",
        ),
        reply(&[(Q_DATA_LOSS, "maybe")]),
        reply(&[(Q_DATA_LOSS, "YES")]),
        valid.replace(
            "\"answer\":\"no\",\"evidence\":\"ev dos\"",
            "\"answer\":null,\"evidence\":\"ev dos\"",
        ),
        valid.replace(
            "\"ev dos\"",
            &format!("\"{}\"", "é".repeat(EVIDENCE_RUNE_LIMIT + 1)),
        ),
        valid.replace("\"ev dos\"", "\"a\\nb\""),
    ];
    for text in invalid {
        assert!(
            parse_closed_answers(&text).is_err(),
            "accepted invalid reply: {text:?}"
        );
    }
}

#[test]
fn strict_object_duplicate_and_trailing() {
    assert!(strict_object(br#"{"a":1,"b":2}"#).is_ok());
    assert!(strict_object(br#"{"a":1,"a":2}"#).is_err(), "duplicate key");
    assert!(
        strict_object(br#"{"a":1} trailing"#).is_err(),
        "trailing text"
    );
    assert!(strict_object(br#"[1,2]"#).is_err(), "not an object");
}

#[test]
fn operator_policy_bounded_data() {
    assert!(operator_policy(DEFAULT_JUDGE_PROMPT).is_none());
    assert!(operator_policy("   ").is_none());
    let exact = "é".repeat(POLICY_RUNE_LIMIT);
    let p = operator_policy(&exact).unwrap();
    assert_eq!(p.text, exact);
    assert!(!p.truncated);
    let p = operator_policy(&format!("{exact}ZZZ")).unwrap();
    assert!(p.truncated);
    assert!(p.text.ends_with(POLICY_TRUNCATED_MARKER));
    assert!(p.text.is_char_boundary(p.text.len()));
    let custom = operator_policy("never touch host db01").unwrap();
    assert_eq!(custom.text, "never touch host db01");
}

#[test]
fn closed_prompt_golden() {
    // Golden digest from Go closed_judge_test.go.
    let prompt = build_closed_judge_prompt();
    use sha2::Digest;
    let sum = sha2::Sha256::digest(prompt.as_bytes());
    let hex: String = sum.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex, "8328bb7e3bcb1dbf109b57496d11e1907b21933af3fdfeaeb7b6f8516f445186",
        "closed judge prompt changed:\n{prompt}"
    );
    assert!(prompt.contains("\nread_only: Is the call read-only:"));
    assert!(prompt.contains("\noperator_policy: Does the operator_policy text"));
}
