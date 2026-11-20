//! Differential tests for intercept rule matching — oracle captured live from
//! Go `ruleMatches`/`compileRule` (9 cases).

use artex::db::intercept_store::InterceptRule;
use artex::intercept::rule_match::{compile_rule, rule_matches};

fn rule(target: &str, typ: &str, pattern: &str, action: &str) -> InterceptRule {
    InterceptRule {
        id: 1,
        name: "r".into(),
        enabled: true,
        priority: 1,
        match_target: target.into(),
        match_type: typ.into(),
        pattern: pattern.into(),
        action: action.into(),
        message: String::new(),
        timeout_enabled: false,
        timeout_seconds: 0,
        timeout_action: String::new(),
        created_at: std::time::UNIX_EPOCH,
        updated_at: std::time::UNIX_EPOCH,
    }
}

#[test]
fn rule_matches_oracle() {
    let cmd = br#"{"command":"git push --force"}"#;
    let cases: Vec<(InterceptRule, &str, &[u8], bool)> = vec![
        (
            rule("tool_name", "string", "Bash", "deny"),
            "Bash",
            &[],
            true,
        ),
        (
            rule("tool_name", "string", "Bash", "deny"),
            "Write",
            &[],
            false,
        ),
        (
            rule("tool_input", "string", "--force", "deny"),
            "Bash",
            cmd,
            true,
        ),
        (
            rule("tool_input", "regex", r"^--force$", "deny"),
            "Bash",
            b"--force",
            true,
        ),
        (
            rule("tool_input", "regex", "(", "deny"),
            "Bash",
            b"x",
            false,
        ),
        (
            rule("tool_input", "argv", r#"["git","push"]"#, "allow"),
            "Bash",
            cmd,
            true,
        ),
        (
            rule("tool_input", "argv", r#"["git","push"]"#, "deny"),
            "Bash",
            cmd,
            true,
        ),
        (
            rule("tool_input", "argv", r#"["python"]"#, "allow"),
            "Bash",
            br#"{"command":"python3 -c x"}"#,
            false,
        ),
        (rule("bad", "string", "Bash", "deny"), "Bash", &[], false),
    ];
    for (i, (r, tool, input, want)) in cases.into_iter().enumerate() {
        let Some(c) = compile_rule(r) else {
            assert!(!want, "case {i}: valid rule failed to compile");
            continue;
        };
        assert_eq!(rule_matches(&c, tool, input), want, "case {i}");
    }
}

#[test]
fn match_first_priority_and_default_message() {
    use artex::intercept::rule_match::match_first;

    let mut deny = rule("tool_input", "string", "--force", "deny");
    deny.id = 1;
    deny.name = "no force".into();
    deny.priority = 50;
    let mut allow = rule("tool_input", "argv", r#"["git","push"]"#, "allow");
    allow.id = 2;
    allow.name = "allow git push".into();
    allow.priority = -1000;
    let rules: Vec<_> = [allow, deny].into_iter().filter_map(compile_rule).collect();

    let force = br#"{"command":"git push --force"}"#;
    let d = match_first(&rules, "Bash", force).unwrap();
    assert_eq!(d.rule_id, 1);
    assert_eq!(d.action, "deny");
    assert_eq!(
        d.message,
        "intercept rule [no force] forbids executing this tool"
    );

    let plain = br#"{"command":"git push origin"}"#;
    let d = match_first(&rules, "Bash", plain).unwrap();
    assert_eq!(d.rule_id, 2);
    assert_eq!(d.action, "allow");
    assert_eq!(d.message, "");
}
