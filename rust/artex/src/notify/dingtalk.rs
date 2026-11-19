//! DingTalk custom-robot adapter (DINGTALK) — port of `notify/dingtalk.go`.
//!
//! Stateless: all credentials arrive via `cfg`, the struct carries nothing (it is
//! the zero-sized Go registry entry).
//!
//! Platform facts mirrored here:
//! - The webhook is itself the credential; `secret` (when set) only triggers an
//!   HMAC-SHA256 `timestamp`/`sign` appended to the URL (built by
//!   [`dingtalk_signed_url`]), so it must never be echoed back.
//! - Success and failure both return HTTP 200, distinguished by `errcode` in the
//!   body — not checking it would record failed deliveries as successful.
//! - Delivery failures must never contain the credential: [`do_json`] prints only
//!   `host:port` for transport errors and the parse error carries only a snippet.

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::http::{do_json, validate_http_url};
use super::markdown::{markdown_body, markdown_title};
use super::sign::dingtalk_signed_url;
use super::{cfg_string, snippet, Channel, Context, Error, Message, KIND_DINGTALK};

/// DingTalk markdown bodies have no explicit byte limit; this defensive cap keeps
/// evidence fields from ballooning abnormally (Go `markdownBody(m, 20000)`).
const DINGTALK_MARKDOWN_LIMIT: i64 = 20000;

/// `dingtalkChannel`: the DingTalk custom-robot adapter.
pub struct DingTalkChannel;

#[derive(Debug, Default, Deserialize)]
struct DingTalkResponse {
    #[serde(default)]
    errcode: i64,
    #[serde(default)]
    errmsg: String,
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn kind(&self) -> &'static str {
        KIND_DINGTALK
    }

    fn validate(&self, cfg: &Value) -> Result<(), Error> {
        let hook = cfg_string(cfg, "webhook");
        if hook.is_empty() {
            return Err(Error::msg("missing Webhook address"));
        }
        validate_http_url(&hook).map_err(|e| Error::msg(format!("invalid Webhook address: {e}")))
    }

    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error> {
        // Go: `if err := c.Validate(cfg); err != nil { return 0, Permanent(err) }`.
        self.validate(cfg)
            .map_err(|e| Error::permanent(e.message()))?;

        let hook = cfg_string(cfg, "webhook");
        let secret = cfg_string(cfg, "secret");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // An empty secret returns the hook unchanged (a bot without signing).
        let endpoint = dingtalk_signed_url(&hook, &secret, now).map_err(Error::permanent)?;

        let title = markdown_title(m);
        let (text, kept) = markdown_body(m, DINGTALK_MARKDOWN_LIMIT);

        // ActionCard (with a detail button) for a single item that has a backlink;
        // markdown otherwise (a batch never uses the single-item card).
        let payload = if !m.batch && m.items.len() == 1 && !m.items[0].detail_url.is_empty() {
            let detail_url = m.items[0].detail_url.clone();
            json!({
                "msgtype": "actionCard",
                "actionCard": {
                    "title": title,
                    "text": text,
                    "btnOrientation": "0",
                    "singleTitle": "View details",
                    "singleURL": detail_url,
                },
            })
        } else {
            json!({
                "msgtype": "markdown",
                "markdown": { "title": title, "text": text },
            })
        };

        // `doJSON` errors (transport, 5xx, 4xx) propagate verbatim, as in Go.
        let raw = do_json(ctx, "POST", &endpoint, &[], Some(&payload)).await?;

        // DingTalk hides business errors inside a 200 body.
        let res: DingTalkResponse = serde_json::from_slice(&raw).map_err(|e| {
            Error::permanent(format!(
                "failed to parse DingTalk response: {e} ({})",
                snippet(&String::from_utf8_lossy(&raw))
            ))
        })?;
        if res.errcode != 0 {
            // 310000 / 301000 are configuration errors a retry cannot fix.
            return Err(Error::permanent(format!(
                "DingTalk returned error {}: {}",
                res.errcode, res.errmsg
            )));
        }
        Ok(kept)
    }
}
