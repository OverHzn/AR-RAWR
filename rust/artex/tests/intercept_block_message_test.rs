//! Differential tests for model-facing block messages — port of Go guard tests
//! `TestTargetBlockReasonIsCapped` + `TestShortBlockReasonUnchanged`.

use artex::intercept::block_message::{
    cap_block_reason, system_block_message, target_block_message, TargetResult, TARGET_DENIED,
};

#[test]
fn short_block_reason_unchanged() {
    for s in ["", "Destructive command rejected", &"日".repeat(4000)] {
        assert_eq!(
            cap_block_reason(s),
            s,
            "reason of {} runes changed",
            s.chars().count()
        );
    }
    assert_eq!(
        system_block_message("Destructive command rejected"),
        "[ARTEX platform control, not target defense] This call was intercepted by the platform. Reason: Destructive command rejected. This operation is forbidden."
    );
    let res = TargetResult {
        code: TARGET_DENIED.to_string(),
        host: "app.example.com".to_string(),
        port: 443,
        reason: "the request was denied".to_string(),
    };
    let msg = target_block_message(&res);
    assert!(msg.starts_with(
        "[ARTEX platform control, not target defense] TARGET_DENIED: target app.example.com:443: the request was denied."
    ), "got {msg}");
}

#[test]
fn oversized_reason_capped_with_target_and_guidance() {
    let res = TargetResult {
        code: TARGET_DENIED.to_string(),
        host: "app.example.com".to_string(),
        port: 443,
        reason: "é".repeat(20_000),
    };
    let msg = target_block_message(&res);
    assert!(
        msg.chars().count() <= 4000 + 256,
        "message too long: {}",
        msg.chars().count()
    );
    assert!(msg.starts_with(
        "[ARTEX platform control, not target defense] TARGET_DENIED: target app.example.com:443:"
    ), "prefix lost: {msg}");
    assert!(
        msg.contains(&format!("{} …(truncated)", "é".repeat(4000))),
        "reason not capped: {msg}"
    );
    assert!(
        msg.contains("do not retry it and do not reach it another way"),
        "guidance lost: {msg}"
    );

    // System variant caps identically.
    let sys = system_block_message(&"日".repeat(20_000));
    assert!(
        sys.ends_with("…(truncated). This operation is forbidden."),
        "got {sys}"
    );
}
