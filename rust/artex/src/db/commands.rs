//! Command / LLM-record views — port of `db/commands.go`.
//!
//! `CommandRecord` is a paired tool_use + tool_result from the activity table;
//! the LLM-record section is the paginated log of recorded API calls.

use std::time::SystemTime;

use crate::db::error::Result;
use crate::db::findings::FilterParam;
use crate::db::DB;

/// `CommandRecord`: a paired tool_use + tool_result (any tool, not just Bash).
#[derive(Debug, Clone, PartialEq)]
pub struct CommandRecord {
    pub id: i64,
    pub exp_id: i64,
    pub worker: String,
    pub tool: String,
    pub command: String,
    pub output: String,
    pub is_error: bool,
    pub created_at: SystemTime,
}

impl Default for CommandRecord {
    fn default() -> Self {
        CommandRecord {
            id: 0,
            exp_id: 0,
            worker: String::new(),
            tool: String::new(),
            command: String::new(),
            output: String::new(),
            is_error: false,
            created_at: SystemTime::UNIX_EPOCH,
        }
    }
}

/// `ToolStat`: one tool's execution tally for the usage summary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolStat {
    pub tool: String,
    pub total: i64,
    pub errors: i64,
}

/// `LLMRecord`: one recorded LLM API call (request + response).
#[derive(Debug, Clone, PartialEq)]
pub struct LLMRecord {
    pub id: i64,
    /// `ts` is nullable in the DB; NULL maps to the epoch.
    pub ts: SystemTime,
    pub model: String,
    pub profile_name: String,
    pub session_id: String,
    pub task_id: String,
    pub worker: String,
    pub latency_ms: i32,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read: i32,
    pub cache_write: i32,
    pub status: String,
    pub error: String,
    pub request_body: String,
    pub response_body: String,
    pub raw_request: String,
    pub raw_response: String,
}

impl Default for LLMRecord {
    fn default() -> Self {
        LLMRecord {
            id: 0,
            ts: SystemTime::UNIX_EPOCH,
            model: String::new(),
            profile_name: String::new(),
            session_id: String::new(),
            task_id: String::new(),
            worker: String::new(),
            latency_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            cache_write: 0,
            status: String::new(),
            error: String::new(),
            request_body: String::new(),
            response_body: String::new(),
            raw_request: String::new(),
            raw_response: String::new(),
        }
    }
}

/// `ModelTokenStat`: one model's aggregated token usage for a task.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelTokenStat {
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

/// `LLMTask`: one distinct non-empty task_id with its LLM-record count.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LLMTask {
    pub task_id: String,
    pub count: i64,
}

fn null_if_empty(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `commandFilter`: the WHERE clause shared by the command list and its per-tool
/// tally, its params, and the next placeholder index.
fn command_filter(exp_id: Option<i64>, q: &str) -> (String, Vec<FilterParam>, usize) {
    let mut where_sql = String::from("WHERE u.kind = 'tool_use'");
    let mut args = Vec::new();
    let mut n = 1;
    if let Some(id) = exp_id {
        where_sql.push_str(&format!(" AND u.exploration_id = ${n}"));
        args.push(FilterParam::Bigint(id));
        n += 1;
    }
    if !q.is_empty() {
        where_sql.push_str(&format!(" AND (u.tool ILIKE ${n} OR u.detail ILIKE ${n})"));
        args.push(FilterParam::Text(format!("%{q}%")));
        n += 1;
    }
    (where_sql, args, n)
}

fn refs(params: &[FilterParam]) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    params.iter().map(|p| p.as_tosql()).collect()
}

impl DB {
    /// `ToolStats`: count executions grouped by tool under the same filters
    /// `list_commands` takes (unpaginated — the whole filtered set).
    pub async fn tool_stats(&self, exp_id: Option<i64>, q: &str) -> Result<Vec<ToolStat>> {
        let (where_sql, params, _) = command_filter(exp_id, q);
        let sql = format!(
            "SELECT COALESCE(NULLIF(u.tool,''),'-') AS tool, COUNT(*) AS total, \
             COUNT(*) FILTER (WHERE COALESCE(r.is_error,false)) AS errors \
             FROM activity u \
             LEFT JOIN activity r ON r.tool_use_id = u.tool_use_id AND r.kind = 'tool_result' \
             {where_sql} GROUP BY 1 ORDER BY total DESC, tool ASC"
        );
        let rows = self.query(&sql, &refs(&params)).await?;
        Ok(rows
            .iter()
            .map(|r| ToolStat {
                tool: r.get(0),
                total: r.get(1),
                errors: r.get(2),
            })
            .collect())
    }

    /// `ListCommands`: paginated tool executions across explorations. `size <= 0`
    /// -> 50; `page < 0` -> 0. Returns `(records, total)`.
    pub async fn list_commands(
        &self,
        exp_id: Option<i64>,
        q: &str,
        page: i32,
        size: i32,
    ) -> Result<(Vec<CommandRecord>, i64)> {
        let size = if size <= 0 { 50 } else { size };
        let page = if page < 0 { 0 } else { page };
        let offset = page as i64 * size as i64;
        let (where_sql, params, n) = command_filter(exp_id, q);

        let count_sql = format!("SELECT COUNT(*) FROM activity u {where_sql}");
        let total: i64 = self.query_one(&count_sql, &refs(&params)).await?.get(0);

        let limit = size as i64;
        let mut all_refs = refs(&params);
        all_refs.push(&limit);
        all_refs.push(&offset);
        let data_sql = format!(
            "SELECT u.id, u.exploration_id, COALESCE(u.worker,''), COALESCE(u.tool,''), \
             COALESCE(u.detail,''), COALESCE(r.detail,''), COALESCE(r.is_error, false), u.created_at \
             FROM activity u \
             LEFT JOIN activity r ON r.tool_use_id = u.tool_use_id AND r.kind = 'tool_result' \
             {where_sql} ORDER BY u.id DESC LIMIT ${} OFFSET ${}",
            n,
            n + 1
        );
        let rows = self.query(&data_sql, &all_refs).await?;
        let out = rows
            .iter()
            .map(|r| CommandRecord {
                id: r.get(0),
                exp_id: r.get(1),
                worker: r.get(2),
                tool: r.get(3),
                command: r.get(4),
                output: r.get(5),
                is_error: r.get(6),
                created_at: r.get(7),
            })
            .collect();
        Ok((out, total))
    }

    /// `InsertLLMRecord`: store one LLM call record. Empty text fields become NULL.
    pub async fn insert_llm_record(&self, r: &LLMRecord) -> Result<()> {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 16] = [
            &r.model,
            &null_if_empty(&r.profile_name),
            &r.session_id,
            &null_if_empty(&r.task_id),
            &null_if_empty(&r.worker),
            &r.latency_ms,
            &r.input_tokens,
            &r.output_tokens,
            &r.cache_read,
            &r.cache_write,
            &r.status,
            &null_if_empty(&r.error),
            &null_if_empty(&r.request_body),
            &null_if_empty(&r.response_body),
            &null_if_empty(&r.raw_request),
            &null_if_empty(&r.raw_response),
        ];
        self.execute(
            "INSERT INTO llm_records(model, profile_name, session_id, task_id, worker, latency_ms, \
             input_tokens, output_tokens, cache_read, cache_write, status, error, request_body, \
             response_body, raw_request, raw_response) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
            &params,
        )
        .await?;
        Ok(())
    }

    /// `ListLLMRecords`: paginated LLM records with optional model/session/task
    /// filters. `size <= 0` -> 50; `page < 0` -> 0. Returns `(records, total)`.
    pub async fn list_llm_records(
        &self,
        model: &str,
        session: &str,
        task: &str,
        page: i32,
        size: i32,
    ) -> Result<(Vec<LLMRecord>, i64)> {
        let size = if size <= 0 { 50 } else { size };
        let page = if page < 0 { 0 } else { page };
        let offset = page as i64 * size as i64;
        let mut where_sql = String::from("WHERE true");
        let mut params: Vec<FilterParam> = Vec::new();
        let mut n = 1;
        if !model.is_empty() {
            where_sql.push_str(&format!(" AND model = ${n}"));
            params.push(FilterParam::Text(model.to_string()));
            n += 1;
        }
        if !session.is_empty() {
            where_sql.push_str(&format!(" AND session_id ILIKE ${n}"));
            params.push(FilterParam::Text(format!("%{session}%")));
            n += 1;
        }
        if !task.is_empty() {
            where_sql.push_str(&format!(" AND COALESCE(task_id,'') = ${n}"));
            params.push(FilterParam::Text(task.to_string()));
            n += 1;
        }
        let count_sql = format!("SELECT COUNT(*) FROM llm_records {where_sql}");
        let total: i64 = self.query_one(&count_sql, &refs(&params)).await?.get(0);

        let limit = size as i64;
        let mut all_refs = refs(&params);
        all_refs.push(&limit);
        all_refs.push(&offset);
        let data_sql = format!(
            "SELECT id, ts, COALESCE(model,''), COALESCE(profile_name,''), COALESCE(session_id,''), \
             COALESCE(task_id,''), COALESCE(worker,''), \
             COALESCE(latency_ms,0), COALESCE(input_tokens,0), COALESCE(output_tokens,0), \
             COALESCE(cache_read,0), COALESCE(cache_write,0), COALESCE(status,''), COALESCE(error,'') \
             FROM llm_records {where_sql} ORDER BY id DESC LIMIT ${} OFFSET ${}",
            n,
            n + 1
        );
        let rows = self.query(&data_sql, &all_refs).await?;
        let out = rows.iter().map(scan_llm_record_short).collect();
        Ok((out, total))
    }

    /// `LLMTasks`: distinct non-empty task_ids with record counts, most recent first.
    pub async fn llm_tasks(&self) -> Result<Vec<LLMTask>> {
        let rows = self
            .query(
                "SELECT task_id, COUNT(*) AS n FROM llm_records \
                 WHERE COALESCE(task_id,'') <> '' GROUP BY task_id ORDER BY MAX(id) DESC",
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| LLMTask {
                task_id: r.get(0),
                count: r.get(1),
            })
            .collect())
    }

    /// `DeleteLLMRecords`: remove every LLM record for one exact task_id; returns
    /// rows deleted.
    pub async fn delete_llm_records(&self, task: &str) -> Result<i64> {
        Ok(self
            .execute(
                "DELETE FROM llm_records WHERE COALESCE(task_id,'') = $1",
                &[&task],
            )
            .await? as i64)
    }

    /// `GetLLMRecord`: one record with full request/response bodies (None if absent).
    pub async fn get_llm_record(&self, id: i64) -> Result<Option<LLMRecord>> {
        let row = self
            .query_opt(
                "SELECT id, ts, COALESCE(model,''), COALESCE(profile_name,''), COALESCE(session_id,''), \
                 COALESCE(task_id,''), COALESCE(worker,''), \
                 COALESCE(latency_ms,0), COALESCE(input_tokens,0), COALESCE(output_tokens,0), \
                 COALESCE(cache_read,0), COALESCE(cache_write,0), COALESCE(status,''), COALESCE(error,''), \
                 request_body, response_body, raw_request, raw_response \
                 FROM llm_records WHERE id=$1",
                &[&id],
            )
            .await?;
        let Some(r) = row else {
            return Ok(None);
        };
        let mut rec = scan_llm_record_short(&r);
        rec.request_body = r.get::<_, Option<String>>(14).unwrap_or_default();
        rec.response_body = r.get::<_, Option<String>>(15).unwrap_or_default();
        rec.raw_request = r.get::<_, Option<String>>(16).unwrap_or_default();
        rec.raw_response = r.get::<_, Option<String>>(17).unwrap_or_default();
        Ok(Some(rec))
    }
}

/// Scan the 14-column short LLM-record set shared by list + get.
fn scan_llm_record_short(r: &tokio_postgres::Row) -> LLMRecord {
    LLMRecord {
        id: r.get(0),
        ts: r
            .get::<_, Option<SystemTime>>(1)
            .unwrap_or(SystemTime::UNIX_EPOCH),
        model: r.get(2),
        profile_name: r.get(3),
        session_id: r.get(4),
        task_id: r.get(5),
        worker: r.get(6),
        latency_ms: r.get(7),
        input_tokens: r.get(8),
        output_tokens: r.get(9),
        cache_read: r.get(10),
        cache_write: r.get(11),
        status: r.get(12),
        error: r.get(13),
        ..LLMRecord::default()
    }
}
