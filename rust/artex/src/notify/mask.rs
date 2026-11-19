//! Rust port of `notify/mask.go` — config masking + the destination-swap guard.
//!
//! The masking layer lets the API echo a channel config back to the browser without
//! leaking credentials (each secret field becomes [`masked_value`]), and makes an
//! update that echoes a masked value mean "keep the stored original". On top of
//! that, [`prepare_config_update`] closes a real exfiltration path: changing a
//! channel's **destination** (where messages go) while saying nothing about the
//! **credentials** (under which identity they are sent) would otherwise send the
//! stored real credentials to an attacker-controlled endpoint.
//!
//! [`masked_value`] / [`is_masked`] / [`is_blank_config_value`] /
//! [`same_config_value`] are reused from the oracle-parity crates via
//! [`crate::notify`]; this module adds the registry-driven [`mask_config`],
//! [`merge_config`], and [`prepare_config_update`] flows.

use serde_json::{Map, Value};

use gocompat::strings::go_trim_space;

use super::{
    destination_keys, is_masked, masked_value, same_config_value, secret_keys, MASKED_PREFIX,
};

/// Mask a config: replace each secret field with a masked value, keep the rest.
///
/// An unknown kind returns an empty map (better "configuration unavailable" than
/// echoing raw content that may contain credentials). Nested structures (e.g.
/// webhook `headers`) are masked as a whole. The input is not mutated.
pub fn mask_config(kind: &str, cfg: &Map<String, Value>) -> Map<String, Value> {
    let secrets = match secret_keys(kind) {
        Some(s) => s,
        None => return Map::new(),
    };
    let mut out = Map::new();
    for (k, v) in cfg {
        if !secrets.contains(&k.as_str()) {
            out.insert(k.clone(), v.clone());
            continue;
        }
        // Nested structures are treated as one credential: mask as a whole.
        match v {
            Value::String(s) => {
                let m = masked_value(s.clone()).unwrap_or_else(|_| MASKED_PREFIX.to_string());
                out.insert(k.clone(), Value::String(m));
            }
            _ => {
                out.insert(k.clone(), Value::String(MASKED_PREFIX.to_string()));
            }
        }
    }
    out
}

/// Merge `incoming` on top of `stored` (Go `MergeConfig`):
/// - a masked incoming string → keep the stored value (field unchanged);
/// - an empty/whitespace incoming string → explicit clear (delete the key);
/// - any other value → overwrite;
/// - keys only in `stored` → kept (partial-update semantics).
pub fn merge_config(
    stored: &Map<String, Value>,
    incoming: &Map<String, Value>,
) -> Map<String, Value> {
    let mut out = stored.clone();
    for (k, v) in incoming {
        if let Value::String(s) = v {
            if is_masked(s.clone()).unwrap_or(false) {
                continue; // masked = unmodified, keep stored
            }
            if go_trim_space(s).is_empty() {
                out.remove(k); // explicit clear
                continue;
            }
            out.insert(k.clone(), Value::String(s.clone()));
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Why a config update was rejected. Mirrors the Go error shapes so the API can
/// give actionable guidance (the `Display` text matches `err.Error()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigUpdateError {
    /// The channel kind is not registered (`channel kind %q is not registered`).
    UnknownKind(String),
    /// The masking sentinel is embedded inside a non-string secret structure.
    MaskedInContainer(String),
    /// A destination key changed but the caller did not take a position on every
    /// credential key. (Go `*ErrDestinationChangedWithoutCredentials`.)
    DestinationChanged {
        changed: Vec<String>,
        missing: Vec<String>,
    },
}

impl std::fmt::Display for ConfigUpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigUpdateError::UnknownKind(kind) => {
                write!(f, "channel kind {kind:?} is not registered")
            }
            ConfigUpdateError::MaskedInContainer(key) => write!(
                f,
                "field {key} contains the masking marker {MASKED_PREFIX:?}: this field can only \
                 be left empty as a whole to keep the original, or submitted as a whole with a \
                 new value; embedding a masking placeholder inside a structure is not allowed"
            ),
            ConfigUpdateError::DestinationChanged { changed, missing } => write!(
                f,
                "destination address ({}) has changed; please also re-fill the credential fields \
                 ({}): enter a new value, or explicitly leave it empty to indicate the credential \
                 is no longer needed. The original credential is only valid for the old address; \
                 continuing to use it amounts to handing it to the new address.",
                changed.join(", "),
                missing.join(", "),
            ),
        }
    }
}

impl std::error::Error for ConfigUpdateError {}

/// `PrepareConfigUpdate` (mask.go): merge a channel config update and enforce the
/// destination-swap rule. When a destination key changes, the caller must take an
/// explicit position on **every** credential key (new value, or explicit empty);
/// omitting a credential or echoing it back masked is rejected.
pub fn prepare_config_update(
    kind: &str,
    stored: &Map<String, Value>,
    incoming: &Map<String, Value>,
) -> Result<Map<String, Value>, ConfigUpdateError> {
    let secrets =
        secret_keys(kind).ok_or_else(|| ConfigUpdateError::UnknownKind(kind.to_string()))?;
    let destinations = destination_keys(kind).unwrap_or(&[]);

    // Reject a masked sentinel stuffed inside a non-string secret structure first
    // (it would be stored as a literal real value, breaking auth silently).
    reject_masked_in_containers(incoming, secrets)?;

    // Destination keys that actually changed (a masked value counts as unchanged).
    let mut changed: Vec<String> = Vec::new();
    for key in destinations {
        let raw = match incoming.get(*key) {
            Some(r) => r,
            None => continue,
        };
        if let Value::String(s) = raw {
            if is_masked(s.clone()).unwrap_or(false) {
                continue;
            }
        }
        let stored_v = stored.get(*key).unwrap_or(&Value::Null);
        if !same_config_value(raw, stored_v) {
            changed.push((*key).to_string());
        }
    }
    if changed.is_empty() {
        // Address unchanged: ordinary merge.
        return Ok(merge_config(stored, incoming));
    }

    // Address changed: require an explicit position on every credential key.
    let mut missing: Vec<String> = Vec::new();
    for key in secrets {
        match incoming.get(*key) {
            None => missing.push((*key).to_string()),
            Some(Value::String(s)) if is_masked(s.clone()).unwrap_or(false) => {
                missing.push((*key).to_string())
            }
            Some(_) => {}
        }
    }
    if !missing.is_empty() {
        return Err(ConfigUpdateError::DestinationChanged { changed, missing });
    }
    Ok(merge_config(stored, incoming))
}

/// `rejectMaskedInContainers` (mask.go): reject a masking sentinel embedded inside
/// a non-string secret value (e.g. webhook `headers`).
fn reject_masked_in_containers(
    incoming: &Map<String, Value>,
    secret_keys: &[&str],
) -> Result<(), ConfigUpdateError> {
    for key in secret_keys {
        let raw = match incoming.get(*key) {
            Some(r) => r,
            None => continue,
        };
        if matches!(raw, Value::String(_)) {
            continue;
        }
        // json.Marshal of a Value is infallible; a serde_json::Value always encodes.
        let encoded = serde_json::to_string(raw).unwrap_or_default();
        if encoded.contains(MASKED_PREFIX) {
            return Err(ConfigUpdateError::MaskedInContainer((*key).to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn merge_keeps_stored_on_masked_and_clears_on_empty() {
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

        let got = merge_config(
            &map(json!({"webhook": "https://real/hook", "secret": "R"})),
            &map(json!({"secret": ""})),
        );
        assert!(got.get("secret").is_none(), "empty string clears");
        assert_eq!(got["webhook"], json!("https://real/hook"));
    }

    #[test]
    fn mask_config_masks_only_secrets() {
        let cfg = map(
            json!({"webhook": "https://x/hook?token=S", "secret": "SECtest123456", "port": 587}),
        );
        let masked = mask_config(crate::notify::KIND_DINGTALK, &cfg);
        assert!(is_masked(masked["webhook"].as_str().unwrap().to_string()).unwrap());
        assert!(is_masked(masked["secret"].as_str().unwrap().to_string()).unwrap());
        assert_eq!(masked["port"], json!(587));
        // Unknown kind -> empty.
        assert!(mask_config("nope", &cfg).is_empty());
    }
}
