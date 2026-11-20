//! Finding retests — port of `db/finding_retests.go`.
//!
//! A retest is an immutable historical verification run bound to one chat
//! conversation. It is created atomically (finding row lock → dedupe → snapshot
//! → conversation → first message) and sealed by `finish_finding_retest`.

use std::time::SystemTime;

use crate::db::conversation::{scan_conv, CONV_COLS};
use crate::db::error::{Error, Result};
use crate::db::DB;
use crate::llm::Context;

pub const FINDING_RETEST_AGENT_KEY: &str = "retester";
const RETEST_NOTE_SUMMARY_RUNES: usize = 200;

pub const ERR_RETEST_NOT_RUNNING: &str =
    "this retest has ended or has not started; please start a new retest from the finding details";

/// `FindingRetest`: one immutable historical verification attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRetest {
    pub id: i64,
    pub finding_id: i64,
    pub conversation_id: Option<i64>,
    pub status: String,
    pub verdict: String,
    pub notes: String,
    /// Only loaded for the agent — never sent with the history list.
    pub snapshot: Option<serde_json::Value>,
    pub summary: String,
    pub evidence: String,
    pub error: String,
    pub created_at: SystemTime,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
}

impl Default for FindingRetest {
    fn default() -> Self {
        FindingRetest {
            id: 0,
            finding_id: 0,
            conversation_id: None,
            status: String::new(),
            verdict: String::new(),
            notes: String::new(),
            snapshot: None,
            summary: String::new(),
            evidence: String::new(),
            error: String::new(),
            created_at: SystemTime::UNIX_EPOCH,
            started_at: None,
            finished_at: None,
        }
    }
}

/// `ActiveFindingRetest`: small status payload polled by the findings list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActiveFindingRetest {
    pub id: i64,
    pub finding_id: i64,
    pub conversation_id: i64,
    pub status: String,
}

const RETEST_COLS: &str = "id, finding_id, conversation_id, status, verdict, notes, summary, evidence, error, created_at, started_at, finished_at";

/// `truncateRunes`: trim + cap at `n` runes with an ellipsis when cut.
fn truncate_runes(s: &str, n: usize) -> String {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        format!("{}…", chars[..n].iter().collect::<String>())
    }
}

fn scan_retest(row: &tokio_postgres::Row) -> FindingRetest {
    FindingRetest {
        id: row.get(0),
        finding_id: row.get(1),
        conversation_id: row.get(2),
        status: row.get(3),
        verdict: row.get(4),
        notes: row.get(5),
        summary: row.get(6),
        evidence: row.get(7),
        error: row.get(8),
        created_at: row.get(9),
        started_at: row.get(10),
        finished_at: row.get(11),
        snapshot: None,
    }
}

impl FindingRetest {
    /// `InitialMessage`: the first user message that starts the retest agent.
    pub fn initial_message(&self) -> String {
        let mut msg = format!(
            "Please retest vulnerability #{}. First call get_finding_retest_context to read the original evidence and constraints associated with this conversation, then perform targeted verification, and finally call record_finding_retest_result to save the conclusion.",
            self.finding_id
        );
        if !self.notes.is_empty() {
            msg.push_str("\n\nAdditional notes for this retest:\n");
            msg.push_str(&self.notes);
        }
        msg
    }
}

fn check_ctx(ctx: &Context) -> Result<()> {
    if let Some(e) = ctx.err() {
        return Err(e.into());
    }
    Ok(())
}

impl DB {
    /// `ListActiveFindingRetests`: pending/running retests with a conversation.
    pub async fn list_active_finding_retests(
        &self,
        ctx: &Context,
    ) -> Result<Vec<ActiveFindingRetest>> {
        check_ctx(ctx)?;
        let rows = self
            .query(
                "SELECT id, finding_id, conversation_id, status FROM finding_retests \
                 WHERE status IN ('pending','running') AND conversation_id IS NOT NULL ORDER BY id",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| ActiveFindingRetest {
                id: r.get(0),
                finding_id: r.get(1),
                conversation_id: r.get(2),
                status: r.get(3),
            })
            .collect())
    }

    /// `CreateFindingRetest`: atomically snapshot the source, create its conversation
    /// and persist the first message. A finding row lock deduplicates simultaneous
    /// clicks; an existing active run is returned without dispatching.
    pub async fn create_finding_retest(
        &self,
        ctx: &Context,
        finding_id: i64,
        notes: &str,
    ) -> Result<(
        FindingRetest,
        Option<crate::db::conversation::Conversation>,
        bool,
    )> {
        check_ctx(ctx)?;
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let row = tx
                .query_one(
                    "SELECT COALESCE(NULLIF(f.name,''), NULLIF(f.vulnclass,''), 'Uncategorized'), \
                     jsonb_build_object('finding', to_jsonb(f), \
                       'assets', COALESCE((SELECT jsonb_agg(to_jsonb(a)) FROM assets a \
                         WHERE f.asset_ids @> to_jsonb(ARRAY[a.id])), '[]'::jsonb), \
                       'constraints', COALESCE((SELECT jsonb_agg(to_jsonb(c)) FROM task_constraints c \
                         JOIN tasks t ON t.exploration_id=c.exploration_id WHERE t.id=f.task_id), '[]'::jsonb)) \
                     FROM findings f WHERE f.id=$1 FOR UPDATE OF f",
                    &[&finding_id],
                )
                .await?;
            let title: String = row.get(0);
            let snapshot: serde_json::Value = row.get(1);

            // Dedupe: return an existing active run without dispatching.
            let existing = tx
                .query_opt(
                    &format!(
                        "SELECT {RETEST_COLS} FROM finding_retests \
                         WHERE finding_id=$1 AND status IN ('pending','running')"
                    ),
                    &[&finding_id],
                )
                .await?;
            if let Some(r) = existing {
                return Ok::<_, Error>((scan_retest(&r), None, false));
            }

            // Title capped at 100 runes, same as ordinary conversations.
            let title: String = title.chars().take(100).collect();
            // The retest conversation takes the finding's task target-gate opt-in.
            let conv_row = tx
                .query_one(
                    &format!(
                        "INSERT INTO conversations(agent_key,title,target_gate) VALUES ($1,$2, \
                         COALESCE((SELECT t.target_gate FROM findings f JOIN tasks t ON t.id=f.task_id \
                         WHERE f.id=$3), false)) RETURNING {CONV_COLS}"
                    ),
                    &[
                        &FINDING_RETEST_AGENT_KEY,
                        &format!("Retest #{finding_id} · {title}"),
                        &finding_id,
                    ],
                )
                .await?;
            let conv = scan_conv(&conv_row);

            let retest_row = tx
                .query_one(
                    &format!(
                        "INSERT INTO finding_retests(finding_id,conversation_id,notes,snapshot) \
                         VALUES ($1,$2,$3,$4) RETURNING {RETEST_COLS}"
                    ),
                    &[&finding_id, &conv.id, &notes.trim(), &snapshot],
                )
                .await?;
            let retest = scan_retest(&retest_row);
            tx.execute(
                "INSERT INTO conversation_activities(conversation_id,worker,kind,summary,detail) \
                 VALUES ($1,$2,'user',$3,$4)",
                &[
                    &conv.id,
                    &FINDING_RETEST_AGENT_KEY,
                    &format!("Please retest vulnerability #{finding_id}"),
                    &retest.initial_message(),
                ],
            )
            .await?;
            Ok((retest, Some(conv), true))
        }
        .await;
        match run {
            Ok(v) => {
                tx.commit().await?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `ListFindingRetests`: a finding's retests, newest first (snapshot omitted).
    pub async fn list_finding_retests(&self, finding_id: i64) -> Result<Vec<FindingRetest>> {
        let rows = self
            .query(
                &format!(
                    "SELECT {RETEST_COLS} FROM finding_retests WHERE finding_id=$1 ORDER BY id DESC"
                ),
                &[&finding_id],
            )
            .await?;
        Ok(rows.iter().map(scan_retest).collect())
    }

    /// `FindingRetestForConversation`: one retest by conversation, with its snapshot.
    pub async fn finding_retest_for_conversation(
        &self,
        ctx: &Context,
        conversation_id: i64,
    ) -> Result<Option<FindingRetest>> {
        check_ctx(ctx)?;
        let row = self
            .query_opt(
                &format!("SELECT {RETEST_COLS} FROM finding_retests WHERE conversation_id=$1"),
                &[&conversation_id],
            )
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut r = scan_retest(&row);
        let snap = self
            .query_one("SELECT snapshot FROM finding_retests WHERE id=$1", &[&r.id])
            .await?;
        r.snapshot = Some(snap.get::<_, serde_json::Value>(0));
        Ok(Some(r))
    }

    /// `FailPendingRetestForConversation`: seal an unfinished retest when the runner
    /// could not load it (only handle is the conversation id).
    pub async fn fail_pending_retest_for_conversation(
        &self,
        conversation_id: i64,
        reason: &str,
    ) -> Result<()> {
        self.execute(
            "UPDATE finding_retests SET status='failed', error=$2, finished_at=now() \
             WHERE conversation_id=$1 AND status IN ('pending','running')",
            &[&conversation_id, &reason],
        )
        .await?;
        Ok(())
    }

    /// `StartFindingRetest`: pending -> running; returns whether a row changed.
    pub async fn start_finding_retest(&self, ctx: &Context, id: i64) -> Result<bool> {
        check_ctx(ctx)?;
        Ok(self
            .execute(
                "UPDATE finding_retests SET status='running', started_at=now() \
                 WHERE id=$1 AND status='pending'",
                &[&id],
            )
            .await?
            > 0)
    }

    /// `RecordFindingRetestResult`: stage a verdict from the owning conversation.
    /// Identical retries are safe; a second differing verdict is refused.
    pub async fn record_finding_retest_result(
        &self,
        ctx: &Context,
        conversation_id: i64,
        verdict: &str,
        summary: &str,
        evidence: &str,
    ) -> Result<()> {
        check_ctx(ctx)?;
        if verdict != "reproduced" && verdict != "fixed" && verdict != "inconclusive" {
            return Err(Error::msg(
                "verdict must be reproduced / fixed / inconclusive",
            ));
        }
        let summary = summary.trim();
        let evidence = evidence.trim();
        if summary.is_empty() || evidence.is_empty() {
            return Err(Error::msg("summary and evidence must not be empty; when the result cannot be confirmed, describe the actual checks and the blocking reason"));
        }
        if summary.len() > 16000 || evidence.len() > 128000 {
            return Err(Error::msg(
                "retest conclusion is too long (summary ≤ 16KB, evidence ≤ 128KB)",
            ));
        }
        let n = self
            .execute(
                "UPDATE finding_retests SET verdict=$2,summary=$3,evidence=$4 \
                 WHERE conversation_id=$1 AND status='running' \
                 AND (verdict='' OR (verdict=$2 AND summary=$3 AND evidence=$4))",
                &[&conversation_id, &verdict, &summary, &evidence],
            )
            .await?;
        if n == 0 {
            return Err(Error::sentinel(
                ERR_RETEST_NOT_RUNNING,
                ERR_RETEST_NOT_RUNNING,
            ));
        }
        Ok(())
    }

    /// `FinishFindingRetest`: seal the result. Cancellation/failure takes precedence
    /// over a staged verdict. A completed fixed verdict also flips finding triage
    /// through the notifying status setter (in the same tx).
    pub async fn finish_finding_retest(&self, id: i64, status: &str, reason: &str) -> Result<()> {
        if status != "completed" && status != "failed" && status != "stopped" {
            return Err(Error::msg("invalid terminal retest status"));
        }
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            let finding_row = tx
                .query_opt(
                    "SELECT f.id FROM findings f \
                     WHERE f.id=(SELECT finding_id FROM finding_retests WHERE id=$1) FOR UPDATE OF f",
                    &[&id],
                )
                .await?;
            let Some(finding_row) = finding_row else {
                return Ok(()); // finding/retest already deleted
            };
            let finding_id: i64 = finding_row.get(0);
            let row = tx
                .query_opt(
                    "UPDATE finding_retests SET \
                     status=CASE WHEN $2='completed' AND verdict='' THEN 'failed' ELSE $2 END, \
                     error=CASE WHEN $2='completed' AND verdict='' THEN 'Agent did not save the retest conclusion; please review the conversation and retest again' ELSE $3 END, \
                     finished_at=now() WHERE id=$1 AND status IN ('pending','running') \
                     RETURNING status,verdict,summary",
                    &[&id, &status, &reason],
                )
                .await?;
            let Some(row) = row else {
                return Ok(()); // replay must not overwrite a later triage decision
            };
            let final_status: String = row.get(0);
            let verdict: String = row.get(1);
            let summary: String = row.get(2);
            if final_status == "completed" && (verdict == "reproduced" || verdict == "fixed") {
                let note = format!(
                    "retest #{id}: {verdict} — {}",
                    truncate_runes(&summary, RETEST_NOTE_SUMMARY_RUNES)
                );
                tx.execute(
                    "UPDATE findings SET verification_status='verified', verification_note=$2 \
                     WHERE id=$1 AND verification_status IN ('unverified','not_assessed','blocked')",
                    &[&finding_id, &note],
                )
                .await?;
            }
            if final_status == "completed" && verdict == "fixed" {
                let ctx = Context::background();
                crate::db::notification::set_finding_status_tx(
                    &ctx,
                    &tx,
                    finding_id,
                    crate::db::findings::FINDING_FIXED,
                )
                .await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        match run {
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

    /// `RecoverFindingRetests`: stop all pending/running retests (server restart).
    pub async fn recover_finding_retests(&self) -> Result<()> {
        self.execute(
            "UPDATE finding_retests SET status='stopped', \
             error='Server restarted; the retest was interrupted, please start it again', \
             finished_at=now() WHERE status IN ('pending','running')",
            &[],
        )
        .await?;
        Ok(())
    }
}
