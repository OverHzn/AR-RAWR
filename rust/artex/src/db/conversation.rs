//! Conversations — port of `db/conversation.go`.
//!
//! A conversation is a ChatGPT-style chat thread bound to an agent key, living
//! independent of the pentest exploration graph. This module ports the CRUD +
//! pin/profile/token-summary/activity machinery. Activity rows are keyed by
//! `conversation_id` and reuse the [`Activity`] struct (its `node_id` is ignored).

use std::time::SystemTime;

use crate::db::error::{Error, Result};
use crate::db::exploration::{utf8_clean, Activity};
use crate::db::DB;

/// `ErrLLMProfileNotFound`: a referenced LLM profile does not exist (Go sentinel,
/// from `lockLLMProfileForReference`).
pub const ERR_LLM_PROFILE_NOT_FOUND: &str = "LLM profile not found";

/// `TargetPinRevokedConversationDelete`: the revoked_reason when a conversation's
/// target pins are revoked on deletion (Go `target_pins.go`).
pub const TARGET_PIN_REVOKED_CONVERSATION_DELETE: &str = "conversation_deleted";

const CONV_COLS: &str =
    "id, agent_key, title, llm_profile_id, pinned_at, created_at, updated_at, target_gate";

/// `ConvTokenSummary`: one conversation's token total (sum of its `kind='result'`
/// rows) with its profile + created_at, used to merge chat usage into the dashboard.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConvTokenSummary {
    pub llm_profile_id: Option<i64>,
    /// `created_at::text` (Go stores it as a string).
    pub created_at: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

/// `Conversation`: one chat thread. `pinned` is derived from `pinned_at IS NOT NULL`.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: i64,
    pub agent_key: String,
    pub title: String,
    pub llm_profile_id: Option<i64>,
    pub pinned: bool,
    pub pinned_at: Option<SystemTime>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub target_gate: bool,
}

impl Default for Conversation {
    fn default() -> Self {
        Conversation {
            id: 0,
            agent_key: String::new(),
            title: String::new(),
            llm_profile_id: None,
            pinned: false,
            pinned_at: None,
            created_at: SystemTime::UNIX_EPOCH,
            updated_at: SystemTime::UNIX_EPOCH,
            target_gate: false,
        }
    }
}

/// `ConversationPatch`: update only the fields whose options are `Some`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversationPatch {
    pub title: Option<String>,
    pub pinned: Option<bool>,
    pub target_gate: Option<bool>,
}

fn scan_conv(row: &tokio_postgres::Row) -> Conversation {
    let pinned_at: Option<SystemTime> = row.get(4);
    Conversation {
        id: row.get(0),
        agent_key: row.get(1),
        title: row.get(2),
        llm_profile_id: row.get(3),
        pinned: pinned_at.is_some(),
        pinned_at,
        created_at: row.get(5),
        updated_at: row.get(6),
        target_gate: row.get(7),
    }
}

/// Summary-only column set for conversation activity reads (detail is lazy).
const CONV_ACT_COLS: &str = "id, COALESCE(worker,''), COALESCE(kind,''), COALESCE(tool,''), COALESCE(tool_use_id,''), is_error, COALESCE(summary,''), created_at, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens";

fn scan_conv_activity(row: &tokio_postgres::Row) -> Activity {
    Activity {
        id: row.get(0),
        worker: row.get(1),
        kind: row.get(2),
        tool: row.get(3),
        tool_use_id: row.get(4),
        is_error: row.get(5),
        summary: row.get(6),
        created_at: row.get(7),
        input_tokens: row.get(8),
        output_tokens: row.get(9),
        cache_read_tokens: row.get(10),
        cache_write_tokens: row.get(11),
        ..Activity::default()
    }
}

/// `lockLLMProfileForReference`: verify a referenced profile exists under
/// `FOR KEY SHARE`, so a concurrent `DeleteProfile` cannot remove it. A nil id is
/// a no-op; a missing profile is `ERR_LLM_PROFILE_NOT_FOUND`.
async fn lock_llm_profile_for_reference(
    tx: &deadpool_postgres::Transaction<'_>,
    profile_id: Option<i64>,
) -> Result<()> {
    let Some(pid) = profile_id else {
        return Ok(());
    };
    let row = tx
        .query_opt(
            "SELECT id FROM llm_profiles WHERE id=$1 FOR KEY SHARE",
            &[&pid],
        )
        .await?;
    if row.is_none() {
        return Err(Error::sentinel(
            ERR_LLM_PROFILE_NOT_FOUND,
            ERR_LLM_PROFILE_NOT_FOUND,
        ));
    }
    Ok(())
}

/// `revokeConversationPins`: revoke active target pins of the given conversations
/// as history (they are not rows of the thread).
async fn revoke_conversation_pins(
    tx: &deadpool_postgres::Transaction<'_>,
    conv_ids: &[i64],
) -> Result<()> {
    if conv_ids.is_empty() {
        return Ok(());
    }
    tx.execute(
        "UPDATE target_pins SET status='revoked', revoked_at=now(), revoked_reason=$2 \
         WHERE conversation_id=ANY($1::bigint[]) AND status='active'",
        &[&conv_ids, &TARGET_PIN_REVOKED_CONVERSATION_DELETE],
    )
    .await?;
    Ok(())
}

impl DB {
    /// `ConversationTokenSummaries`: one row per conversation, summing result-row
    /// token usage (0 for conversations without a completed run yet).
    pub async fn conversation_token_summaries(&self) -> Result<Vec<ConvTokenSummary>> {
        let rows = self
            .query(
                "SELECT c.llm_profile_id, c.created_at::text, \
                 COALESCE(sum(ca.input_tokens),0), COALESCE(sum(ca.output_tokens),0), \
                 COALESCE(sum(ca.cache_read_tokens),0), COALESCE(sum(ca.cache_write_tokens),0) \
                 FROM conversations c \
                 LEFT JOIN conversation_activities ca ON ca.conversation_id = c.id AND ca.kind = 'result' \
                 GROUP BY c.id",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| ConvTokenSummary {
                llm_profile_id: r.get(0),
                created_at: r.get(1),
                input_tokens: r.get(2),
                output_tokens: r.get(3),
                cache_read_tokens: r.get(4),
                cache_write_tokens: r.get(5),
            })
            .collect())
    }

    /// `CreateConversation`: open a new thread for `agent_key` (no target gate).
    pub async fn create_conversation(
        &self,
        agent_key: &str,
        title: &str,
        llm_profile_id: Option<i64>,
    ) -> Result<Conversation> {
        self.create_conversation_gated(agent_key, title, llm_profile_id, false)
            .await
    }

    /// `CreateConversationGated`: create a thread with the target-gate opt-in set
    /// at creation. The child row is inserted before taking the profile lock, so
    /// it is exclusively owned by this tx first (matching DeleteProfile's protocol).
    pub async fn create_conversation_gated(
        &self,
        agent_key: &str,
        title: &str,
        llm_profile_id: Option<i64>,
        target_gate: bool,
    ) -> Result<Conversation> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let result = async {
            let row = tx
                .query_one(
                    &format!(
                        "INSERT INTO conversations(agent_key, title, llm_profile_id, target_gate) \
                         VALUES ($1, $2, NULL, $3) RETURNING {CONV_COLS}"
                    ),
                    &[&agent_key, &title, &target_gate],
                )
                .await?;
            let mut c = scan_conv(&row);
            lock_llm_profile_for_reference(&tx, llm_profile_id).await?;
            if let Some(pid) = llm_profile_id {
                let row = tx
                    .query_one(
                        &format!(
                            "UPDATE conversations SET llm_profile_id=$2 WHERE id=$1 RETURNING {CONV_COLS}"
                        ),
                        &[&c.id, &pid],
                    )
                    .await?;
                c = scan_conv(&row);
            }
            Ok::<_, crate::db::error::Error>(c)
        }
        .await;
        match result {
            Ok(c) => {
                tx.commit().await?;
                Ok(c)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `UpdateConversationProfile`: set (or clear) the LLM profile override. An
    /// unknown id is a no-op (preserving the previous UPDATE semantics).
    pub async fn update_conversation_profile(
        &self,
        id: i64,
        llm_profile_id: Option<i64>,
    ) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let result = async {
            let row = tx
                .query_opt(
                    "SELECT id FROM conversations WHERE id=$1 FOR UPDATE",
                    &[&id],
                )
                .await?;
            if row.is_none() {
                return Ok(()); // unknown id -> no-op (commit below)
            }
            lock_llm_profile_for_reference(&tx, llm_profile_id).await?;
            tx.execute(
                "UPDATE conversations SET llm_profile_id=$2 WHERE id=$1",
                &[&id, &llm_profile_id],
            )
            .await?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                tx.commit().await?;
                Ok(())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `ListConversations`: all threads, pinned first then most-recently-updated.
    pub async fn list_conversations(&self) -> Result<Vec<Conversation>> {
        let rows = self
            .query(
                &format!(
                    "SELECT {CONV_COLS} FROM conversations \
                     ORDER BY (pinned_at IS NOT NULL) DESC, pinned_at DESC NULLS LAST, updated_at DESC, id DESC"
                ),
                &[],
            )
            .await?;
        Ok(rows.iter().map(scan_conv).collect())
    }

    /// `GetConversation`: one thread (None if absent).
    pub async fn get_conversation(&self, id: i64) -> Result<Option<Conversation>> {
        let row = self
            .query_opt(
                &format!("SELECT {CONV_COLS} FROM conversations WHERE id=$1"),
                &[&id],
            )
            .await?;
        Ok(row.as_ref().map(scan_conv))
    }

    /// `UpdateConversation`: partial title/pin/target-gate mutation; returns the
    /// updated row (None if absent). Pinning an already-pinned row preserves its
    /// original `pinned_at` (so pin order is stable).
    pub async fn update_conversation(
        &self,
        id: i64,
        patch: ConversationPatch,
    ) -> Result<Option<Conversation>> {
        let row = self
            .query_opt(
                &format!(
                    "UPDATE conversations SET \
                     title = CASE WHEN $2::boolean THEN $3 ELSE title END, \
                     pinned_at = CASE \
                         WHEN $4::boolean IS NULL THEN pinned_at \
                         WHEN $4::boolean THEN COALESCE(pinned_at, now()) \
                         ELSE NULL END, \
                     target_gate = CASE WHEN $5::boolean THEN $6 ELSE target_gate END \
                     WHERE id=$1 RETURNING {CONV_COLS}"
                ),
                &[
                    &id,
                    &patch.title.is_some(),
                    &patch.title.as_deref(),
                    &patch.pinned,
                    &patch.target_gate.is_some(),
                    &patch.target_gate,
                ],
            )
            .await?;
        Ok(row.as_ref().map(scan_conv))
    }

    /// `RenameConversation`: set a thread's title (compatibility helper).
    pub async fn rename_conversation(&self, id: i64, title: &str) -> Result<()> {
        let _ = self
            .update_conversation(
                id,
                ConversationPatch {
                    title: Some(title.to_string()),
                    ..ConversationPatch::default()
                },
            )
            .await?;
        Ok(())
    }

    /// `TouchConversation`: bump `updated_at` so the thread floats to the top.
    pub async fn touch_conversation(&self, id: i64) -> Result<()> {
        self.execute(
            "UPDATE conversations SET updated_at=now() WHERE id=$1",
            &[&id],
        )
        .await?;
        Ok(())
    }

    /// `DeleteConversation`: remove a thread; activities cascade, target pins are
    /// revoked in the same tx (and stay as history).
    pub async fn delete_conversation(&self, id: i64) -> Result<()> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let result = async {
            revoke_conversation_pins(&tx, &[id]).await?;
            tx.execute("DELETE FROM conversations WHERE id=$1", &[&id])
                .await?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                tx.commit().await?;
                Ok(())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `DeleteConversations`: remove existing threads in one statement, returning
    /// the ids that were actually present; revoke their target pins in the same tx.
    pub async fn delete_conversations(&self, ids: &[i64]) -> Result<Vec<i64>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let result = async {
            let rows = tx
                .query(
                    "DELETE FROM conversations WHERE id=ANY($1::bigint[]) RETURNING id",
                    &[&ids],
                )
                .await?;
            let deleted: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
            revoke_conversation_pins(&tx, &deleted).await?;
            Ok::<_, crate::db::error::Error>(deleted)
        }
        .await;
        match result {
            Ok(deleted) => {
                tx.commit().await?;
                Ok(deleted)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `AppendConvActivity`: record one step of a conversation; returns its id.
    pub async fn append_conv_activity(&self, conv_id: i64, a: Activity) -> Result<i64> {
        let row = self
            .query_one(
                "INSERT INTO conversation_activities(conversation_id, worker, kind, tool, tool_use_id, \
                 is_error, summary, detail, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) \
                 VALUES ($1,NULLIF($2,''),NULLIF($3,''),NULLIF($4,''),NULLIF($5,''),$6,NULLIF($7,''),NULLIF($8,''),$9,$10,$11,$12) \
                 RETURNING id",
                &[
                    &conv_id,
                    &utf8_clean(&a.worker),
                    &utf8_clean(&a.kind),
                    &utf8_clean(&a.tool),
                    &utf8_clean(&a.tool_use_id),
                    &a.is_error,
                    &utf8_clean(&a.summary),
                    &utf8_clean(&a.detail),
                    &a.input_tokens,
                    &a.output_tokens,
                    &a.cache_read_tokens,
                    &a.cache_write_tokens,
                ],
            )
            .await?;
        Ok(row.get(0))
    }

    /// `ConvActivityList`: steps after `since_id` (exclusive) with the summary-only
    /// column set; returns `(items, new_cursor)`. `limit <= 0` -> 500.
    pub async fn conv_activity_list(
        &self,
        conv_id: i64,
        since_id: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, i64)> {
        let limit = if limit <= 0 { 500 } else { limit } as i64;
        let rows = self
            .query(
                &format!(
                    "SELECT {CONV_ACT_COLS} FROM conversation_activities \
                     WHERE conversation_id=$1 AND id>$2 ORDER BY id LIMIT $3"
                ),
                &[&conv_id, &since_id, &limit],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        let mut cursor = since_id;
        for r in &rows {
            let a = scan_conv_activity(r);
            if a.id > cursor {
                cursor = a.id;
            }
            out.push(a);
        }
        Ok((out, cursor))
    }

    /// `ConvActivityPage`: one reverse-paginated window ending before `before`
    /// (exclusive; `<=0` = latest), returned in ascending id order. `has_more`
    /// reports whether older history remains before the window. `limit <= 0` -> 200.
    pub async fn conv_activity_page(
        &self,
        conv_id: i64,
        before: i64,
        limit: i32,
    ) -> Result<(Vec<Activity>, bool)> {
        let limit = if limit <= 0 { 200 } else { limit } as i64;
        let fetch = limit + 1;
        let rows = self
            .query(
                &format!(
                    "SELECT {CONV_ACT_COLS} FROM conversation_activities \
                     WHERE conversation_id=$1 AND ($2 <= 0::bigint OR id < $2) \
                     ORDER BY id DESC LIMIT $3"
                ),
                &[&conv_id, &before, &fetch],
            )
            .await?;
        let mut desc: Vec<Activity> = rows.iter().map(scan_conv_activity).collect();
        let has_more = desc.len() as i64 > limit;
        if has_more {
            desc.truncate(limit as usize);
        }
        let out: Vec<Activity> = desc.into_iter().rev().collect();
        Ok((out, has_more))
    }

    /// `ConvActivityDetail`: lazily return the full `detail` blob of one step
    /// ("" when absent or NULL).
    pub async fn conv_activity_detail(&self, conv_id: i64, id: i64) -> Result<String> {
        let row = self
            .query_opt(
                "SELECT detail FROM conversation_activities WHERE id=$1 AND conversation_id=$2",
                &[&id, &conv_id],
            )
            .await?;
        Ok(row
            .and_then(|r| r.get::<_, Option<String>>(0))
            .unwrap_or_default())
    }
}
