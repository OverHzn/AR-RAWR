//! Rust port of the pure `notify/mask_test.go` specs — config masking and the
//! destination-swap guard ([`artex::notify::mask`]). `masked_value`/`is_masked`
//! come from the oracle-parity `notify-rs`; the merge / prepare flows are the fresh
//! port and these tests pin them to the Go behavior (including the exact rejection
//! semantics).

use serde_json::{json, Map, Value};

use artex::notify::mask::{mask_config, merge_config, prepare_config_update, ConfigUpdateError};
use artex::notify::{
    is_masked, masked_value, KIND_DINGTALK, KIND_EMAIL, KIND_TELEGRAM, KIND_WEBHOOK, MASKED_PREFIX,
};

fn map(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// `TestMaskedValueHidesBodyButKeepsTailHint`.
#[test]
fn masked_value_hides_body_but_keeps_tail_hint() {
    let secret = "https://oapi.dingtalk.com/robot/send?access_token=abcdef123456";
    let got = masked_value(secret.to_string()).unwrap();
    assert!(
        !got.contains("abcdef123456"),
        "leaked the full credential: {got:?}"
    );
    assert!(
        !got.contains("oapi.dingtalk.com"),
        "exposed the address body: {got:?}"
    );
    assert!(
        got.ends_with("123456"),
        "should keep the last 6 chars as a hint: {got:?}"
    );
    assert!(
        is_masked(got.clone()).unwrap(),
        "a masked value must be recognized: {got:?}"
    );
}

/// `TestMaskedValueShortSecretGivesNoHint`.
#[test]
fn masked_value_short_secret_gives_no_hint() {
    for s in ["abc", "abcdef", ""] {
        let got = masked_value(s.to_string()).unwrap();
        assert_eq!(
            got,
            MASKED_PREFIX,
            "len {} should give no tail hint",
            s.len()
        );
        if !s.is_empty() {
            assert!(
                !got.contains(s),
                "masked value contains the original: {got:?}"
            );
        }
    }
}

/// `TestMaskConfigMasksOnlySecrets`.
#[test]
fn mask_config_masks_only_secrets() {
    let cfg = map(json!({
        "webhook": "https://example.com/hook?token=SECRETVALUE",
        "secret": "SECtest123456",
        "port": 587,
        "host": "smtp.example.com",
    }));
    let masked = mask_config(KIND_DINGTALK, &cfg);
    for k in ["webhook", "secret"] {
        let s = masked[k].as_str().unwrap();
        assert!(
            is_masked(s.to_string()).unwrap(),
            "{k} should be masked, got {s:?}"
        );
    }
    assert_eq!(
        masked["port"],
        json!(587),
        "non-credential field must not change"
    );
}

/// `TestMaskConfigUnknownKindReturnsEmpty`.
#[test]
fn mask_config_unknown_kind_returns_empty() {
    let got = mask_config("nope", &map(json!({"webhook": "https://x/y?token=LEAK"})));
    assert!(
        got.is_empty(),
        "unknown kind should return an empty config, got {got:?}"
    );
}

/// `TestMaskConfigDoesNotMutateInput`.
#[test]
fn mask_config_does_not_mutate_input() {
    let cfg = map(json!({"webhook": "https://example.com/hook", "secret": "SECtest123456"}));
    let _ = mask_config(KIND_DINGTALK, &cfg);
    assert!(
        !is_masked(cfg["secret"].as_str().unwrap().to_string()).unwrap(),
        "MaskConfig mutated its input"
    );
}

/// `TestMergeConfigKeepsStoredOnMaskedIncoming`.
#[test]
fn merge_config_keeps_stored_on_masked_incoming() {
    let stored =
        map(json!({"webhook": "https://real/hook", "secret": "REALSECRET", "method": "POST"}));
    let incoming = map(json!({
        "webhook": masked_value("https://real/hook".into()).unwrap(),
        "secret": masked_value("REALSECRET".into()).unwrap(),
        "method": "PUT",
    }));
    let got = merge_config(&stored, &incoming);
    assert_eq!(got["webhook"], json!("https://real/hook"));
    assert_eq!(got["secret"], json!("REALSECRET"));
    assert_eq!(got["method"], json!("PUT"));
}

/// `TestMergeConfigEmptyStringClears`.
#[test]
fn merge_config_empty_string_clears() {
    let stored = map(json!({"webhook": "https://real/hook", "secret": "REALSECRET"}));
    let got = merge_config(&stored, &map(json!({"secret": ""})));
    assert!(
        got.get("secret").is_none(),
        "empty string should clear, got {got:?}"
    );
    assert_eq!(
        got["webhook"],
        json!("https://real/hook"),
        "unmentioned fields kept"
    );
}

/// `TestMergeConfigKeepsUnmentionedStoredKeys`.
#[test]
fn merge_config_keeps_unmentioned_stored_keys() {
    let stored = map(json!({"host": "smtp.example.com", "port": 587, "password": "pw"}));
    let got = merge_config(&stored, &map(json!({"port": 465})));
    assert_eq!(got["host"], json!("smtp.example.com"));
    assert_eq!(got["password"], json!("pw"));
    assert_eq!(got["port"], json!(465));
}

/// `TestPrepareConfigUpdateBlocksDestinationSwap` — the most important security
/// invariant: changing the destination must not carry the old credentials along.
#[test]
fn prepare_config_update_blocks_destination_swap() {
    let cases: Vec<(&str, &str, Value, Value, &str)> = vec![
        (
            "generic webhook changing URL while keeping Authorization",
            KIND_WEBHOOK,
            json!({"url": "https://legit.example.com/hook", "headers": {"Authorization": "Bearer REAL-TOKEN"}}),
            json!({"url": "https://attacker.tld/c"}),
            "headers",
        ),
        (
            "Telegram changing base_url to its own endpoint",
            KIND_TELEGRAM,
            json!({"bot_token": "123456:REAL", "chat_id": "1", "base_url": "https://api.telegram.org"}),
            json!({"base_url": "https://attacker.tld"}),
            "bot_token",
        ),
        (
            "email changing SMTP host to hand over password",
            KIND_EMAIL,
            json!({"host": "smtp.corp.com", "port": 587, "password": "REALPW", "from": "a@b.c", "to": ["d@e.f"]}),
            json!({"host": "smtp.attacker.tld"}),
            "password",
        ),
        (
            "email toggling TLS must re-declare password",
            KIND_EMAIL,
            json!({"host": "smtp.corp.com", "port": 587, "tls": false, "password": "REALPW", "from": "a@b.c", "to": ["d@e.f"]}),
            json!({"tls": true}),
            "password",
        ),
        (
            "passing back masked credentials + new address",
            KIND_TELEGRAM,
            json!({"bot_token": "123456:REAL", "chat_id": "1", "base_url": "https://api.telegram.org"}),
            json!({"base_url": "https://attacker.tld", "bot_token": masked_value("123456:REAL".into()).unwrap()}),
            "bot_token",
        ),
        (
            "DingTalk changing webhook while keeping signing secret",
            KIND_DINGTALK,
            json!({"webhook": "https://oapi.dingtalk.com/robot/send?access_token=OLD", "secret": "REALSEC"}),
            json!({"webhook": "https://attacker.tld/hook"}),
            "secret",
        ),
    ];
    for (name, kind, stored, incoming, want_missing) in cases {
        let stored = map(stored);
        let incoming = map(incoming);
        let err = prepare_config_update(kind, &stored, &incoming).expect_err(&format!(
            "{name}: changing address without credentials should be rejected"
        ));
        match &err {
            ConfigUpdateError::DestinationChanged { missing, .. } => {
                assert!(
                    missing.contains(&want_missing.to_string()),
                    "{name}: {want_missing} should be named, got {missing:?}"
                );
            }
            other => panic!("{name}: want DestinationChanged, got {other:?}"),
        }
        assert!(
            err.to_string().contains(want_missing),
            "{name}: error message should mention {want_missing}: {err}"
        );
    }
}

/// `TestPrepareConfigUpdateAllowsLegitimateEdits`.
#[test]
fn prepare_config_update_allows_legitimate_edits() {
    // (name, kind, stored, incoming)
    let cases: Vec<(&str, &str, Value, Value)> = vec![
        (
            "only renaming (config passed back as-is)",
            KIND_WEBHOOK,
            json!({"url": "https://legit.example.com/hook", "headers": {"Authorization": "Bearer REAL"}}),
            json!({"url": masked_value("https://legit.example.com/hook".into()).unwrap()}),
        ),
        (
            "only changing the method",
            KIND_WEBHOOK,
            json!({"url": "https://legit.example.com/hook", "method": "POST"}),
            json!({"method": "PUT"}),
        ),
        (
            "changing address AND providing new credentials",
            KIND_WEBHOOK,
            json!({"url": "https://old.example.com/hook", "headers": {"Authorization": "Bearer OLD"}}),
            json!({"url": "https://new.example.com/hook", "headers": {"Authorization": "Bearer NEW"}}),
        ),
        (
            "changing address, credential explicitly dropped",
            KIND_WEBHOOK,
            json!({"url": "https://old.example.com/hook", "headers": {"Authorization": "Bearer OLD"}}),
            json!({"url": "https://new.example.com/hook", "headers": ""}),
        ),
        (
            "Telegram changing chat_id (not a destination)",
            KIND_TELEGRAM,
            json!({"bot_token": "t", "chat_id": "1", "base_url": "https://api.telegram.org"}),
            json!({"chat_id": "-100200"}),
        ),
        (
            "email changing recipients (not a destination)",
            KIND_EMAIL,
            json!({"host": "smtp.corp.com", "port": 587, "password": "PW", "from": "a@b.c", "to": ["x@y.z"]}),
            json!({"to": ["new@y.z"]}),
        ),
    ];
    for (name, kind, stored, incoming) in cases {
        let merged = prepare_config_update(kind, &map(stored), &map(incoming))
            .unwrap_or_else(|e| panic!("{name}: legitimate edit was blocked: {e}"));
        assert!(!merged.is_empty(), "{name}: should return a merged result");
    }
}

/// `TestPrepareConfigUpdatePortTypeTolerance`: an identical port (type aside) is not
/// an address change; a real port change is blocked.
#[test]
fn prepare_config_update_port_type_tolerance() {
    let stored = map(json!({"host": "smtp.corp.com", "port": 587.0, "password": "PW"}));
    assert!(prepare_config_update(KIND_EMAIL, &stored, &map(json!({"port": 587}))).is_ok());
    assert!(prepare_config_update(KIND_EMAIL, &stored, &map(json!({"port": 25}))).is_err());
}

/// `TestPrepareConfigUpdateSurvivesRepeatedSaveWithBlankDestination`.
#[test]
fn prepare_config_update_survives_repeated_save_with_blank_destination() {
    let stored = map(json!({"bot_token": "123:ABC", "chat_id": "-100", "base_url": ""}));
    let submit = || {
        map(json!({
            "bot_token": masked_value("123:ABC".into()).unwrap(),
            "chat_id": "-100",
            "base_url": "",
        }))
    };

    let merged =
        prepare_config_update(KIND_TELEGRAM, &stored, &submit()).expect("first save blocked");
    assert!(
        merged.get("base_url").is_none(),
        "empty string should be deleted by MergeConfig"
    );

    let merged2 =
        prepare_config_update(KIND_TELEGRAM, &merged, &submit()).expect("second save blocked");
    let _ = prepare_config_update(KIND_TELEGRAM, &merged2, &submit()).expect("third save blocked");
    assert_eq!(
        merged2["bot_token"],
        json!("123:ABC"),
        "Bot Token should survive"
    );
}

/// `TestPrepareConfigUpdateStillGuardsBlankDestinationChanges`.
#[test]
fn prepare_config_update_still_guards_blank_destination_changes() {
    let official = map(json!({"bot_token": "123:ABC", "chat_id": "-100"}));
    assert!(
        prepare_config_update(KIND_TELEGRAM, &official, &map(json!({"bot_token": masked_value("123:ABC".into()).unwrap(), "base_url": "https://tg-proxy.attacker.tld"}))).is_err(),
        "official -> self-hosted must require re-entering the Token"
    );
    let proxied = map(json!({"bot_token": "123:ABC", "base_url": "https://proxy.internal/bot"}));
    assert!(
        prepare_config_update(
            KIND_TELEGRAM,
            &proxied,
            &map(json!({"bot_token": masked_value("123:ABC".into()).unwrap(), "base_url": ""}))
        )
        .is_err(),
        "clearing a self-hosted address must require re-entering the Token"
    );
}

/// `TestSecretKeysDeclaredForEveryKind` / `TestDestinationKeysDeclaredForEveryKind`:
/// every registered kind must declare both key sets (else masking / the guard is
/// ineffective for it).
#[test]
fn every_kind_declares_secret_and_destination_keys() {
    for kind in [
        "dingtalk", "feishu", "wecom", "webhook", "telegram", "email",
    ] {
        assert!(
            artex::notify::secret_keys(kind).is_some_and(|s| !s.is_empty()),
            "{kind} declares no credential keys"
        );
        assert!(
            artex::notify::destination_keys(kind).is_some_and(|d| !d.is_empty()),
            "{kind} declares no destination keys"
        );
    }
}

/// `TestPrepareConfigUpdateRejectsMaskedInContainer`: a sentinel embedded inside a
/// non-string secret structure is rejected; submitting a new object is accepted.
#[test]
fn prepare_config_update_rejects_masked_in_container() {
    let stored = map(
        json!({"url": "https://legit.example.com/hook", "headers": {"Authorization": "Bearer REAL"}}),
    );
    assert!(
        prepare_config_update(
            KIND_WEBHOOK,
            &stored,
            &map(json!({"headers": {"Authorization": MASKED_PREFIX}}))
        )
        .is_err(),
        "a sentinel embedded inside a structure should be rejected"
    );
    assert!(
        prepare_config_update(
            KIND_WEBHOOK,
            &stored,
            &map(json!({"headers": {"Authorization": "Bearer NEW"}}))
        )
        .is_ok(),
        "submitting new headers normally should be accepted"
    );
}

/// `TestPrepareConfigUpdateUnknownKind`: an unknown kind is its own error.
#[test]
fn prepare_config_update_unknown_kind() {
    let err = prepare_config_update("nope", &Map::new(), &Map::new()).unwrap_err();
    assert!(
        matches!(err, ConfigUpdateError::UnknownKind(_)),
        "got {err:?}"
    );
}
