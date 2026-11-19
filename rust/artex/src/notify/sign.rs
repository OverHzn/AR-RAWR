//! Rust port of the notify signing functions that live in `notify/dingtalk.go`
//! (`dingTalkSignedURL`) and `notify/feishu.go` (`feishuSign`).
//!
//! The two algorithms are **mirror images** and must never be merged:
//! - DingTalk: `key = secret`, `message = timestamp + "\n" + secret`.
//! - Feishu:   `key = timestamp + "\n" + secret`, `message = ""`.
//!
//! Copying one platform's implementation into the other always fails signature
//! verification (Feishu error 19021), so they stay as two separate functions. The
//! URL query handling reuses the Go-compatible parser/encoder in
//! [`gocompat::neturl`] so the produced address round-trips exactly like Go's
//! `net/url`.

use base64::Engine as _;
use gocompat::neturl::{parse_url, query_escape, unescape, Url, ENC_QUERY_COMPONENT};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::http::redact_request_target;

type HmacSha256 = Hmac<Sha256>;

/// `dingTalkSignedURL` (`notify/dingtalk.go`): append the `timestamp` (ms) and
/// `sign` parameters to the webhook per DingTalk's official signing rule:
/// `string-to-sign = timestamp + "\n" + secret`, HMAC-SHA256 keyed **with the
/// secret itself**, base64-encoded then URL-encoded.
///
/// An empty secret returns the hook unchanged (a bot without signing enabled).
/// `now_millis` is the timestamp in milliseconds (Go `now.UnixMilli()`).
pub fn dingtalk_signed_url(hook: &str, secret: &str, now_millis: i64) -> Result<String, String> {
    if secret.is_empty() {
        return Ok(hook.to_string());
    }
    let ts = now_millis.to_string();

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "failed to initialise signer".to_string())?;
    mac.update(format!("{ts}\n{secret}").as_bytes());
    let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    let mut u: Url = parse_url(hook).map_err(|_| {
        // url.Parse's error text carries the full address (with access_token);
        // never pass it through.
        format!(
            "failed to parse Webhook address: {}",
            redact_request_target(hook)
        )
    })?;

    let mut pairs = parse_query(&u.raw_query);
    set_query(&mut pairs, "timestamp", &ts);
    set_query(&mut pairs, "sign", &sign);
    u.raw_query = encode_query(&pairs);
    Ok(u.to_url_string())
}

/// `feishuSign` (`notify/feishu.go`): `key = timestamp + "\n" + secret`, empty
/// message, HMAC-SHA256, base64. The mirror image of [`dingtalk_signed_url`].
pub fn feishu_sign(timestamp: &str, secret: &str) -> String {
    let string_to_sign = format!("{timestamp}\n{secret}");
    // The HMAC message is empty — Go never calls mac.Write before Sum.
    let mac = HmacSha256::new_from_slice(string_to_sign.as_bytes())
        .expect("HMAC accepts a key of any length");
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Parse a URL query string into ordered key/value pairs (Go `url.ParseQuery`
/// subset): split on `&`, then each pair on the first `=`; keys/values are
/// unescaped with query-component rules (`+` → space). Empty pairs are skipped.
fn parse_query(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if raw.is_empty() {
        return out;
    }
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = unescape(k, ENC_QUERY_COMPONENT).unwrap_or_else(|_| k.to_string());
        let val = unescape(v, ENC_QUERY_COMPONENT).unwrap_or_else(|_| v.to_string());
        out.push((key, val));
    }
    out
}

/// `url.Values.Set`: remove any existing values for `key`, then append one.
fn set_query(pairs: &mut Vec<(String, String)>, key: &str, val: &str) {
    pairs.retain(|(k, _)| k != key);
    pairs.push((key.to_string(), val.to_string()));
}

/// `url.Values.Encode`: sort keys alphabetically; for each key, emit its values
/// in slice order as `key=value` pairs joined by `&`, each query-escaped.
fn encode_query(pairs: &[(String, String)]) -> String {
    let mut order: Vec<&str> = Vec::new();
    for (k, _) in pairs {
        if !order.contains(&k.as_str()) {
            order.push(k);
        }
    }
    order.sort_unstable();
    let mut buf = String::new();
    for k in order {
        for (pk, pv) in pairs {
            if pk == k {
                if !buf.is_empty() {
                    buf.push('&');
                }
                buf.push_str(&query_escape(pk));
                buf.push('=');
                buf.push_str(&query_escape(pv));
            }
        }
    }
    buf
}
