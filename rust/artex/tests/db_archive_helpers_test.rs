//! Differential tests for pure archive helpers — oracle captured live from Go.

use artex::db::archive_helpers::{
    decode_archive_rows, fill_archive_defaults, first_archive_row, json_int64, merge_json_task_id,
    nil_if_empty_string, raw_row_count,
};
use serde_json::json;

#[test]
fn json_scalar_helpers_oracle() {
    assert_eq!(json_int64(&json!(42)), Some(42));
    assert_eq!(json_int64(&json!("17")), Some(17));
    assert_eq!(json_int64(&json!(1.5)), None);
    assert_eq!(raw_row_count(&json!([1, 2, 3])), 3);
    assert_eq!(raw_row_count(&json!({})), 0);
    assert_eq!(raw_row_count(&json!(null)), 0);
    assert_eq!(
        first_archive_row(&json!([{"a": 1}, {"b": 2}])),
        json!({"a": 1})
    );
    assert_eq!(first_archive_row(&json!([])), json!({}));
    assert_eq!(first_archive_row(&json!({})), json!({}));
    assert!(decode_archive_rows(&json!(null)).is_empty());
    assert_eq!(
        decode_archive_rows(&json!([{"id": 1}])),
        vec![json!({"id": 1})]
    );
    assert_eq!(nil_if_empty_string("  "), None);
    assert_eq!(nil_if_empty_string(" x "), Some(" x "));
}

#[test]
fn fill_archive_defaults_oracle() {
    let got = fill_archive_defaults(
        "findings",
        &json!([{"id": 1, "report_state": null}, {"id": 2, "report_state": "open"}, {"id": 3}]),
    )
    .unwrap();
    assert_eq!(
        got,
        json!([
            {"id": 1, "report_state": "none"},
            {"id": 2, "report_state": "open"},
            {"id": 3, "report_state": "none"},
        ])
    );

    let got = fill_archive_defaults(
        "side_question_sessions",
        &json!([{"session_key": "a"}, {"session_key": "b", "memory": {"x": 1}}]),
    )
    .unwrap();
    assert_eq!(
        got,
        json!([
            {"session_key": "a", "memory": {}},
            {"session_key": "b", "memory": {"x": 1}},
        ])
    );

    assert_eq!(
        fill_archive_defaults("side_question_requests", &json!([{"id": "x"}])).unwrap(),
        json!([{"id": "x", "context_info": {}}]),
    );
    assert_eq!(
        fill_archive_defaults("other", &json!([{"a": 1}])).unwrap(),
        json!([{"a": 1}])
    );
}

#[test]
fn merge_json_task_id_oracle() {
    assert_eq!(
        merge_json_task_id(&json!([3, 1, 3, 0, -2]), 9),
        vec![3, 1, 9]
    );
    assert_eq!(merge_json_task_id(&json!("7"), 9), vec![9]);
    assert_eq!(merge_json_task_id(&json!(null), 9), vec![9]);
    assert_eq!(merge_json_task_id(&json!({}), 9), vec![9]);
}
