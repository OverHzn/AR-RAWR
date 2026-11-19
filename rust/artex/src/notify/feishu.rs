//! Feishu (incl. Lark) custom-bot adapter (FEISHU) — port of `notify/feishu.go`.
//!
//! Uses interactive cards. Platform facts mirrored here:
//! - The signing algorithm is the **mirror image** of DingTalk's (see
//!   [`feishu_sign`]); copying one into the other always fails verification (19021).
//! - Business errors hide inside an HTTP 200 body (`code != 0`); some hook versions
//!   use the `StatusCode`/`StatusMessage` field names instead, so both are checked.
//! - Card headers carry a colour template per severity so severity is visible at a
//!   glance in the message list.
//!
//! ## Byte-exact packing
//! [`feishu_card`] packs whole items against the request body **as it will be
//! serialized**, not against the line text alone, and measures with the very same
//! serializer ([`serde_json`]) that [`do_json`] sends with — so the conservative
//! envelope constant ([`FEISHU_ENVELOPE_BYTES`]) and the final on-wire size stay
//! consistent (no item pushes the body past [`FEISHU_MAX_CARD_BYTES`]).

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::http::{do_json, validate_http_url};
use super::markdown::{markdown_batch_intro, markdown_title};
use super::sign::feishu_sign;
use super::{
    asset_line, cfg_string, feishu_severity_template, markdown_text, severity_label, snippet,
    status_label, truncate_bytes, Channel, Context, Error, Item, Limits, Message, KIND_FEISHU,
    MARKDOWN_RESERVED_BYTES,
};

/// Conservative cap on the card content. Feishu rejects a request body over 20 KB
/// as a whole; this stays clearly below it to cover the JSON wrapping overhead.
const FEISHU_MAX_CARD_BYTES: i64 = 16000;
/// Bytes [`Channel::send`] adds around the card when signing:
/// `{"card":…,"msg_type":"interactive","sign":"<44>","timestamp":"<10>"}`.
const FEISHU_ENVELOPE_BYTES: i64 = 113;

/// `feishuChannel`: the Feishu/Lark custom-bot adapter.
pub struct FeishuChannel;

#[derive(Debug, Default, Deserialize)]
struct FeishuResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    msg: String,
    #[serde(rename = "StatusCode", default)]
    status_code: i64,
    #[serde(rename = "StatusMessage", default)]
    status_message: String,
}

#[async_trait]
impl Channel for FeishuChannel {
    fn kind(&self) -> &'static str {
        KIND_FEISHU
    }

    fn validate(&self, cfg: &Value) -> Result<(), Error> {
        let hook = cfg_string(cfg, "webhook");
        if hook.is_empty() {
            return Err(Error::msg("missing Webhook URL"));
        }
        validate_http_url(&hook).map_err(|e| Error::msg(format!("invalid webhook URL: {e}")))
    }

    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error> {
        self.validate(cfg)
            .map_err(|e| Error::permanent(e.message()))?;

        let (card, kept) = feishu_card(m);
        let mut payload = json!({ "msg_type": "interactive", "card": card });
        // Signing parameters sit at the same level as the message and appear only
        // when a secret is configured.
        let secret = cfg_string(cfg, "secret");
        if !secret.is_empty() {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .to_string();
            payload["sign"] = json!(feishu_sign(&ts, &secret));
            payload["timestamp"] = json!(ts);
        }

        let hook = cfg_string(cfg, "webhook");
        let raw = do_json(ctx, "POST", &hook, &[], Some(&payload)).await?;

        let res: FeishuResponse = serde_json::from_slice(&raw).map_err(|e| {
            Error::permanent(format!(
                "failed to parse Feishu response: {e} ({})",
                snippet(&String::from_utf8_lossy(&raw))
            ))
        })?;
        if res.code != 0 {
            return Err(Error::permanent(format!(
                "Feishu returned error {}: {}",
                res.code, res.msg
            )));
        }
        if res.status_code != 0 {
            return Err(Error::permanent(format!(
                "Feishu returned error {}: {}",
                res.status_code, res.status_message
            )));
        }
        Ok(kept)
    }
}

/// `feishuCard`: build the interactive card and return it with the number of items
/// actually written. `kept` marks only the delivered items.
pub fn feishu_card(m: &Message) -> (Value, usize) {
    let lim = m.limits();
    let budget = FEISHU_MAX_CARD_BYTES - MARKDOWN_RESERVED_BYTES;

    let mut card = json!({
        "config": { "wide_screen_mode": true },
        "header": { "title": { "tag": "plain_text", "content": markdown_title(m) } },
    });
    if !m.items.is_empty() {
        card["header"]["template"] = json!(feishu_severity_template(&m.items[0].severity));
    }

    let elements: Vec<Value>;
    let kept: usize;
    if m.batch {
        // Pack whole items against the request body as it will be serialized. Each
        // line is its own element; the intro and platform link are measured too.
        // The first item is always kept so one oversized item cannot jam the batch.
        let mut lines: Vec<Value> = Vec::new();
        let mut used: i64 = 0;
        for (i, it) in m.items.iter().enumerate() {
            let line = feishu_markdown_div(
                truncate_bytes(feishu_batch_line(it, i + 1, lim), budget).unwrap_or_default(),
            );
            used += feishu_json_size(&line) + 1; // the element and its separating comma
            card["elements"] = Value::Array(digest_elements(m, i + 1, &[]));
            if i > 0
                && FEISHU_ENVELOPE_BYTES + feishu_json_size(&card) + used > FEISHU_MAX_CARD_BYTES
            {
                break;
            }
            lines.push(line);
        }
        kept = lines.len();
        elements = digest_elements(m, kept, &lines);
    } else if !m.items.is_empty() {
        kept = 1;
        let it = &m.items[0];
        let mut el: Vec<Value> = vec![feishu_markdown_div(
            truncate_bytes(feishu_item_lines(it, lim), budget).unwrap_or_default(),
        )];
        if !it.detail_url.is_empty() {
            el.push(feishu_button("View details", &it.detail_url));
        }
        elements = el;
    } else {
        kept = 0;
        elements = Vec::new();
    }

    card["elements"] = Value::Array(elements);
    (card, kept)
}

/// The card elements for a digest of `n` items plus the given `lines`, with the
/// "View all" button appended when a home URL is set (Go `digest` closure).
fn digest_elements(m: &Message, n: usize, lines: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![feishu_markdown_div(markdown_batch_intro(
        m,
        &m.items[..n],
        m.items.len(),
    ))];
    out.extend_from_slice(lines);
    if !m.home_url.is_empty() {
        out.push(feishu_button("View all on the platform", &m.home_url));
    }
    out
}

/// `feishuJSONSize`: the number of bytes `v` takes in the request body. Uses the
/// same serializer [`do_json`] sends with, so measurement and delivery agree.
fn feishu_json_size(v: &Value) -> i64 {
    serde_json::to_vec(v).map(|b| b.len() as i64).unwrap_or(0)
}

fn feishu_markdown_div(content: String) -> Value {
    json!({ "tag": "div", "text": { "tag": "lark_md", "content": content } })
}

fn feishu_button(label: &str, url: &str) -> Value {
    json!({
        "tag": "action",
        "actions": [{
            "tag": "button",
            "text": { "tag": "lark_md", "content": label },
            "url": url,
            "type": "primary",
        }],
    })
}

/// `feishuItemLines`: the `lark_md` body of a single vulnerability. All outside
/// content goes through [`markdown_text`] so a title cannot become a clickable
/// external link inside Feishu.
fn feishu_item_lines(it: &Item, lim: Limits) -> String {
    let sev = severity_label(it.severity.clone()).unwrap_or_else(|e| e);
    let mut out = format!("**{sev} \u{00B7} {}**", markdown_text(it.title(), 0));
    if it.is_status_change() {
        let from = status_label(it.from_status.clone()).unwrap_or_else(|e| e);
        let to = status_label(it.to_status.clone()).unwrap_or_else(|e| e);
        out.push_str(&format!(
            "\n**Status change**: {} \u{2192} {}",
            markdown_text(&from, 0),
            markdown_text(&to, 0)
        ));
    }
    if !it.vulnclass.is_empty() && it.vulnclass != it.title() {
        out.push_str(&format!("\n**Type**: {}", markdown_text(&it.vulnclass, 0)));
    }
    let a = asset_line(&it.assets, lim.assets_shown);
    if !a.is_empty() {
        out.push_str(&format!("\n**Assets**: {}", markdown_text(&a, 0)));
    }
    if !it.summary.is_empty() {
        let s = markdown_text(&it.summary, lim.summary_runes);
        if !s.is_empty() {
            out.push_str(&format!("\n**Summary**: {s}"));
        }
    }
    out
}

/// `feishuBatchLine`: one entry of a digest card.
fn feishu_batch_line(it: &Item, index: usize, lim: Limits) -> String {
    let sev = severity_label(it.severity.clone()).unwrap_or_else(|e| e);
    let mut line = format!(
        "**{index}. {sev} \u{00B7} {}**",
        markdown_text(it.title(), 0)
    );
    let a = asset_line(&it.assets, lim.assets_shown);
    if !a.is_empty() {
        line.push_str(&format!(" \u{2014} {}", markdown_text(&a, 0)));
    }
    line
}
