//! LLM profile CRUD — slice of `db/config.go` (profiles only).
//!
//! API keys + proxy are sealed at rest; readers open them. The failover pool
//! order is policy: active first, then priority desc (id asc), excluding keyless
//! and `pool_exclude` profiles.

use crate::db::error::{Error, Result};
use crate::db::llmretry::{RetryOverride, RetryRule};
use crate::db::secrets::{open_secret, seal_secret, COL_LLM_KEY, COL_LLM_PROXY};
use crate::db::DB;

pub const ERR_ACTIVE_LLM_PROFILE_DELETE: &str =
    "cannot delete the active LLM profile; activate another profile first";
pub const ERR_LLM_PROFILE_NOT_FOUND: &str = "LLM profile not found";

/// `LLMProfile`: one recorded LLM endpoint configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LLMProfile {
    pub id: i64,
    pub name: String,
    pub format: String,
    pub base_url: String,
    pub proxy: String,
    pub model: String,
    /// Never serialized to UI.
    pub api_key: String,
    pub api_key_hint: String,
    pub rate_per_second: f64,
    pub rate_per_minute: f64,
    pub context_window_k: i32,
    pub thinking_type: String,
    pub reasoning_effort: String,
    pub is_default: bool,
    pub priority: i32,
    pub pool_exclude: bool,
    pub streaming: bool,
    pub max_tokens: i32,
    pub max_tokens_field: String,
    pub session_header_key: String,
    pub user_agent: String,
    pub retry: RetryOverride,
    pub price_input_per_m: f64,
    pub price_output_per_m: f64,
    pub price_cache_read_per_m: f64,
    pub price_cache_write_per_m: f64,
}

const PROFILE_RETRY_COLS: &str = "COALESCE(retry_connect_attempts,0),COALESCE(retry_connect_interval_ms,0),COALESCE(retry_empty_attempts,0),COALESCE(retry_empty_interval_ms,0),COALESCE(retry_stream_attempts,0),COALESCE(retry_stream_interval_ms,0)";
const PROFILE_COLS_HINT: &str = "id,name,format,COALESCE(base_url,''),COALESCE(proxy,''),model,COALESCE(api_key_hint,''),rate_per_second,rate_per_minute,context_window_k,COALESCE(reasoning_effort,''),is_default,priority,pool_exclude,COALESCE(thinking_type,''),COALESCE(streaming,true),COALESCE(max_tokens,0),COALESCE(max_tokens_field,''),COALESCE(session_header_key,''),";
const PROFILE_COLS_KEY: &str = "id,name,format,COALESCE(base_url,''),COALESCE(proxy,''),model,COALESCE(api_key,''),rate_per_second,rate_per_minute,context_window_k,COALESCE(reasoning_effort,''),is_default,priority,pool_exclude,COALESCE(thinking_type,''),COALESCE(streaming,true),COALESCE(max_tokens,0),COALESCE(max_tokens_field,''),COALESCE(session_header_key,''),";

fn profile_cols(key: bool) -> String {
    format!(
        "{}{},COALESCE(price_input_per_m,0)::float8,COALESCE(price_output_per_m,0)::float8,COALESCE(price_cache_read_per_m,0)::float8,COALESCE(price_cache_write_per_m,0)::float8,COALESCE(user_agent,'')",
        if key { PROFILE_COLS_KEY } else { PROFILE_COLS_HINT },
        PROFILE_RETRY_COLS
    )
}

/// `scanProfile`: read one profile row. The 7th column is the key when `key=true`,
/// the key hint otherwise; the proxy/hint are opened as secrets.
fn scan_profile(row: &tokio_postgres::Row, key: bool) -> Result<LLMProfile> {
    let proxy_raw: String = row.get(4);
    let proxy = open_secret(&COL_LLM_PROXY, &proxy_raw).map_err(Error::msg)?;
    let seventh: String = row.get(6);
    let (api_key, api_key_hint) = if key {
        (
            open_secret(&COL_LLM_KEY, &seventh).map_err(Error::msg)?,
            String::new(),
        )
    } else {
        (String::new(), seventh)
    };
    Ok(LLMProfile {
        id: row.get(0),
        name: row.get(1),
        format: row.get(2),
        base_url: row.get(3),
        proxy,
        model: row.get(5),
        api_key,
        api_key_hint,
        rate_per_second: row.get(7),
        rate_per_minute: row.get(8),
        context_window_k: row.get(9),
        reasoning_effort: row.get(10),
        is_default: row.get(11),
        priority: row.get(12),
        pool_exclude: row.get(13),
        thinking_type: row.get(14),
        streaming: row.get(15),
        max_tokens: row.get(16),
        max_tokens_field: row.get(17),
        session_header_key: row.get(18),
        retry: RetryOverride {
            connect: RetryRule {
                attempts: row.get(19),
                interval_ms: row.get(20),
            },
            empty: RetryRule {
                attempts: row.get(21),
                interval_ms: row.get(22),
            },
            stream: RetryRule {
                attempts: row.get(23),
                interval_ms: row.get(24),
            },
        },
        price_input_per_m: row.get(25),
        price_output_per_m: row.get(26),
        price_cache_read_per_m: row.get(27),
        price_cache_write_per_m: row.get(28),
        user_agent: row.get(29),
    })
}

impl DB {
    /// `ListProfiles`: all profiles by id (key hidden, hint shown).
    pub async fn list_profiles(&self) -> Result<Vec<LLMProfile>> {
        let rows = self
            .query(
                &format!(
                    "SELECT {} FROM llm_profiles ORDER BY id",
                    profile_cols(false)
                ),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_profile(r, false)?);
        }
        Ok(out)
    }

    /// `ActiveProfile`: the default profile with its API key (None when absent).
    pub async fn active_profile(&self) -> Result<Option<LLMProfile>> {
        let row = self
            .query_opt(
                &format!(
                    "SELECT {} FROM llm_profiles WHERE is_default LIMIT 1",
                    profile_cols(true)
                ),
                &[],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(scan_profile(&r, true)?)),
            None => Ok(None),
        }
    }

    /// `ProfileByID`: one profile with its API key (None when absent).
    pub async fn profile_by_id(&self, id: i64) -> Result<Option<LLMProfile>> {
        let row = self
            .query_opt(
                &format!(
                    "SELECT {} FROM llm_profiles WHERE id=$1",
                    profile_cols(true)
                ),
                &[&id],
            )
            .await?;
        match row {
            Some(r) => Ok(Some(scan_profile(&r, true)?)),
            None => Ok(None),
        }
    }

    /// `PoolProfiles`: failover chain in run order, keys included.
    pub async fn pool_profiles(&self) -> Result<Vec<LLMProfile>> {
        let rows = self
            .query(
                &format!(
                    "SELECT {} FROM llm_profiles \
                     WHERE (COALESCE(api_key,'') <> '' OR format='ollama') AND (is_default OR NOT pool_exclude) \
                     ORDER BY is_default DESC, priority DESC, id ASC",
                    profile_cols(true)
                ),
                &[],
            )
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(scan_profile(r, true)?);
        }
        Ok(out)
    }

    /// `SaveProfile`: insert (id==0) or update. Empty API key on update keeps existing.
    pub async fn save_profile(&self, p: &mut LLMProfile) -> Result<i64> {
        p.api_key = p.api_key.trim().to_string();
        let hint = if p.api_key.len() >= 4 {
            format!("…{}", &p.api_key[p.api_key.len() - 4..])
        } else {
            p.api_key_hint.clone()
        };
        let key = seal_secret(&COL_LLM_KEY, &p.api_key).map_err(Error::msg)?;
        let proxy = seal_secret(&COL_LLM_PROXY, &p.proxy).map_err(Error::msg)?;
        let r = p.retry.clamped();

        let id = if p.id == 0 {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 25] = [
                &p.name,
                &p.format,
                &nn(&p.base_url),
                &nn(&proxy),
                &p.model,
                &nn(&key),
                &nn(&hint),
                &p.rate_per_second,
                &p.rate_per_minute,
                &p.context_window_k,
                &p.reasoning_effort,
                &p.priority,
                &p.pool_exclude,
                &p.thinking_type,
                &p.streaming,
                &p.max_tokens,
                &p.max_tokens_field,
                &p.session_header_key,
                &r.connect.attempts,
                &r.connect.interval_ms,
                &r.empty.attempts,
                &r.empty.interval_ms,
                &r.stream.attempts,
                &r.stream.interval_ms,
                &p.user_agent,
            ];
            let row = self.query_one(
                "INSERT INTO llm_profiles(name,format,base_url,proxy,model,api_key,api_key_hint,rate_per_second,rate_per_minute,context_window_k,reasoning_effort,priority,pool_exclude,thinking_type,streaming,max_tokens,max_tokens_field,session_header_key,retry_connect_attempts,retry_connect_interval_ms,retry_empty_attempts,retry_empty_interval_ms,retry_stream_attempts,retry_stream_interval_ms,user_agent) \
                 VALUES ($1,$2,NULLIF($3,''),NULLIF($4,''),$5,NULLIF($6,''),NULLIF($7,''),$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25) RETURNING id",
                &params,
            ).await?;
            row.get(0)
        } else if p.api_key.is_empty() {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 24] = [
                &p.name,
                &p.format,
                &nn(&p.base_url),
                &nn(&proxy),
                &p.model,
                &p.rate_per_second,
                &p.rate_per_minute,
                &p.context_window_k,
                &p.reasoning_effort,
                &p.priority,
                &p.pool_exclude,
                &p.thinking_type,
                &p.streaming,
                &p.max_tokens,
                &p.max_tokens_field,
                &p.session_header_key,
                &r.connect.attempts,
                &r.connect.interval_ms,
                &r.empty.attempts,
                &r.empty.interval_ms,
                &r.stream.attempts,
                &r.stream.interval_ms,
                &p.user_agent,
                &p.id,
            ];
            self.execute(
                "UPDATE llm_profiles SET name=$1,format=$2,base_url=NULLIF($3,''),proxy=NULLIF($4,''),model=$5,rate_per_second=$6,rate_per_minute=$7,context_window_k=$8,reasoning_effort=$9,priority=$10,pool_exclude=$11,thinking_type=$12,streaming=$13,max_tokens=$14,max_tokens_field=$15,session_header_key=$16,retry_connect_attempts=$17,retry_connect_interval_ms=$18,retry_empty_attempts=$19,retry_empty_interval_ms=$20,retry_stream_attempts=$21,retry_stream_interval_ms=$22,user_agent=$23 WHERE id=$24",
                &params,
            ).await?;
            p.id
        } else {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 26] = [
                &p.name,
                &p.format,
                &nn(&p.base_url),
                &nn(&proxy),
                &p.model,
                &nn(&key),
                &nn(&hint),
                &p.rate_per_second,
                &p.rate_per_minute,
                &p.context_window_k,
                &p.reasoning_effort,
                &p.priority,
                &p.pool_exclude,
                &p.thinking_type,
                &p.streaming,
                &p.max_tokens,
                &p.max_tokens_field,
                &p.session_header_key,
                &r.connect.attempts,
                &r.connect.interval_ms,
                &r.empty.attempts,
                &r.empty.interval_ms,
                &r.stream.attempts,
                &r.stream.interval_ms,
                &p.user_agent,
                &p.id,
            ];
            self.execute(
                "UPDATE llm_profiles SET name=$1,format=$2,base_url=NULLIF($3,''),proxy=NULLIF($4,''),model=$5,api_key=$6,api_key_hint=$7,rate_per_second=$8,rate_per_minute=$9,context_window_k=$10,reasoning_effort=$11,priority=$12,pool_exclude=$13,thinking_type=$14,streaming=$15,max_tokens=$16,max_tokens_field=$17,session_header_key=$18,retry_connect_attempts=$19,retry_connect_interval_ms=$20,retry_empty_attempts=$21,retry_empty_interval_ms=$22,retry_stream_attempts=$23,retry_stream_interval_ms=$24,user_agent=$25 WHERE id=$26",
                &params,
            ).await?;
            p.id
        };
        self.save_profile_prices(id, p).await?;
        Ok(id)
    }

    async fn save_profile_prices(&self, id: i64, p: &LLMProfile) -> Result<()> {
        self.execute(
            "UPDATE llm_profiles SET price_input_per_m=($1::float8),price_output_per_m=($2::float8),price_cache_read_per_m=($3::float8),price_cache_write_per_m=($4::float8) WHERE id=$5",
            &[&p.price_input_per_m, &p.price_output_per_m, &p.price_cache_read_per_m, &p.price_cache_write_per_m, &id],
        ).await?;
        Ok(())
    }
}

fn nn(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
