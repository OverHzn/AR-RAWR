//! Rust port of the pure `notify/pack_test.go` specs — the "pack whole items"
//! digest fix and the untrusted-content escaping invariant, exercised through the
//! re-exported/fresh Markdown renderers in [`artex::notify::markdown`].
//!
//! These tests do not need a network or a channel adapter: they pin the renderer's
//! contract (whole-item packing, honest `kept` count, structural escaping) that the
//! Telegram/Feishu/DingTalk/WeCom adapters all build on. The `telegramHTML` /
//! `feishuCard` cases in the Go file belong to those channel adapters and are
//! covered in the channels unit.

use artex::notify::markdown::{markdown_body, write_item};
use artex::notify::{
    markdown_escape, Item, Message, DEFAULT_MAX_ASSETS_SHOWN, DEFAULT_MAX_SUMMARY_RUNES,
};

/// WeCom group bot markdown hard limit (bytes, not characters).
const WECOM_MARKDOWN_LIMIT: i64 = 4096;

/// `singleMsg` (`channels_test.go`): a single-send message with quotes and
/// newlines — exactly the input that most easily breaks template interpolation.
fn single_msg() -> Message {
    Message {
        items: vec![Item {
            finding_id: 42,
            name: "login page \"SQL injection\" risk".into(),
            vulnclass: "SQL injection".into(),
            severity: "high".into(),
            summary: "param id\nunfiltered leads to injection".into(),
            assets: vec!["a.example.com".into(), "b.example.com".into()],
            detail_url: "https://artex.local/function/findings/detail?id=42".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// `batchMsg` (`channels_test.go`): a batch digest message of `n` items.
fn batch_msg(n: usize) -> Message {
    let mut m = Message {
        batch: true,
        window_minutes: 30,
        home_url: "https://artex.local/function/findings".into(),
        ..Default::default()
    };
    for i in 0..n {
        m.items.push(Item {
            finding_id: (i + 1) as i64,
            name: format!("vuln{}", i + 1),
            vulnclass: "XSS".into(),
            severity: "medium".into(),
            summary: "reflected cross-site scripting".into(),
            assets: vec!["target.example.com".into()],
            ..Default::default()
        });
    }
    m
}

/// `TestMarkdownBodyPacksWholeItemsWithinByteLimit`.
#[test]
fn markdown_body_packs_whole_items_within_byte_limit() {
    let m = batch_msg(200);
    let (body, kept) = markdown_body(&m, WECOM_MARKDOWN_LIMIT);

    assert!(
        (body.len() as i64) <= WECOM_MARKDOWN_LIMIT,
        "body of {} bytes exceeds the limit of {}",
        body.len(),
        WECOM_MARKDOWN_LIMIT
    );
    assert!(
        kept > 0 && kept < m.items.len(),
        "only a part should fit (0 < kept < {}), got {kept}",
        m.items.len()
    );
    assert!(
        body.contains("remaining") && body.contains("next message"),
        "the header should state how many items are not included:\n{}",
        &body[..body.len().min(400)]
    );
    for i in 0..kept {
        assert!(
            body.contains(&format!("vuln{}", i + 1)),
            "item {} should be in this message",
            i + 1
        );
    }
    assert!(
        !body.contains(&format!("vuln{}", kept + 1)),
        "item {} should not appear (it belongs to the next batch)",
        kept + 1
    );
}

/// `TestMarkdownBodyKeepsEverythingWhenUnderLimit`.
#[test]
fn markdown_body_keeps_everything_when_under_limit() {
    let m = batch_msg(3);
    let (body, kept) = markdown_body(&m, 0); // 0 = unlimited
    assert_eq!(
        kept,
        m.items.len(),
        "everything should be kept when length is unlimited, got kept={kept}"
    );
    assert!(
        !body.contains("remaining"),
        "no truncation notice should appear when nothing was truncated:\n{body}"
    );
}

/// `TestMarkdownBodyAlwaysKeepsAtLeastOneItem`.
#[test]
fn markdown_body_always_keeps_at_least_one_item() {
    // When the budget cannot fit even one item, still send one (final truncation
    // as the backstop) — otherwise an oversized vulnerability jams the batch.
    let m = batch_msg(5);
    let (_, kept) = markdown_body(&m, 50);
    assert_eq!(kept, 1, "at least 1 item should be kept, got {kept}");
}

/// `TestMarkdownBodySingleReturnsOne`.
#[test]
fn markdown_body_single_returns_one() {
    let (_, kept) = markdown_body(&single_msg(), 4096);
    assert_eq!(
        kept, 1,
        "a single message should report 1 delivered, got {kept}"
    );
    let (_, k) = markdown_body(&Message::default(), 4096);
    assert_eq!(k, 0, "an empty message should report 0 items, got {k}");
}

/// `TestWebhookAndEmailReportAllItems`.
#[test]
fn webhook_and_email_report_all_items() {
    // These channels do not truncate; the whole batch counts as delivered.
    let m = batch_msg(7);
    assert_eq!(m.items.len(), 7, "precondition does not hold");
    let (_, k) = markdown_body(&m, 0);
    assert_eq!(
        k,
        m.items.len(),
        "everything should be used when length is unlimited, got {k}"
    );
}

/// `TestMarkdownEscapesUntrustedContent`: regression for "untrusted content must
/// not change message structure". Titles/summaries come from model output (which
/// reads the tested target's responses); asset names come from scanned URLs.
#[test]
fn markdown_escapes_untrusted_content() {
    struct Case {
        name: &'static str,
        item: Item,
        must: Vec<&'static str>,
        wrong: Vec<&'static str>,
    }
    let cases = vec![
        Case {
            name: "newline + external link in the title",
            item: Item {
                severity: "high".into(),
                name: "SQL injection on login page\n[URGENT: click to verify your account](http://attacker.tld)".into(),
                ..Default::default()
            },
            must: vec![r"\[URGENT: click to verify your account\]", r"\(http://attacker.tld\)"],
            wrong: vec!["\n[URGENT", "\n\n[URGENT"],
        },
        Case {
            name: "image beacon in the title",
            item: Item {
                severity: "high".into(),
                name: "Vuln ![](http://attacker.tld/beacon)".into(),
                ..Default::default()
            },
            must: vec![r"\!", r"\(http://attacker.tld/beacon\)"],
            wrong: vec!["![]("],
        },
        Case {
            name: "emphasis and quote in an asset name",
            item: Item {
                severity: "high".into(),
                name: "plain title".into(),
                assets: vec!["a.com/*inject*>quote".into()],
                ..Default::default()
            },
            must: vec![r"\*inject\*", r"\>"],
            wrong: vec!["*inject*"],
        },
        Case {
            name: "backticks and pipe in the summary",
            item: Item {
                severity: "high".into(),
                name: "title".into(),
                summary: "`code` | table".into(),
                ..Default::default()
            },
            must: vec![r"\`code\`", r"\|"],
            wrong: vec!["`code`"],
        },
    ];
    for tc in cases {
        let lim = Message::default().limits();
        let got = write_item(&tc.item, "", true, lim);
        for want in &tc.must {
            assert!(
                got.contains(want),
                "{}: missing escaped form {want:?}:\n{got}",
                tc.name
            );
        }
        for bad in &tc.wrong {
            assert!(
                !got.contains(bad),
                "{}: unescaped form {bad:?} appeared (injectable structure/links):\n{got}",
                tc.name
            );
        }
    }
}

/// `TestMarkdownEscapeBackslashFirst`: the escape order — the backslash must be
/// handled first, else the backslashes added later get another layer.
#[test]
fn markdown_escape_backslash_first() {
    assert_eq!(markdown_escape(r"a\b*c"), r"a\\b\*c");
}

/// Sanity-check the limits accessor the renderers depend on (defaults when absent).
#[test]
fn message_limits_defaults_for_render() {
    let m = Message::default();
    assert_eq!(m.limits().summary_runes, DEFAULT_MAX_SUMMARY_RUNES);
    assert_eq!(m.limits().assets_shown, DEFAULT_MAX_ASSETS_SHOWN);
}

// === HTML body rendering (html.go) ===

use artex::notify::html::{html_body, html_item, html_title};

/// `TestEmailEscapesStructuralInjection`: the email body must defend against the
/// injections that really matter — the title and summary come from the tested
/// target / model output (untrusted). Text positions escape `& < >`; attribute
/// positions (the backlink href) also escape quotes.
#[test]
fn email_escapes_structural_injection() {
    let m = Message {
        items: vec![Item {
            severity: "high".into(),
            name: "<script>alert(1)</script>".into(),
            summary: "a & b > c".into(),
            detail_url: "https://artex.local/x?a=\"onmouseover=alert(1)".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let html = html_body(&m, 0);
    assert!(
        !html.contains("<script>"),
        "title not escaped, tag injection possible: {html}"
    );
    assert!(
        html.contains("&lt;script&gt;"),
        "expected escaped entities: {html}"
    );
    assert!(
        html.contains("a &amp; b &gt; c"),
        "& and > not escaped: {html}"
    );
    // Attribute positions must escape quotes, else a quote closes the href and
    // injects an event handler.
    assert!(
        !html.contains("onmouseover=alert(1)\">"),
        "href attribute not escaped correctly: {html}"
    );
    assert!(
        html.contains("&quot;"),
        "quotes in attribute positions should be escaped: {html}"
    );
}

/// `TestEmailTitleKeepsQuotesVerbatim`: double quotes are legal in HTML text
/// content and need no escaping; the title must appear verbatim in the text
/// position (guards against a future quote-escaping layer).
#[test]
fn email_title_keeps_quotes_verbatim() {
    let m = single_msg(); // title: `login page "SQL injection" risk`
    let html = html_body(&m, 0);
    assert!(html.starts_with("<div"), "body is not HTML: {:.80}", html);
    assert!(
        html.contains("\"SQL injection\""),
        "quotes in the title should be kept verbatim in the text position: {:.200}",
        html
    );
}

/// `html_title` equals `markdown_title` (the email subject).
#[test]
fn html_title_matches_markdown_title() {
    let single = single_msg();
    assert_eq!(
        html_title(&single),
        artex::notify::markdown::markdown_title(&single)
    );
    assert_eq!(
        html_title(&batch_msg(3)),
        "Vulnerability digest \u{00B7} 3 total"
    );
}

/// `html_item` single mode contains the status-change and backlink lines.
#[test]
fn html_item_full_renders_status_and_backlink() {
    let it = Item {
        severity: "high".into(),
        name: "n".into(),
        from_status: "pending".into(),
        to_status: "fixed".into(),
        detail_url: "https://artex.local/x".into(),
        ..Default::default()
    };
    let lim = artex::notify::Limits {
        summary_runes: DEFAULT_MAX_SUMMARY_RUNES,
        assets_shown: DEFAULT_MAX_ASSETS_SHOWN,
    };
    let out = html_item(&it, true, lim);
    assert!(
        out.contains("Status change"),
        "full item should show the status change: {out}"
    );
    assert!(
        out.contains("View details"),
        "full item should show the backlink: {out}"
    );
    assert!(
        out.contains("https://artex.local/x"),
        "backlink href present: {out}"
    );
}
