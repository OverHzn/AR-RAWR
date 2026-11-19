//! Rust port of `notify/html.go` — the HTML renderers for email (and, later, the
//! Telegram HTML path reuses the same escapes).
//!
//! All leaf helpers are **reused** from the parity crates via [`super`]
//! (`html_escape`, `html_escape_attr`, `html_severity_color`, `asset_line`,
//! `one_line`, `truncate_html`, `severity_label`, `status_label`). This module
//! adds the `Message`/`Item`-aware composition: [`html_title`], [`html_body`],
//! [`html_batch_intro`], [`html_item`]. Email deliberately uses inline styles +
//! a simple table-free layout — email clients (Outlook, enterprise mailboxes)
//! vary wildly on `<style>`/flex/grid support, so inline is the only thing that
//! renders correctly everywhere.

use super::{
    asset_line, html_escape, html_escape_attr, html_severity_color, one_line, severity_label,
    status_label, truncate_html, Item, Limits, Message,
};

use crate::notify::markdown::markdown_title;

/// `htmlTitle`: the email subject (identical to the Markdown title).
pub fn html_title(m: &Message) -> String {
    markdown_title(m)
}

/// `htmlBody`: render the email body HTML. `max_runes <= 0` means no truncation.
pub fn html_body(m: &Message, max_runes: i64) -> String {
    let mut b = String::new();
    b.push_str(
        "<div style=\"font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',\
'PingFang SC','Microsoft YaHei',sans-serif;font-size:14px;color:#262626;line-height:1.6;\">",
    );
    if m.batch {
        b.push_str(&html_batch_intro(m));
        for it in &m.items {
            b.push_str(&html_item(it, false, m.limits()));
        }
    } else if !m.items.is_empty() {
        b.push_str(&html_item(&m.items[0], true, m.limits()));
    }
    if !m.home_url.is_empty() {
        b.push_str(&format!(
            "<p style=\"margin:16px 0 0;\"><a href=\"{}\" style=\"color:#1677ff;\">View all on the platform</a></p>",
            html_escape_attr(&m.home_url)
        ));
    }
    b.push_str("</div>");
    truncate_html(b, max_runes).unwrap_or_default()
}

/// `htmlBatchIntro`: the digest email opening — item count + severity distribution.
pub fn html_batch_intro(m: &Message) -> String {
    let mut b = String::new();
    if m.window_minutes > 0 {
        b.push_str(&format!(
            "<h2 style=\"font-size:16px;margin:0 0 4px;\">{} new vulnerabilities in the last {} minutes</h2>",
            m.items.len(),
            m.window_minutes
        ));
    } else {
        b.push_str(&format!(
            "<h2 style=\"font-size:16px;margin:0 0 4px;\">{} new vulnerabilities</h2>",
            m.items.len()
        ));
    }
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for it in &m.items {
        *counts.entry(it.severity.as_str()).or_insert(0) += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for sev in ["critical", "high", "medium", "low"] {
        let n = counts.get(sev).copied().unwrap_or(0);
        if n > 0 {
            let color = html_severity_color(sev);
            let label = severity_label(sev.to_string()).unwrap_or_else(|e| e);
            parts.push(format!(
                "<span style=\"color:{color};font-weight:600;\">{} {n}</span>",
                html_escape(&label)
            ));
        }
    }
    if !parts.is_empty() {
        b.push_str(&format!(
            "<p style=\"margin:0 0 12px;\">{}</p>",
            parts.join(" &middot; ")
        ));
    }
    b
}

/// `htmlItem`: render one vulnerability. `full = true` includes the summary and
/// backlink (single push); `false` compresses it into one line (digest list).
pub fn html_item(it: &Item, full: bool, lim: Limits) -> String {
    let color = html_severity_color(&it.severity);
    let mut b = String::new();
    if full {
        b.push_str(&format!(
            "<div style=\"border-left:4px solid {color};padding:8px 0 8px 12px;margin-bottom:12px;\">"
        ));
    } else {
        b.push_str(&format!(
            "<div style=\"border-left:3px solid {color};padding:4px 0 4px 10px;margin-bottom:8px;\">"
        ));
    }
    let sev_label = severity_label(it.severity.clone()).unwrap_or_else(|e| e);
    b.push_str(&format!(
        "<div style=\"font-weight:600;\">{} &middot; {}</div>",
        html_escape(&sev_label),
        html_escape(it.title())
    ));

    if !full {
        let mut extras: Vec<String> = Vec::new();
        let a = asset_line(&it.assets, lim.assets_shown);
        if !a.is_empty() {
            extras.push(html_escape(&a));
        }
        if !it.summary.is_empty() {
            extras.push(html_escape(
                &one_line(it.summary.clone(), 60).unwrap_or_default(),
            ));
        }
        if !extras.is_empty() {
            b.push_str(&format!(
                "<div style=\"color:#595959;font-size:13px;\">{}</div>",
                extras.join(" &middot; ")
            ));
        }
        b.push_str("</div>");
        return b;
    }

    if it.is_status_change() {
        let from = status_label(it.from_status.clone()).unwrap_or_else(|e| e);
        let to = status_label(it.to_status.clone()).unwrap_or_else(|e| e);
        b.push_str(&format!(
            "<div><b>Status change</b>: {} \u{2192} {}</div>",
            html_escape(&from),
            html_escape(&to)
        ));
    }
    if !it.vulnclass.is_empty() && it.vulnclass != it.title() {
        b.push_str(&format!(
            "<div><b>Type</b>: {}</div>",
            html_escape(&it.vulnclass)
        ));
    }
    let a = asset_line(&it.assets, lim.assets_shown);
    if !a.is_empty() {
        b.push_str(&format!("<div><b>Assets</b>: {}</div>", html_escape(&a)));
    }
    let s = one_line(it.summary.clone(), lim.summary_runes).unwrap_or_default();
    if !s.is_empty() {
        b.push_str(&format!("<div><b>Summary</b>: {}</div>", html_escape(&s)));
    }
    if !it.detail_url.is_empty() {
        b.push_str(&format!(
            "<div style=\"margin-top:6px;\"><a href=\"{}\" style=\"color:#1677ff;\">View details</a></div>",
            html_escape_attr(&it.detail_url)
        ));
    }
    b.push_str("</div>");
    b
}
