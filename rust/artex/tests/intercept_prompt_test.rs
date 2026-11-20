//! Differential tests for `intercept/prompt.go` — all cases from
//! `intercept/prompt_test.go` + EffectiveJudgePrompt assembly. Pure.

use artex::intercept::prompt::{default_judge_prompt, effective_judge_prompt, parse_verdict};

#[test]
fn parse_verdict_all_actions() {
    for action in ["allow", "ask", "deny"] {
        let reason = "Actual action: write a report containing the words ALLOW, DENY and ASK; Consequence if successful: saves the text, does not execute commands in the body; Matched rule: custom clause";
        let raw = serde_json::json!({ "decision": action, "comment": reason }).to_string();
        let got = parse_verdict(&format!("\n{raw}\n"));
        assert_eq!(got.action, action, "action {action}");
        assert_eq!(got.reason, reason, "reason {action}");
    }
}

#[test]
fn parse_verdict_rejects_incomplete_or_ambiguous() {
    let valid = r#"{"decision":"allow","comment":"Actual action: read file; Consequence if successful: returns content; Matched rule: A5"}"#;
    let replies = [
        "",
        "ALLOW",
        "DENY: hit D4",
        "allow: ALLOW",
        "ASK: ownership unclear",
        r#"{"decision":"allow"}"#,
        r#"{"decision":"approve","comment":"Actual action: read; Consequence if successful: returns content; Matched rule: A5"}"#,
        r#"{"decision":"allow","comment":null}"#,
        r#"{"decision":"allow","comment":123}"#,
        &valid.replace("Actual action: read file", "Actual action: "),
        &valid.replace(
            "Consequence if successful: returns content",
            "Consequence if successful: ",
        ),
        &valid.replace("Matched rule: A5", "Matched rule: "),
        &valid.replace("; Matched rule: A5", ""),
        &valid.replace(
            "\"decision\":\"allow\"",
            "\"decision\":\"deny\",\"decision\":\"allow\"",
        ),
        &valid.replace(
            "\"decision\":\"allow\"",
            "\"extra\":true,\"decision\":\"allow\"",
        ),
        &format!("{valid}{valid}"),
        &valid[..valid.len() - 1],
        // Unclosed fence = truncated reply: must not be completed.
        &format!("```json\n{}", &valid[..valid.len() - 1]),
        &format!("```json\n{valid}\n```\nAlso, I recommend a follow-up manual review."),
        &format!("My verdict is:\n{valid}"),
    ];
    for reply in replies {
        let got = parse_verdict(reply);
        assert!(
            got.action.is_empty(),
            "accepted incomplete/ambiguous verdict: {reply:?} => {got:?}"
        );
    }
}

#[test]
fn parse_verdict_unwraps_code_fence() {
    let deny = r#"{"decision":"deny","comment":"Actual action: delete production files; Consequence if successful: business data is lost; Matched rule: D4"}"#;
    for reply in [
        format!("```json\n{deny}\n```"),
        format!("```JSON\n{deny}\n```"),
        format!("```\n{deny}\n```"),
        format!("  ```json\n{deny}\n```  "),
    ] {
        let got = parse_verdict(&reply);
        assert_eq!(got.action, "deny", "fenced verdict lost: {reply:?}");
        assert!(
            got.reason.ends_with("Matched rule: D4"),
            "reason lost: {reply:?}"
        );
    }
}

#[test]
fn parse_verdict_keeps_complete_explanation() {
    let reason = format!(
        "Actual action: {}; Consequence if successful: only saves the file; Matched rule: A2",
        "write report ".repeat(30)
    );
    let raw = serde_json::json!({ "decision": "allow", "comment": &reason }).to_string();
    assert_eq!(
        parse_verdict(&raw).reason,
        reason,
        "explanation truncated or rule lost"
    );
    let too_long =
        serde_json::json!({ "decision": "allow", "comment": "x".repeat(2401) }).to_string();
    assert!(
        parse_verdict(&too_long).action.is_empty(),
        "accepted unbounded explanation"
    );
}

#[test]
fn effective_prompt_appends_missing_sections() {
    let built_in = effective_judge_prompt("my custom policy");
    assert!(built_in.starts_with("my custom policy"));
    assert!(built_in.contains("# Review input boundary"));
    assert!(built_in.contains("# Verdict output protocol"));
    // Idempotent: applying twice does not duplicate sections.
    assert_eq!(effective_judge_prompt(&built_in), built_in);
    assert!(default_judge_prompt().contains("# Role"));
}
