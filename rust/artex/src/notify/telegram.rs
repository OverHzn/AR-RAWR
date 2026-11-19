//! Telegram Bot API adapter (TELEGRAM) — port of `notify/telegram.go`.
//!
//! Platform facts mirrored here:
//! - Authentication lives entirely in the URL path (`/bot<token>/sendMessage`);
//!   no signing. The Bot Token is the whole credential; `base_url` only decides
//!   which endpoint it is sent to.
//! - Uses HTML parse mode (escape `& < >`) rather than MarkdownV2, whose 18
//!   metacharacters are easy to under-escape.
//! - Business errors hide inside an HTTP 200 body, detected via the `ok` field;
//!   `429` is a retryable rate limit, the rest are permanent configuration errors.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::http::{do_json, redact_request_target, validate_http_url};
use super::markdown::{markdown_title, pack_item_count};
use super::{
    asset_line, cfg_string, one_line, rune_size, severity_label, snippet, status_label,
    telegram_escape, telegram_escape_attr, truncate_html, Channel, Context, Error, Item, Limits,
    Message, KIND_TELEGRAM,
};

/// The `text` field limit (in characters) of Telegram `sendMessage`.
const TELEGRAM_TEXT_LIMIT: i64 = 4096;
/// Reserved for the title line and a possible truncation notice (in characters).
const TELEGRAM_RESERVED_RUNES: i64 = 160;

/// `telegramChannel`: the Telegram Bot API adapter.
pub struct TelegramChannel;

#[derive(Debug, Default, Deserialize)]
struct TelegramResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    description: String,
}

#[async_trait]
impl Channel for TelegramChannel {
    fn kind(&self) -> &'static str {
        KIND_TELEGRAM
    }

    fn validate(&self, cfg: &Value) -> Result<(), Error> {
        if cfg_string(cfg, "bot_token").is_empty() {
            return Err(Error::msg("missing Bot Token"));
        }
        if cfg_string(cfg, "chat_id").is_empty() {
            return Err(Error::msg("missing Chat ID"));
        }
        let base = cfg_string(cfg, "base_url");
        if !base.is_empty() {
            validate_http_url(&base).map_err(|e| Error::msg(format!("invalid API URL: {e}")))?;
        }
        Ok(())
    }

    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error> {
        self.validate(cfg)
            .map_err(|e| Error::permanent(e.message()))?;
        let endpoint = telegram_endpoint(cfg).map_err(Error::permanent)?;

        let (text, kept) = telegram_html(m);
        let payload = json!({
            "chat_id": cfg_string(cfg, "chat_id"),
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": false,
        });

        let raw = do_json(ctx, "POST", &endpoint, &[], Some(&payload)).await?;

        let res: TelegramResponse = serde_json::from_slice(&raw).map_err(|e| {
            Error::permanent(format!(
                "failed to parse Telegram response: {e} ({})",
                snippet(&String::from_utf8_lossy(&raw))
            ))
        })?;
        if res.ok {
            return Ok(kept);
        }
        // 429 is rate limiting — retrying after backoff works. The rest (400/401/
        // 403/404) are configuration problems a retry cannot fix.
        if res.error_code == 429 {
            return Err(Error::msg(format!(
                "Telegram rate limit: {}",
                res.description
            )));
        }
        Err(Error::permanent(format!(
            "Telegram returned error {}: {}",
            res.error_code, res.description
        )))
    }
}

/// `telegramEndpoint`: build the `sendMessage` URL. An empty `base_url` uses the
/// official API; a non-empty one targets a self-hosted Bot API reverse proxy.
fn telegram_endpoint(cfg: &Value) -> Result<String, String> {
    let mut base = cfg_string(cfg, "base_url");
    if base.is_empty() {
        base = "https://api.telegram.org".to_string();
    }
    // TrimSuffix(base, "/").
    if let Some(stripped) = base.strip_suffix('/') {
        base = stripped.to_string();
    }
    let token = cfg_string(cfg, "bot_token");
    let raw = format!("{base}/bot{token}/sendMessage");
    match gocompat::neturl::parse_url(&raw) {
        Ok(u) => Ok(u.to_url_string()),
        Err(_) => Err(format!(
            "failed to build API URL (base URL: {})",
            redact_request_target(&base)
        )),
    }
}

/// `telegramHTML`: render the HTML body and return it with the number of items
/// actually written (`kept`, see [`Channel::send`]).
pub fn telegram_html(m: &Message) -> (String, usize) {
    let lim = m.limits();
    let mut b = String::new();
    b.push_str(&format!("<b>{}</b>\n", telegram_escape(&markdown_title(m))));
    if m.batch {
        // Telegram's limit is in characters, so packing is measured in runes.
        let footer = if m.home_url.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n<a href=\"{}\">View all on the platform</a>",
                telegram_escape_attr(&m.home_url)
            )
        };
        let kept = pack_item_count(
            &m.items,
            TELEGRAM_TEXT_LIMIT,
            TELEGRAM_RESERVED_RUNES,
            &footer,
            rune_size,
            |it, idx| telegram_batch_line(it, idx + 1, lim),
        );
        let items = &m.items[..kept];
        b.clear();
        b.push_str(&format!(
            "<b>{}</b>",
            telegram_escape(&telegram_batch_title(m, items, m.items.len()))
        ));
        for (i, it) in items.iter().enumerate() {
            b.push('\n');
            b.push_str(&telegram_escape(&telegram_batch_line(it, i + 1, lim)));
        }
        b.push_str(&footer);
        return (
            truncate_html(b, TELEGRAM_TEXT_LIMIT).unwrap_or_default(),
            kept,
        );
    }
    if m.items.is_empty() {
        return (b, 0);
    }
    let it = &m.items[0];
    if it.is_status_change() {
        let from = status_label(it.from_status.clone()).unwrap_or_else(|e| e);
        let to = status_label(it.to_status.clone()).unwrap_or_else(|e| e);
        b.push_str(&format!(
            "\n<b>Status change</b>: {} \u{2192} {}",
            telegram_escape(&from),
            telegram_escape(&to)
        ));
    }
    if !it.vulnclass.is_empty() && it.vulnclass != it.title() {
        b.push_str(&format!(
            "\n<b>Type</b>: {}",
            telegram_escape(&it.vulnclass)
        ));
    }
    let a = asset_line(&it.assets, lim.assets_shown);
    if !a.is_empty() {
        b.push_str(&format!("\n<b>Assets</b>: {}", telegram_escape(&a)));
    }
    let s = one_line(it.summary.clone(), lim.summary_runes).unwrap_or_default();
    if !s.is_empty() {
        b.push_str(&format!("\n<b>Summary</b>: {}", telegram_escape(&s)));
    }
    if !it.detail_url.is_empty() {
        b.push_str(&format!(
            "\n\n<a href=\"{}\">View details</a>",
            telegram_escape_attr(&it.detail_url)
        ));
    }
    (truncate_html(b, TELEGRAM_TEXT_LIMIT).unwrap_or_default(), 1)
}

/// `telegramBatchLine`: one digest entry, unescaped (the caller escapes it).
fn telegram_batch_line(it: &Item, idx: usize, lim: Limits) -> String {
    let sev = severity_label(it.severity.clone()).unwrap_or_else(|e| e);
    let a = asset_line(&it.assets, lim.assets_shown);
    if !a.is_empty() {
        format!("{idx}. {sev} \u{00B7} {} \u{2014} {a}", it.title())
    } else {
        format!("{idx}. {sev} \u{00B7} {}", it.title())
    }
}

/// `telegramBatchTitle`: the digest title line. `items` is what this message
/// actually includes; `total` is the whole batch.
fn telegram_batch_title(m: &Message, items: &[Item], total: usize) -> String {
    let mut title = format!("Vulnerability digest \u{00B7} {total} total");
    let extra = total as i64 - items.len() as i64;
    if extra > 0 {
        title.push_str(&format!(
            " (showing the first {}, {} more in the next message)",
            items.len(),
            extra
        ));
    }
    if m.window_minutes > 0 {
        title = format!("Last {} min \u{00B7} {}", m.window_minutes, title);
    }
    title
}
