//! Typed bbot ingestion — pure port of `agent/bbot_ingest.go` (#124, F10b).
//!
//! bbot output.json is shaped by the target and by third parties writing about
//! it. Only typed fields are read; free text survives as one inert quoted
//! string and is never used for scope, routing or an action.

use regex::Regex;

pub const FACT_SOURCE_TOOL_OUTPUT: &str = "tool_output";
pub const FACT_SOURCE_THIRD_PARTY_INTEL: &str = "third_party_intel";

/// One event reduced to validated typed fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BbotEvent {
    pub typ: String,
    pub scope_distance: i32,
    pub host: String,
    pub url: String,
    pub port: i32,
    pub module: String,
    pub module_sequence: String,
    pub source: String,
    /// Free text as one inert quoted string; "" when absent.
    pub quoted: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BbotIngest {
    pub in_scope: Vec<BbotEvent>,
    pub intel: Vec<BbotEvent>,
    pub skipped: i64,
}

/// The insert_assets item shape (only the fields bbot items populate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssetInputItem {
    pub typ: String,
    pub domain: String,
    pub ip: String,
    pub url: String,
    pub port: i32,
}

fn token_re() -> Regex {
    Regex::new(r"^[A-Za-z0-9_.>-]{1,128}$").unwrap()
}
fn host_re() -> Regex {
    Regex::new(r"^[a-z0-9_.-]{1,253}$").unwrap()
}

fn bbot_token(s: &str) -> String {
    if token_re().is_match(s) {
        s.to_string()
    } else {
        String::new()
    }
}

fn is_absent(v: Option<&serde_json::Value>) -> bool {
    v.map(|v| v.is_null()).unwrap_or(true)
}

/// `bbotQuote`: a JSON string is unwrapped and quoted; an object is kept as its
/// compact JSON text, then quoted. `data` wins over `data_json`.
fn bbot_quote(data: Option<&serde_json::Value>, data_json: Option<&serde_json::Value>) -> String {
    let raw = if is_absent(data) { data_json } else { data };
    let Some(raw) = raw else { return String::new() };
    if let Some(s) = raw.as_str() {
        return format!("{s:?}");
    }
    format!("{:?}", raw.to_string())
}

fn bbot_typed(raw: &serde_json::Value) -> BbotEvent {
    let get = |k: &str| raw.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let mut ev = BbotEvent {
        typ: bbot_token(get("type")),
        scope_distance: -1,
        module: bbot_token(get("module")),
        module_sequence: bbot_token(get("module_sequence")),
        quoted: bbot_quote(raw.get("data"), raw.get("data_json")),
        ..BbotEvent::default()
    };
    if let Some(v) = raw.get("scope_distance").and_then(|v| v.as_i64()) {
        ev.scope_distance = v as i32;
    }
    let host = get("host").trim().to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    if host.parse::<std::net::IpAddr>().is_ok() || host_re().is_match(&host) {
        ev.host = host.clone();
    }
    let port = raw.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
    if (1..=65535).contains(&port) {
        ev.port = port as i32;
    }

    let url_raw = get("url").trim();
    if !ev.host.is_empty() && !url_raw.is_empty() {
        if let Ok(mut u) = url::Url::parse(url_raw) {
            if matches!(u.scheme(), "http" | "https")
                && u.host_str()
                    .map(|h| h.eq_ignore_ascii_case(&ev.host))
                    .unwrap_or(false)
            {
                let _ = u.set_username("");
                let _ = u.set_password(None);
                ev.url = u.to_string();
            }
        }
    }
    ev
}

/// `parseBbotJSONL`: parse one output.json (JSON Lines). Pure.
pub fn parse_bbot_jsonl(input: &str) -> Result<BbotIngest, String> {
    let mut out = BbotIngest::default();
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(line) else {
            out.skipped += 1;
            continue;
        };
        if !raw.is_object() {
            out.skipped += 1;
            continue;
        }
        let mut ev = bbot_typed(&raw);
        if raw.get("scope_distance").and_then(|v| v.as_i64()) == Some(0) {
            ev.source = FACT_SOURCE_TOOL_OUTPUT.to_string();
            out.in_scope.push(ev);
        } else {
            ev.source = FACT_SOURCE_THIRD_PARTY_INTEL.to_string();
            out.intel.push(ev);
        }
    }
    Ok(out)
}

/// `assetItems`: only in-scope typed events may feed insert_assets.
impl BbotIngest {
    pub fn asset_items(&self) -> Vec<AssetInputItem> {
        let mut items = Vec::new();
        for ev in &self.in_scope {
            if ev.host.is_empty() {
                continue;
            }
            let ip = ev.host.parse::<std::net::IpAddr>().is_ok();
            match ev.typ.as_str() {
                "DNS_NAME" if !ip => items.push(AssetInputItem {
                    typ: "subdomain".into(),
                    domain: ev.host.clone(),
                    ..Default::default()
                }),
                "IP_ADDRESS" if ip => items.push(AssetInputItem {
                    typ: "ip".into(),
                    ip: ev.host.clone(),
                    ..Default::default()
                }),
                "OPEN_TCP_PORT" if ev.port > 0 => items.push(AssetInputItem {
                    typ: "service".into(),
                    port: ev.port,
                    domain: if ip { String::new() } else { ev.host.clone() },
                    ip: if ip { ev.host.clone() } else { String::new() },
                    ..Default::default()
                }),
                "URL" if !ev.url.is_empty() => items.push(AssetInputItem {
                    typ: "service".into(),
                    url: ev.url.clone(),
                    ..Default::default()
                }),
                _ => {}
            }
        }
        items
    }
}
