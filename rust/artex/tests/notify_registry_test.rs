//! Port of the cross-channel registry + validation tables in
//! `notify/channels_test.go`: every one of the six channels must be registered,
//! report a matching `Kind()`, and — when misconfigured — fail validation with a
//! message that names what is missing (shown verbatim to the configurator).

use artex::notify::{
    channel, kinds, valid_kind, KIND_DINGTALK, KIND_EMAIL, KIND_FEISHU, KIND_TELEGRAM,
    KIND_WEBHOOK, KIND_WECOM,
};
use serde_json::{json, Value};

#[test]
fn registry_covers_all_kinds() {
    // All six channels are required — a missing one would silently vanish from
    // the UI dropdown.
    let want = [
        KIND_DINGTALK,
        KIND_EMAIL,
        KIND_FEISHU,
        KIND_TELEGRAM,
        KIND_WEBHOOK,
        KIND_WECOM,
    ];
    let got = kinds().expect("kinds()");
    assert_eq!(
        got.len(),
        want.len(),
        "channel count should be {}: {got:?}",
        want.len()
    );
    for k in want {
        assert!(
            valid_kind(k.to_string()).expect("valid_kind"),
            "channel {k} not registered"
        );
        let ch = channel(k).expect("channel should exist");
        assert_eq!(
            ch.kind(),
            k,
            "channel {k}'s Kind() does not match the registry key"
        );
    }
    assert!(
        !valid_kind("nope".to_string()).expect("valid_kind"),
        "an unregistered type should not pass validation"
    );
}

#[test]
fn channel_validate_reports_missing_fields() {
    let cases: &[(&str, Value, &str)] = &[
        (KIND_DINGTALK, json!({}), "Webhook"),
        (KIND_FEISHU, json!({}), "Webhook"),
        (KIND_WECOM, json!({}), "Webhook"),
        (KIND_TELEGRAM, json!({}), "Bot Token"),
        (KIND_TELEGRAM, json!({"bot_token": "t"}), "Chat ID"),
        (KIND_EMAIL, json!({}), "SMTP"),
        (KIND_EMAIL, json!({"host": "h"}), "port"),
        (
            KIND_EMAIL,
            json!({"host": "h", "port": 587, "from": "f"}),
            "recipient",
        ),
    ];
    for (kind, cfg, substr) in cases {
        let ch = channel(kind).unwrap_or_else(|| panic!("channel {kind} not registered"));
        let err = match ch.validate(cfg) {
            Ok(()) => panic!("{kind} config {cfg} should fail validation"),
            Err(e) => e,
        };
        let msg = err.message();
        assert!(
            msg.contains(substr),
            "{kind}'s error message should mention {substr:?}, got {msg:?}"
        );
    }
}
