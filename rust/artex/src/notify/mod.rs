//! Rust port of the Go `notify/` package — IM / email push channel adapters for
//! vulnerability findings.
//!
//! `notify/` is a **leaf package**: it depends only on the standard library (plus,
//! here, the pure parity helpers), knows neither the database nor the server.
//! Channel configuration arrives as a decoded JSON value (the
//! `notification_channels.config` JSONB column, a `map[string]any` in Go), and the
//! content to push arrives as a [`Message`]. Splitting it this way keeps the
//! error-prone, security-relevant parts — signature computation, UTF-8 truncation,
//! filter matching, config masking — unit-testable without a database.
//!
//! ## Reuse, not re-port
//! The pure, deterministic helpers are already proven against the real Go via the
//! oracle-parity crates, and are **re-exported** here rather than re-implemented:
//! - [`notify_rs::port`] — channel registry (`get`/`valid_kind`/`kinds`), severity
//!   (`severity_rank`/`severity_label`/`at_least`), status labels, truncation
//!   (`truncate_bytes`/`truncate_runes`/`one_line`/`truncate_html`), config masking
//!   primitives (`masked_value`/`is_masked`), and filter parse/match.
//! - [`notifyx_rs::port`] — the unexported config readers (`cfg_*`), escape helpers
//!   (`html_escape`/`markdown_escape`/…), `asset_line`, severity colours, Feishu
//!   template, SMTP reply-code parse, and `is_blank_config_value`/`same_config_value`.
//!
//! This module adds the pieces that are not pure data: the [`Channel`] adapter
//! trait, the permanent/transient [`Error`] classification, the [`Context`]
//! cancellation surface, and the [`Snapshot`]/[`Item`]/[`Message`]/[`Limits`] types.
//! Concrete channel implementations (Telegram, email, Feishu, DingTalk, WeCom,
//! generic webhook) and the SSRF-guarded HTTP client land in later units on top of
//! this core.

pub use notify_rs::port::{
    at_least, get, is_masked, kinds, masked_value, one_line, parse_filter, r#match, severity_label,
    severity_rank, status_label, truncate_bytes, truncate_html, truncate_runes, valid_kind,
    valid_min_severity,
};

pub use notifyx_rs::port::{
    asset_line, byte_size, cfg_bool, cfg_int, cfg_map, cfg_string, cfg_strings, contains_any_fold,
    feishu_severity_template, html_escape, html_escape_attr, html_severity_color, intersects_int,
    is_blank_config_value, markdown_escape, markdown_text, rune_size, same_config_value,
    smtp_reply_code, snippet, telegram_escape, telegram_escape_attr,
};

pub mod dingtalk;
pub mod feishu;
pub mod html;
pub mod http;
pub mod markdown;
pub mod mask;
pub mod sign;
pub mod telegram;
pub mod wecom;

use async_trait::async_trait;
use serde_json::Value;

use crate::llm::Error as LlmError;

// === Kind / event identifiers (notify.go) ===

pub const KIND_DINGTALK: &str = "dingtalk";
pub const KIND_FEISHU: &str = "feishu";
pub const KIND_WECOM: &str = "wecom";
pub const KIND_WEBHOOK: &str = "webhook";
pub const KIND_TELEGRAM: &str = "telegram";
pub const KIND_EMAIL: &str = "email";

pub const EVENT_FINDING_CREATED: &str = "finding_created";
pub const EVENT_FINDING_STATUS_CHANGED: &str = "finding_status_changed";

/// `InitKind` (`notify.go:38`): the fallback kind for an empty config kind.
pub const INIT_KIND: &str = KIND_DINGTALK;

/// `MaskedPrefix` (`mask.go:17`): the marker prefix of a masked value. When the API
/// echoes a credential it replaces the real content with a value carrying this
/// prefix; on update, a value carrying it means "keep the stored original".
pub const MASKED_PREFIX: &str = "__masked__";

// === Render defaults (markdown.go) ===

/// `DefaultMaxAssetsShown`: default max assets listed per vulnerability.
pub const DEFAULT_MAX_ASSETS_SHOWN: i64 = 3;
/// `DefaultMaxSummaryRunes`: default max runes of a single push's summary.
pub const DEFAULT_MAX_SUMMARY_RUNES: i64 = 120;

/// `markdownReservedBytes` (`markdown.go:28`): bytes reserved for a digest's header
/// (count line + severity distribution + truncation notice) and footer (platform
/// link), deducted from the packing budget so head and tail are never cut off.
pub const MARKDOWN_RESERVED_BYTES: i64 = 320;

// === Context (Go context.Context) ===

/// Cancellation surface for a channel send (Go `context.Context`). Minimal here —
/// `Err()` is `Some` once cancelled — mirroring [`crate::llm::Context`]. The server
/// wires a real cancellation source (tokio) when notify is driven.
#[derive(Debug, Clone, Default)]
pub struct Context {
    err: Option<LlmError>,
}

impl Context {
    /// `context.Background()`: never cancelled.
    pub fn background() -> Self {
        Context::default()
    }

    /// A context already cancelled with `context.Canceled`.
    pub fn canceled() -> Self {
        Context {
            err: Some(LlmError::Canceled),
        }
    }

    /// A context already past its deadline.
    pub fn deadline_exceeded() -> Self {
        Context {
            err: Some(LlmError::DeadlineExceeded),
        }
    }

    /// `ctx.Err()`.
    pub fn err(&self) -> Option<&LlmError> {
        self.err.as_ref()
    }

    /// Whether this context is cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.err.is_some()
    }
}

// === Error classification (channel.go) ===

/// A channel delivery error, classified as permanent (do not retry) or transient
/// (retryable). Mirrors Go's `*PermanentError` sentinel + plain errors: a plain
/// error is transient, [`Error::Permanent`] is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A permanent failure: bad credentials, destination refusal, invalid request
    /// body. Retrying never succeeds. (Go `Permanent(err)`.)
    Permanent(String),
    /// A transient failure: a network blip, rate limit, or peer 5xx worth retrying.
    Transient(String),
    /// Any other delivery error, treated as transient (Go's default for a plain
    /// `error`).
    Msg(String),
}

impl Error {
    /// `Permanent(err)`: mark a message as a permanent failure.
    pub fn permanent(s: impl Into<String>) -> Self {
        Error::Permanent(s.into())
    }

    /// A transient delivery error.
    pub fn transient(s: impl Into<String>) -> Self {
        Error::Transient(s.into())
    }

    /// A free-form delivery error (transient by default, like Go's plain `error`).
    pub fn msg(s: impl Into<String>) -> Self {
        Error::Msg(s.into())
    }

    /// `err.Error()`.
    pub fn message(&self) -> String {
        match self {
            Error::Permanent(s) | Error::Transient(s) | Error::Msg(s) => s.clone(),
        }
    }

    /// `IsPermanent(err)`: whether this is a permanent (non-retryable) failure.
    pub fn is_permanent(&self) -> bool {
        matches!(self, Error::Permanent(_))
    }

    /// `errors.Is(err, context.Canceled)`.
    pub fn is_canceled(&self) -> bool {
        matches!(self, Error::Msg(s) if s == "context canceled")
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for Error {}

/// `Permanent(err)`: wrap a message as permanent. An empty message yields a
/// transient [`Error::Msg("")`] only if you pass one; this always returns
/// [`Error::Permanent`]. (Go's `Permanent(nil) == nil` is modelled by the caller
/// choosing not to wrap a `None`.)
pub fn permanent(s: impl Into<String>) -> Error {
    Error::permanent(s)
}

/// `IsPermanent(err)`.
pub fn is_permanent(err: &Error) -> bool {
    err.is_permanent()
}

// === Payload types (event.go) ===

/// `notify.Snapshot`: the contract for the `notification_events.snapshot` JSONB
/// column. The writer is the vulnerability persistence transaction; the readers are
/// the delivery engine and filter matching. A vulnerability can be renamed or
/// re-severitied after the fact, but a pushed message must reflect the conclusion
/// **at the moment of the event** — so the fields are stored redundantly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// `finding_created` / `finding_status_changed`.
    pub kind: String,
    pub finding_id: i64,
    pub task_id: i64,
    pub vulnclass: String,
    pub name: String,
    pub severity: String,
    pub summary: String,
    pub asset_ids: Vec<i64>,
    /// Non-empty only when `kind = finding_status_changed`.
    pub from_status: String,
    pub to_status: String,
}

/// `notify.Item`: one vulnerability to push, rendered by channels.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub finding_id: i64,
    pub name: String,
    pub vulnclass: String,
    pub severity: String,
    pub summary: String,
    /// Resolved display names of the assets (e.g. domain/IP). Filled by the server
    /// layer — this package never touches the database.
    pub assets: Vec<String>,
    /// Backlink to the vulnerability details; empty means no link is included.
    pub detail_url: String,
    /// Only for status-change events; when both are non-empty, rendered as
    /// "Pending → Fixed".
    pub from_status: String,
    pub to_status: String,
}

impl Item {
    /// `Item.IsStatusChange`: whether this is a status-change event.
    pub fn is_status_change(&self) -> bool {
        !self.from_status.is_empty() || !self.to_status.is_empty()
    }

    /// `Item.Title`: the display title — prefer the human name, fall back to the
    /// vulnclass, and use a placeholder when both are empty (never empty).
    pub fn title(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else if !self.vulnclass.is_empty() {
            &self.vulnclass
        } else {
            "(unnamed vulnerability)"
        }
    }
}

/// `notify.Limits`: the render-time truncation limits of a message. In both fields
/// `0` means no truncation; each channel's own length cap remains the final guard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// How many characters a single push's summary is compressed to.
    pub summary_runes: i64,
    /// Max assets listed per vulnerability.
    pub assets_shown: i64,
}

/// `notify.Message`: the complete content of one channel send.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    /// `1` for a single push; a whole batch for a digest push. Never empty — callers
    /// guarantee at least one item.
    pub items: Vec<Item>,
    /// `true` renders as a digest message (different title, with a time window and
    /// item count).
    pub batch: bool,
    /// The digest period in minutes (only with `batch = true`). Passed in explicitly
    /// rather than computed at render time, so rendering stays deterministic.
    pub window_minutes: i64,
    /// The platform dashboard address; empty means no dashboard entry.
    pub home_url: String,
    /// The truncation limits to render with; `None` means the defaults. An
    /// `Option` because the zero value (`Some(0,0)`) is meaningful (no truncation).
    pub limits: Option<Limits>,
}

impl Message {
    /// `Message.limits()`: the limits to render with, falling back to the defaults.
    pub fn limits(&self) -> Limits {
        self.limits.unwrap_or(Limits {
            summary_runes: DEFAULT_MAX_SUMMARY_RUNES,
            assets_shown: DEFAULT_MAX_ASSETS_SHOWN,
        })
    }
}

// === Per-channel metadata (SecretKeys / DestinationKeys / DefaultRatePerMin) ===
//
// The single source of truth for each channel's credential keys, destination keys,
// and default rate limit — mirroring the Go channel methods of the same name. The
// config-masking logic (`crate::notify::mask`) keys off these, and the concrete
// [`Channel`] impls return them. An unknown kind yields `None` (Go `Get`'s ok=false).

/// `(secret_keys, destination_keys, default_rate_per_min)` for a registered kind.
pub fn channel_meta(kind: &str) -> Option<(&'static [&'static str], &'static [&'static str], i64)> {
    match kind {
        KIND_DINGTALK => Some((&["webhook", "secret"], &["webhook"], 20)),
        KIND_FEISHU => Some((&["webhook", "secret"], &["webhook"], 100)),
        KIND_WECOM => Some((&["webhook"], &["webhook"], 20)),
        KIND_WEBHOOK => Some((&["url", "headers"], &["url"], 0)),
        KIND_TELEGRAM => Some((&["bot_token"], &["base_url"], 20)),
        KIND_EMAIL => Some((&["password"], &["host", "port", "tls"], 60)),
        _ => None,
    }
}

/// `Channel.SecretKeys` for a kind (`None` if unknown).
pub fn secret_keys(kind: &str) -> Option<&'static [&'static str]> {
    channel_meta(kind).map(|(s, _, _)| s)
}

/// `Channel.DestinationKeys` for a kind (`None` if unknown).
pub fn destination_keys(kind: &str) -> Option<&'static [&'static str]> {
    channel_meta(kind).map(|(_, d, _)| d)
}

/// `Channel.DefaultRatePerMin` for a kind (`0` if unknown).
pub fn default_rate_per_min(kind: &str) -> i64 {
    channel_meta(kind).map_or(0, |(_, _, r)| r)
}

// === Channel adapter (channel.go) ===

/// `notify.Channel`: the adapter for one notification channel. Implementations must
/// be **stateless**: the same instance is shared concurrently by multiple channel
/// configurations, and credentials always arrive via `cfg`.
#[async_trait]
pub trait Channel: Send + Sync {
    /// `Kind`: the channel type identifier; must match the registry key.
    fn kind(&self) -> &'static str;

    /// `Validate`: called when saving a configuration; checks required fields and
    /// formats. The error message is shown to the configurator, so it says which
    /// field is missing.
    fn validate(&self, cfg: &Value) -> Result<(), Error>;

    /// `Send`: deliver one message, returning the **number of items actually
    /// delivered** plus an error. A truncated batch returns `kept < items.len()` so
    /// the caller marks only the first `kept` items and leaves the rest for the next
    /// batch. A returned error means delivery failed (`Error::Permanent` ⇒ do not
    /// retry); on failure `kept` is meaningless.
    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error>;

    /// `DefaultRatePerMin`: the channel's recommended per-minute delivery cap (`0`
    /// = no known limit). Defaults to [`default_rate_per_min`] of [`Channel::kind`].
    fn default_rate_per_min(&self) -> i64 {
        default_rate_per_min(self.kind())
    }

    /// `SecretKeys`: the config keys that are credentials (masked on readback; a
    /// masked value on update keeps the stored original). Defaults to
    /// [`secret_keys`] of [`Channel::kind`].
    fn secret_keys(&self) -> Vec<String> {
        secret_keys(self.kind())
            .unwrap_or(&[])
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// `DestinationKeys`: the config keys that decide "where messages go". Kept
    /// separate from [`Channel::secret_keys`] so a config edit cannot redirect the
    /// stored real credentials to an attacker-controlled server (see Go
    /// `PrepareConfigUpdate`). Defaults to [`destination_keys`] of [`Channel::kind`].
    fn destination_keys(&self) -> Vec<String> {
        destination_keys(self.kind())
            .unwrap_or(&[])
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}

/// `Get` (`channel.go`): the channel adapter registered for `kind`, or `None` if the
/// kind is unknown. Each call returns a fresh **stateless** instance (the Go registry
/// holds zero-sized structs; adapters carry no fields, credentials always arrive via
/// `cfg`), so a boxed value per call is equivalent and keeps call sites simple.
///
/// The set here grows unit by unit as each adapter lands (webhook, then email); the
/// pure `valid_kind`/`kinds` from `notify-rs` already describe the full six-kind
/// registry for the config-masking layer.
pub fn channel(kind: &str) -> Option<Box<dyn Channel>> {
    match kind {
        KIND_DINGTALK => Some(Box::new(dingtalk::DingTalkChannel)),
        KIND_FEISHU => Some(Box::new(feishu::FeishuChannel)),
        KIND_WECOM => Some(Box::new(wecom::WeComChannel)),
        KIND_TELEGRAM => Some(Box::new(telegram::TelegramChannel)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_title_prefers_name_then_class_then_placeholder() {
        let base = Item::default();
        assert_eq!(base.title(), "(unnamed vulnerability)");
        let named = Item {
            name: "SQLi".into(),
            ..Default::default()
        };
        assert_eq!(named.title(), "SQLi");
        let classed = Item {
            vulnclass: "sqli".into(),
            ..Default::default()
        };
        assert_eq!(classed.title(), "sqli");
        // A name wins over the class.
        let both = Item {
            name: "N".into(),
            vulnclass: "C".into(),
            ..Default::default()
        };
        assert_eq!(both.title(), "N");
    }

    #[test]
    fn item_is_status_change() {
        assert!(!Item::default().is_status_change());
        assert!(Item {
            from_status: "open".into(),
            ..Default::default()
        }
        .is_status_change());
        assert!(Item {
            to_status: "fixed".into(),
            ..Default::default()
        }
        .is_status_change());
    }

    #[test]
    fn message_limits_default_when_absent() {
        let m = Message::default();
        assert_eq!(
            m.limits(),
            Limits {
                summary_runes: 120,
                assets_shown: 3
            }
        );
        // A Some(0,0) is meaningful (no truncation) and must not fall back.
        let m = Message {
            limits: Some(Limits {
                summary_runes: 0,
                assets_shown: 0,
            }),
            ..Default::default()
        };
        assert_eq!(
            m.limits(),
            Limits {
                summary_runes: 0,
                assets_shown: 0
            }
        );
    }

    #[test]
    fn error_permanent_classification() {
        let p = Error::permanent("bad key");
        assert!(p.is_permanent());
        assert_eq!(p.message(), "bad key");
        let t = Error::msg("dial tcp: reset");
        assert!(!t.is_permanent());
        assert!(is_permanent(&permanent("nope")));
        assert_eq!(
            format!("{}", Error::transient("rate limited")),
            "rate limited"
        );
    }

    #[test]
    fn context_cancellation() {
        assert!(!Context::background().is_cancelled());
        assert!(Context::background().err().is_none());
        assert!(Context::canceled().is_cancelled());
        assert!(Context::deadline_exceeded().is_cancelled());
    }
}
