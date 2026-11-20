//! Pure archive helpers — slice of `db/task_archives_restore.go` +
//! `db/task_archives.go`, used by the snapshot/restore path.

use serde_json::Value as Json;

/// `jsonInt64`: an integer from a JSON number/string (no fractional floats).
pub fn json_int64(v: &Json) -> Option<i64> {
    match v {
        Json::Number(n) => n.as_i64(),
        Json::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

/// `rawRowCount`: number of rows in an archive JSON array (0 on bad input).
pub fn raw_row_count(raw: &Json) -> i64 {
    raw.as_array().map(|a| a.len() as i64).unwrap_or(0)
}

/// `firstArchiveRow`: first row of an archive array, or `{}`.
pub fn first_archive_row(raw: &Json) -> Json {
    raw.as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

/// `decodeArchiveRows`: rows as objects; UseNumber makes integer ids exact.
pub fn decode_archive_rows(raw: &Json) -> Vec<Json> {
    if raw.is_null() {
        return Vec::new();
    }
    raw.as_array().cloned().unwrap_or_default()
}

/// Columns older archives predate; their absent key would otherwise insert NULL
/// into a NOT NULL column and reject the whole restore.
pub fn archive_defaults(table: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match table {
        "side_question_sessions" => Some(&[("memory", "{}")]),
        "side_question_requests" => Some(&[("context_info", "{}")]),
        "findings" => Some(&[("report_state", "\"none\"")]),
        _ => None,
    }
}

/// `fillArchiveDefaults`: restore the default of a column an older archive
/// predates (absent or `null` value).
pub fn fill_archive_defaults(table: &str, raw: &Json) -> Result<Json, String> {
    let Some(defaults) = archive_defaults(table) else {
        return Ok(raw.clone());
    };
    let Some(rows) = raw.as_array() else {
        return Err("archive rows must be a JSON array".to_string());
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut row = row
            .as_object()
            .cloned()
            .ok_or_else(|| "archive row must be an object".to_string())?;
        for (field, def) in defaults {
            let missing = row.get(*field).map(|v| v.is_null()).unwrap_or(true);
            if missing {
                let value: Json = serde_json::from_str(def).map_err(|e| e.to_string())?;
                row.insert((*field).to_string(), value);
            }
        }
        out.push(Json::Object(row));
    }
    Ok(Json::Array(out))
}

/// `mergeJSONTaskID`: add the task id to an array of positive, unique ids.
pub fn merge_json_task_id(value: &Json, task_id: i64) -> Vec<i64> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(values) = value.as_array() {
        for item in values {
            if let Some(id) = json_int64(item) {
                if id > 0 && seen.insert(id) {
                    out.push(id);
                }
            }
        }
    }
    if seen.insert(task_id) {
        out.push(task_id);
    }
    out
}

/// `nilIfEmptyString`: blank strings become SQL NULL.
pub fn nil_if_empty_string(value: &str) -> Option<&str> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// `remapArchiveAssetReferences`: rewrite old asset ids to their restored ids
/// in anchor/link rows and in `asset_ids` arrays (node payloads, findings).
pub fn remap_archive_asset_references(
    tables: &std::collections::BTreeMap<String, Json>,
    mapping: &std::collections::HashMap<i64, i64>,
) -> std::collections::BTreeMap<String, Json> {
    let mut out = tables.clone();
    for table in ["exploration_anchors", "task_asset_links"] {
        let Some(rows) = tables.get(table).and_then(|v| v.as_array()) else {
            continue;
        };
        let mut rows = rows.clone();
        for row in &mut rows {
            if let Some(obj) = row.as_object_mut() {
                if let Some(old) = obj.get("asset_id").and_then(json_int64) {
                    if let Some(new) = mapping.get(&old) {
                        obj.insert(
                            "asset_id".to_string(),
                            Json::Number(serde_json::Number::from(*new)),
                        );
                    }
                }
            }
        }
        out.insert(table.to_string(), Json::Array(rows));
    }
    for table in ["exploration_nodes", "findings"] {
        let Some(rows) = tables.get(table).and_then(|v| v.as_array()) else {
            continue;
        };
        let mut rows = rows.clone();
        for row in &mut rows {
            let Some(obj) = row.as_object_mut() else {
                continue;
            };
            let container = if table == "exploration_nodes" {
                match obj.get_mut("payload").and_then(|v| v.as_object_mut()) {
                    Some(p) => p,
                    None => continue,
                }
            } else {
                obj
            };
            if let Some(values) = container
                .get_mut("asset_ids")
                .and_then(|v| v.as_array_mut())
            {
                for value in values {
                    if let Some(old) = json_int64(value) {
                        if let Some(new) = mapping.get(&old) {
                            *value = Json::Number(serde_json::Number::from(*new));
                        }
                    }
                }
            }
        }
        out.insert(table.to_string(), Json::Array(rows));
    }
    out
}
