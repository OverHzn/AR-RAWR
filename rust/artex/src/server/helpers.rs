//! Pure server helpers — slice of `server/manager.go`.

use serde_json::Value as Json;

/// `decodeStrMap`: JSON object -> string map; null / invalid / empty input
/// yields a writable empty map (callers assign into it).
pub fn decode_str_map(raw: Option<&[u8]>) -> std::collections::BTreeMap<String, String> {
    let Some(raw) = raw else {
        return std::collections::BTreeMap::new();
    };
    if raw.is_empty() {
        return std::collections::BTreeMap::new();
    }
    match serde_json::from_slice::<Option<Json>>(raw) {
        Ok(Some(Json::Object(map))) => map
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect(),
        _ => std::collections::BTreeMap::new(),
    }
}

/// `encodeJSON`: serialize a value to JSON bytes (None on failure).
pub fn encode_json(v: &impl serde::Serialize) -> Option<Vec<u8>> {
    serde_json::to_vec(v).ok()
}
