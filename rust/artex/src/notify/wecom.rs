//! WeCom (WeChat Work) group-bot adapter (WECOM) — port of `notify/wecom.go`.
//!
//! Platform facts mirrored here:
//! - Authenticates only via the key on the URL; signing is unsupported, so the
//!   webhook address itself is the entire credential.
//! - The markdown `content` is capped at 4096 **bytes**; a longer body is packed
//!   down item by item (see [`markdown_body`]) rather than rejected outright, so a
//!   big digest still delivers its first few items.
//! - Rate limit is 20/min, held up client-side; `errcode 45009` (API freq out of
//!   limit) is a rolling window and thus retryable, everything else permanent.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::http::{do_json, validate_http_url};
use super::markdown::markdown_body;
use super::{cfg_string, snippet, Channel, Context, Error, Message, KIND_WECOM};

/// The hard byte limit (not characters) of WeCom group-bot markdown content — the
/// tightest limit of all six channels.
const WECOM_MARKDOWN_LIMIT: i64 = 4096;

/// `weComChannel`: the WeCom group-bot adapter.
pub struct WeComChannel;

#[derive(Debug, Default, Deserialize)]
struct WeComResponse {
    #[serde(default)]
    errcode: i64,
    #[serde(default)]
    errmsg: String,
}

#[async_trait]
impl Channel for WeComChannel {
    fn kind(&self) -> &'static str {
        KIND_WECOM
    }

    fn validate(&self, cfg: &Value) -> Result<(), Error> {
        let hook = cfg_string(cfg, "webhook");
        if hook.is_empty() {
            return Err(Error::msg("missing Webhook address"));
        }
        validate_http_url(&hook).map_err(|e| Error::msg(format!("invalid Webhook address: {e}")))
    }

    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error> {
        self.validate(cfg)
            .map_err(|e| Error::permanent(e.message()))?;

        // Pack whole items to fit 4096 bytes; `kept` is how many actually fit.
        let (content, kept) = markdown_body(m, WECOM_MARKDOWN_LIMIT);
        let payload = json!({
            "msgtype": "markdown",
            "markdown": { "content": content },
        });

        let hook = cfg_string(cfg, "webhook");
        let raw = do_json(ctx, "POST", &hook, &[], Some(&payload)).await?;

        let res: WeComResponse = serde_json::from_slice(&raw).map_err(|e| {
            Error::permanent(format!(
                "failed to parse WeCom response: {e} ({})",
                snippet(&String::from_utf8_lossy(&raw))
            ))
        })?;
        if res.errcode != 0 {
            // 45009 = the API rate window rolled: retrying after backoff works, so
            // it is a plain (transient) error. Anything else is permanent.
            if res.errcode == 45009 {
                return Err(Error::msg(format!(
                    "WeCom rate limited {}: {}",
                    res.errcode, res.errmsg
                )));
            }
            return Err(Error::permanent(format!(
                "WeCom returned error {}: {}",
                res.errcode, res.errmsg
            )));
        }
        Ok(kept)
    }
}
