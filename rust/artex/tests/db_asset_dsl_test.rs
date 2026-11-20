//! Differential tests for the pure `db/asset_dsl.go` parser — `ValidateDSL`
//! oracle captured live from Go + WHERE builder structure/params assertions.

use artex::db::asset_dsl::{
    build_dsl_where, build_dsl_where_base, parse_dsl, validate_dsl, AstNode, DslParam,
};

/// (dsl, expected_ok) — oracle from Go `db.ValidateDSL`.
const CASES: &[(&str, bool)] = &[
    ("", true),
    ("   ", true),
    ("example.com", true),
    ("domain=example", true),
    ("domain==example.com", true),
    ("domain!=evil", true),
    ("port>8080", true),
    ("port>=443", true),
    ("port<1000", true),
    ("port<=80", true),
    ("task_id=7", true),
    ("company_id=3", true),
    ("technology=go", true),
    ("technology==rust", true),
    ("technology!=java", true),
    ("status=200", true),
    ("title=hello", true),
    ("ip=10.0.0.1", true),
    ("domain=example AND port>80", true),
    ("domain=example OR ip=10.0.0.1", true),
    ("(domain=a OR domain=b) AND port=443", true),
    ("domain=example AND (port>80 OR port<20)", true),
    ("AND", false),
    ("OR", false),
    ("(", false),
    (")", false),
    ("(domain=a", false),
    ("domain=a)", false),
    ("badfield=x", true),
    ("port=abc", false),
    ("task_id=xx", false),
    ("domain>5", false),
    ("technology>5", false),
    ("domain=a AND", false),
    ("domain=a OR", false),
    ("foo bar", false),
    ("domain=\"hello world\"", true),
    ("DOMAIN=Example AND STATUS_CODE>=200", true),
];

#[test]
fn validate_dsl_matches_go_oracle() {
    for &(dsl, want_ok) in CASES {
        let got = validate_dsl(dsl);
        assert_eq!(got.is_ok(), want_ok, "ValidateDSL({dsl:?}) = {got:?}");
    }
}

#[test]
fn where_builder_structure_and_params() {
    let node = parse_dsl("domain=example AND port>80").unwrap().unwrap();
    let (clause, params) = build_dsl_where(Some(&node)).unwrap();
    assert_eq!(clause, "(domain ILIKE $1) AND (port > $2)");
    assert_eq!(
        params,
        vec![DslParam::Text("%example%".into()), DslParam::Bigint(80)]
    );

    // Base offset leaves $1 free (scope CTE task id).
    let (clause2, params2) = build_dsl_where_base(Some(&node), 1).unwrap();
    assert_eq!(clause2, "(domain ILIKE $2) AND (port > $3)");
    assert_eq!(params2, params);

    // Empty DSL -> 1=1, no params.
    let (empty, p) = build_dsl_where(None).unwrap();
    assert_eq!(empty, "1=1");
    assert!(p.is_empty());

    // Bare text fans out across full-text cols + arrays with one shared param.
    let node = parse_dsl("hello").unwrap().unwrap();
    let (clause, params) = build_dsl_where(Some(&node)).unwrap();
    assert!(clause.starts_with("(domain ILIKE $1 OR root_domain ILIKE $1"));
    assert!(clause.contains("unnest(technologies)"));
    assert!(clause.contains("unnest(bound_domains)"));
    assert_eq!(params, vec![DslParam::Text("%hello%".into())]);

    // Array field == and !=.
    let node = parse_dsl("technology==rust").unwrap().unwrap();
    let (clause, params) = build_dsl_where(Some(&node)).unwrap();
    assert_eq!(clause, "$1 = ANY(technologies)");
    assert_eq!(params, vec![DslParam::Text("rust".into())]);

    // task_id / company_id are bigint params.
    let node = parse_dsl("task_id=7 OR company_id=3").unwrap().unwrap();
    let (clause, params) = build_dsl_where(Some(&node)).unwrap();
    assert_eq!(clause, "($1 = ANY(task_ids)) OR (company_id = $2)");
    assert_eq!(params, vec![DslParam::Bigint(7), DslParam::Bigint(3)]);

    // AND binds tighter than OR: (a AND b) OR c.
    let node = parse_dsl("domain=a AND port=1 OR ip=x").unwrap().unwrap();
    assert!(matches!(node, AstNode::Or(_)), "OR is the root");
    let (clause, _) = build_dsl_where(Some(&node)).unwrap();
    assert_eq!(
        clause,
        "((domain ILIKE $1) AND (port = $2)) OR (ip ILIKE $3)"
    );
}
