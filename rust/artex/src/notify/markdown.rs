//! Rust port of `notify/markdown.go` + the `packItemCount` helper from
//! `notify/render.go` — the Markdown renderers shared by the "Markdown family"
//! channels (DingTalk, WeCom).
//!
//! All the leaf helpers are **reused** from the parity crates via [`super`]
//! (`markdown_text`, `markdown_escape`, `asset_line`, `truncate_bytes`,
//! `severity_label`, `status_label`, `byte_size`, …). This module only adds the
//! `Message`/`Item`-aware composition: [`markdown_title`], [`markdown_body`],
//! [`markdown_batch_intro`], [`write_item`], and the [`pack_item_count`] packing
//! loop. The body returns the **number of items actually written** so a truncated
//! digest marks only the delivered items (the "pack whole items" fix).

use super::{
    asset_line, byte_size, markdown_text, one_line, severity_label, status_label, truncate_bytes,
    Item, Limits, Message, MARKDOWN_RESERVED_BYTES,
};

/// Render one vulnerability item into Markdown.
///
/// `prefix` is the sequence number in a digest list; `single = true` renders the
/// full version (summary + backlink), while a digest renders only a one-line
/// summary. All outside content (title/class/assets/summary) goes through
/// [`markdown_text`] (single-line + escape); the backlink comes from the
/// admin-configured `public_base_url`, is not untrusted, and is emitted as-is so
/// it stays clickable.
pub fn write_item(it: &Item, prefix: &str, single: bool, lim: Limits) -> String {
    let sev = severity_label(it.severity.clone()).unwrap_or_else(|e| e);
    let mut out = format!(
        "{prefix}**{sev} \u{00B7} {}**",
        markdown_text(it.title(), 0)
    );

    if !single {
        // Digest mode: a single line with assets and summary compressed in.
        let mut extras: Vec<String> = Vec::new();
        let a = asset_line(&it.assets, lim.assets_shown);
        if !a.is_empty() {
            extras.push(markdown_text(&a, 0));
        }
        if !it.summary.is_empty() {
            extras.push(markdown_text(&it.summary, 60));
        }
        if !extras.is_empty() {
            out.push_str(" \u{2014} ");
            out.push_str(&extras.join(" \u{00B7} "));
        }
        out.push('\n');
        return out;
    }

    out.push('\n');
    if it.is_status_change() {
        let from = status_label(it.from_status.clone()).unwrap_or_else(|e| e);
        let to = status_label(it.to_status.clone()).unwrap_or_else(|e| e);
        out.push_str(&format!(
            "**Status change**: {} \u{2192} {}\n",
            markdown_text(&from, 0),
            markdown_text(&to, 0)
        ));
    }
    if !it.vulnclass.is_empty() && it.vulnclass != it.title() {
        out.push_str(&format!("**Type**: {}\n", markdown_text(&it.vulnclass, 0)));
    }
    let a = asset_line(&it.assets, lim.assets_shown);
    if !a.is_empty() {
        out.push_str(&format!("**Assets**: {}\n", markdown_text(&a, 0)));
    }
    if !it.summary.is_empty() {
        let s = markdown_text(&it.summary, lim.summary_runes);
        if !s.is_empty() {
            out.push_str(&format!("**Summary**: {s}\n"));
        }
    }
    if !it.detail_url.is_empty() {
        out.push_str(&format!("[View details]({})\n", it.detail_url));
    }
    out
}

/// `markdownTitle`: the message title (IM title bar / card title). Its content is
/// the **unescaped original text** — each of the four rendering contexts escapes
/// differently, so escaping belongs to the output side, not here.
pub fn markdown_title(m: &Message) -> String {
    if m.batch {
        return format!("Vulnerability digest \u{00B7} {} total", m.items.len());
    }
    if m.items.is_empty() {
        return "Vulnerability notification".to_string();
    }
    let it = &m.items[0];
    let sev = severity_label(it.severity.clone()).unwrap_or_else(|e| e);
    format!(
        "[{}] {}",
        sev,
        one_line(it.title().to_string(), 0).unwrap_or_default()
    )
}

/// `markdownBody`: render the Markdown body and return it with the **number of
/// items actually written** (`kept`). The caller uses `kept` to mark only the
/// first `kept` items delivered — the rest stay for the next batch. `max_bytes <=
/// 0` means unlimited.
pub fn markdown_body(m: &Message, max_bytes: i64) -> (String, usize) {
    if !m.batch {
        if m.items.is_empty() {
            return (String::new(), 0);
        }
        let s = write_item(&m.items[0], "", true, m.limits());
        // A single message is sent even if oversized (final truncation as the
        // backstop): partial information beats sending nothing.
        return (truncate_bytes(s, max_bytes).unwrap_or_default(), 1);
    }

    let footer = if !m.home_url.is_empty() {
        format!("\n[View all on the platform]({})\n", m.home_url)
    } else {
        String::new()
    };
    let lim = m.limits();
    let kept = pack_item_count(
        &m.items,
        max_bytes,
        MARKDOWN_RESERVED_BYTES,
        &footer,
        byte_size,
        |it, idx| write_item(it, &format!("{}. ", idx + 1), false, lim),
    );

    let items = &m.items[..kept];
    let mut b = String::new();
    b.push_str(&markdown_batch_intro(m, items, m.items.len()));
    for (i, it) in items.iter().enumerate() {
        b.push_str(&write_item(it, &format!("{}. ", i + 1), false, lim));
    }
    b.push_str(&footer);
    (truncate_bytes(b, max_bytes).unwrap_or_default(), kept)
}

/// `markdownBatchIntro`: the digest opening — time window, item count, severity
/// distribution. When `items` (actually included) differ from `total` (the whole
/// batch), the gap is stated explicitly so readers do not take the header count
/// for the whole batch.
pub fn markdown_batch_intro(m: &Message, items: &[Item], total: usize) -> String {
    let mut b = String::new();
    if m.window_minutes > 0 {
        b.push_str(&format!(
            "**{total} new vulnerabilities in the last {} minutes**",
            m.window_minutes
        ));
    } else {
        b.push_str(&format!("**{total} new vulnerabilities**"));
    }
    let extra = total as i64 - items.len() as i64;
    if extra > 0 {
        b.push_str(&format!(
            " (this message shows the first {}, the remaining {extra} will continue in the next message)",
            items.len()
        ));
    }
    let mut counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for it in items {
        *counts.entry(it.severity.as_str()).or_insert(0) += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for sev in ["critical", "high", "medium", "low"] {
        let n = counts.get(sev).copied().unwrap_or(0);
        if n > 0 {
            let label = severity_label(sev.to_string()).unwrap_or_else(|e| e);
            parts.push(format!("{label} {n}"));
        }
    }
    if !parts.is_empty() {
        b.push('\n');
        b.push_str(&parts.join(" \u{00B7} "));
    }
    b.push_str("\n\n");
    b
}

/// `packItemCount` (`render.go`): how many items fit **wholly** within the budget,
/// so a digest is packed item by item instead of rendered-then-truncated (which
/// silently drops the overflow while the delivery log shows all sent).
///
/// `max_size <= 0` means unlimited. `reserve` is space for the header/footer;
/// `size` measures text (bytes or runes per channel); `render` renders item `idx`
/// into its actual text. Returns at least 1 whenever there is at least one item —
/// even a single oversized item must be sent, with the caller's final truncation
/// as the backstop, or one giant vulnerability would jam the whole batch forever.
pub fn pack_item_count<S, R>(
    items: &[Item],
    max_size: i64,
    reserve: i64,
    footer: &str,
    size: S,
    render: R,
) -> usize
where
    S: Fn(&str) -> i64,
    R: Fn(&Item, usize) -> String,
{
    if max_size <= 0 {
        return items.len();
    }
    let mut budget = max_size - reserve - size(footer);
    if budget < 0 {
        budget = 0;
    }
    let mut used = 0i64;
    for (i, it) in items.iter().enumerate() {
        used += size(&render(it, i));
        if used > budget && i > 0 {
            return i;
        }
    }
    items.len()
}
