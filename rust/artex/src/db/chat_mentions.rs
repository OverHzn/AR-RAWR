//! Chat mention search — port of `db/chat_mentions.go`.
//!
//! `SearchChatMentions`/`SearchChatMentionsPage` page the shared catalog (findings,
//! companies, assets) for the `@`-mention picker, just like the asset/finding pages.
//! Search values stay SQL parameters; `%`, `_` and `\` in the query are escaped and
//! treated literally. Pagination uses the last row's stable sort key (a base64 JSON
//! cursor), not an offset, so later pages don't re-skip earlier results.
//!
//! The cursor format mirrors Go byte-for-byte: `base64.RawURLEncoding` (URL-safe,
//! no padding) over `encoding/json` of `{ID,Kind,Exact,Scope,Query}` (Go's exported
//! field names, capitalised). Each side only ever reads its own cursors, but matching
//! the bytes keeps the wire format identical.

use crate::db::error::{Error, Result};
use crate::db::DB;
use crate::llm::Context;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// `ChatMention` — a lightweight search result; details are re-read on send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMention {
    pub kind: String,
    pub id: i64,
    pub label: String,
    pub description: String,
}

/// `ChatMentionPage` — one page of results plus the cursor for the next page.
#[derive(Debug, Clone, Default)]
pub struct ChatMentionPage {
    pub items: Vec<ChatMention>,
    pub next_cursor: String,
}

/// `ErrInvalidChatMentionCursor`: "invalid pagination cursor, please search again".
pub const ERR_INVALID_CHAT_MENTION_CURSOR: &str = "invalid pagination cursor, please search again";

/// `chatMentionCursor`: the stable-sort key embedded in `next_cursor`. Field names
/// match Go's exported names so the JSON is byte-identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMentionCursor {
    #[serde(rename = "ID")]
    id: i64,
    #[serde(rename = "Kind")]
    kind: String,
    #[serde(rename = "Exact")]
    exact: bool,
    #[serde(rename = "Scope")]
    scope: String,
    #[serde(rename = "Query")]
    query: String,
}

/// `ValidChatMentionKind`: is `kind` a searchable catalog kind?
pub fn valid_chat_mention_kind(kind: &str) -> bool {
    matches!(
        kind,
        "finding"
            | "company"
            | "asset"
            | "endpoint"
            | "ip"
            | "app"
            | "root_domain"
            | "subdomain"
            | "service"
    )
}

/// Escape `%`, `_` and `\` so they are literal in a LIKE pattern (Go's replacer).
fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for ch in query.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            _ => out.push(ch),
        }
    }
    out.push('%');
    out
}

/// The shared-catalog search query, parameterised by
/// `$1 kind, $2 query, $3 pattern, $4 afterID, $5 afterKind, $6 afterExact`.
const CHAT_MENTIONS_SQL: &str = r#"
SELECT kind, id, left(label, 160), left(description, 240) FROM (
 (SELECT 'finding' AS kind, id, COALESCE(NULLIF(name,''), vulnclass) AS label,
         concat_ws(' · ', severity, status, left(summary, 160)) AS description
  FROM findings WHERE ($1='' OR $1='finding') AND
    ($2='' OR id::text=$2 OR concat_ws(' ',name,vulnclass,summary) ILIKE $3)
    AND ($4::bigint=0 OR (id::text=$2)<$6 OR ((id::text=$2)=$6 AND (id<$4 OR (id=$4 AND 'finding'>$5))))
  ORDER BY (id::text=$2) DESC, id DESC LIMIT 21)
 UNION ALL
 (SELECT 'company', id, name, nkey FROM companies
  WHERE ($1='' OR $1='company') AND ($2='' OR id::text=$2 OR name ILIKE $3 OR nkey ILIKE $3)
    AND ($4::bigint=0 OR (id::text=$2)<$6 OR ((id::text=$2)=$6 AND (id<$4 OR (id=$4 AND 'company'>$5))))
  ORDER BY (id::text=$2) DESC, id DESC LIMIT 21)
 UNION ALL
 (SELECT type, id,
    CASE WHEN type='endpoint' THEN concat_ws(' ',NULLIF(method,''),url)
         ELSE COALESCE(NULLIF(app_name,''),NULLIF(url,''),NULLIF(domain,''),NULLIF(ip,''),NULLIF(bundle_id,''),'Asset #'||id::text) END,
    concat_ws(' · ',type,NULLIF(page_title,''),NULLIF(service_name,''),NULLIF(bundle_id,''),NULLIF(ip,''),port::text)
  FROM assets WHERE ($1='' OR $1='asset' OR type=$1) AND
    ($2='' OR id::text=$2 OR concat_ws(' ',domain,root_domain,ip,url,app_name,bundle_id,page_title,service_name,method) ILIKE $3)
    AND ($4::bigint=0 OR (id::text=$2)<$6 OR ((id::text=$2)=$6 AND (id<$4 OR (id=$4 AND type>$5))))
  ORDER BY (id::text=$2) DESC, id DESC LIMIT 21)
) matches ORDER BY (id::text=$2) DESC, id DESC, kind LIMIT 21"#;

impl DB {
    /// `SearchChatMentions`: the first page's items for `kind`/`query`.
    pub async fn search_chat_mentions(
        &self,
        ctx: &Context,
        kind: &str,
        query: &str,
    ) -> Result<Vec<ChatMention>> {
        let page = self.search_chat_mentions_page(ctx, kind, query, "").await?;
        Ok(page.items)
    }

    /// `SearchChatMentionsPage`: one page using the last result's stable sort key.
    pub async fn search_chat_mentions_page(
        &self,
        ctx: &Context,
        kind: &str,
        query: &str,
        cursor: &str,
    ) -> Result<ChatMentionPage> {
        let mut page = ChatMentionPage::default();
        if !kind.is_empty() && !valid_chat_mention_kind(kind) {
            return Err(Error::msg("unsupported mention kind"));
        }
        // Decode + validate the cursor.
        let mut after = ChatMentionCursor {
            id: 0,
            kind: String::new(),
            exact: false,
            scope: String::new(),
            query: String::new(),
        };
        if !cursor.is_empty() {
            let valid = (|| -> Option<()> {
                if cursor.len() > 2048 {
                    return None;
                }
                let blob = URL_SAFE_NO_PAD.decode(cursor).ok()?;
                let c: ChatMentionCursor = serde_json::from_slice(&blob).ok()?;
                if c.id <= 0
                    || !valid_chat_mention_kind(&c.kind)
                    || c.scope != kind
                    || c.query != query
                {
                    return None;
                }
                after = c;
                Some(())
            })();
            if valid.is_none() {
                return Err(Error::sentinel(
                    ERR_INVALID_CHAT_MENTION_CURSOR,
                    ERR_INVALID_CHAT_MENTION_CURSOR,
                ));
            }
        }

        let pattern = like_pattern(query);
        // `QueryContext`: an already-cancelled context returns its error up front.
        if let Some(e) = ctx.err() {
            return Err(e.into());
        }
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 6] = [
            &kind,
            &query,
            &pattern,
            &after.id,
            &after.kind,
            &after.exact,
        ];
        let fut = self.query(CHAT_MENTIONS_SQL, &params);
        tokio::pin!(fut);
        let rows = tokio::select! {
            biased;
            () = ctx.cancelled() => {
                return Err(ctx.err().unwrap_or(crate::llm::Error::Canceled).into());
            }
            rows = fut => rows?,
        };

        for row in &rows {
            page.items.push(ChatMention {
                kind: row.get(0),
                id: row.get(1),
                label: row.get(2),
                description: row.get(3),
            });
        }
        if page.items.len() > 20 {
            page.items.truncate(20);
            let last = &page.items[19];
            let c = ChatMentionCursor {
                id: last.id,
                kind: last.kind.clone(),
                exact: last.id.to_string() == query,
                scope: kind.to_string(),
                query: query.to_string(),
            };
            let blob = serde_json::to_vec(&c).map_err(|e| Error::msg(e.to_string()))?;
            page.next_cursor = URL_SAFE_NO_PAD.encode(&blob);
        }
        Ok(page)
    }
}
