//! `sidequestion/context.go`: bounded context preparation + one overflow recovery.
//!
//! [`SideQuestionService::respond`] (Go `SideQuestionService.Respond`) owns request
//! preparation around the single completion: it loads and folds prior history, compacts
//! an oversized main-agent snapshot into a running summary, and — at most once — retries
//! an answer whose context overflowed the model window. The summary calls are plain
//! completions (no tools, no main transcript, no model-failover chain), capped per
//! request and UTF-8-safe when chunking oversized material.

use sha2::{Digest, Sha256};

use crate::llm::{messages_for_api, user_text, CompletionRequest, Context, Error, Message, Usage};

use super::{
    assemble, clone_request, err_context_budget, estimate_input_tokens, exchange_messages,
    input_budget, is_context_overflow, message_groups, output_budget, snapshot_summary_messages,
    Answer, ContextInfo, ContextOptions, Exchange, Memory, Replay, SideQuestionService, Snapshot,
    UpdateFn, MAX_RECENT_EXCHANGES, SUMMARY_CALL_CAP, SUMMARY_INSTRUCTION,
};

/// `contextBuilder`: the per-request preparation state. Cheap, owned by one
/// [`respond`](SideQuestionService::respond) call; it never escapes the call.
struct ContextBuilder {
    service: SideQuestionService,
    replay: Replay,
    memory: Memory,
    recent: Vec<Exchange>,
    info: ContextInfo,
    usage: Usage,
    calls: i32,
    window: i64,
    on_update: Option<UpdateFn>,
}

impl ContextBuilder {
    /// `contextBuilder.progress`: set the phase and emit a progress update carrying the
    /// usage accumulated so far.
    fn progress(&mut self, phase: &str) {
        self.info.phase = phase.to_string();
        if let Some(u) = &self.on_update {
            u(
                Answer {
                    usage: self.usage,
                    ..Default::default()
                },
                self.info.clone(),
            );
        }
    }

    /// `contextBuilder.save`: persist a memory snapshot, rejecting the write once the
    /// caller's context is cancelled (a clear/delete/cancel during preparation).
    async fn save(&mut self, ctx: &Context, memory: Memory) -> Result<(), Error> {
        if let Some(e) = ctx.err() {
            return Err(e);
        }
        if let Some(save) = &self.replay.save {
            save(ctx.clone(), memory.clone())?;
        }
        self.memory = memory;
        Ok(())
    }

    /// `contextBuilder.summarize`: reduce `text` to a bounded summary, updating `prior`
    /// rolling-style across UTF-8-safe chunks. Every call carries [`SUMMARY_INSTRUCTION`]
    /// as its system prompt and `Thinking: "disabled"`, so it is identified as a summary
    /// call and never counted as an answer.
    async fn summarize(
        &mut self,
        ctx: &Context,
        prior: String,
        text: String,
    ) -> Result<String, Error> {
        let mut prior = prior;
        let mut text = text;
        while !text.is_empty() {
            if let Some(e) = ctx.err() {
                return Err(e);
            }
            if self.calls >= SUMMARY_CALL_CAP {
                return Err(Error::msg(
                    "side question context preparation has reached this request's processing limit; narrow the question scope and retry",
                ));
            }
            let overhead_req = CompletionRequest {
                system: vec![SUMMARY_INSTRUCTION.to_string()],
                messages: vec![user_text(&format!(
                    "[Prior summary]\n{}\n[New material fragment]\n",
                    prior
                ))],
                ..Default::default()
            };
            let overhead = estimate_input_tokens(&overhead_req);
            // chunkBytes = min(32000, window-2048-overhead-512) * 3
            let chunk_bytes = 32000i64.min(self.window - 2048 - overhead - 512) * 3;
            if chunk_bytes < 1024 {
                return Err(err_context_budget());
            }
            // n = min(len(text), chunkBytes), then back up to a UTF-8 rune start.
            let bytes = text.as_bytes();
            let mut n = (bytes.len() as i64).min(chunk_bytes) as usize;
            while n < bytes.len() && (bytes[n] & 0xC0) == 0x80 {
                n -= 1;
            }
            let part = &text[..n];
            let req = CompletionRequest {
                system: vec![SUMMARY_INSTRUCTION.to_string()],
                thinking: "disabled".to_string(),
                max_tokens: 2048,
                messages: vec![user_text(&format!(
                    "[Prior summary]\n{}\n[New material fragment]\n{}",
                    prior, part
                ))],
                ..Default::default()
            };
            if estimate_input_tokens(&req) + req.max_tokens + 512 > self.window {
                return Err(err_context_budget());
            }
            self.calls += 1;
            let (msg, stop, usage, eopt) =
                self.service.provider.complete_full(ctx.clone(), req).await;
            self.usage.add(usage);
            let phase = self.info.phase.clone();
            self.progress(&phase);
            if let Some(e) = ctx.err() {
                return Err(e);
            }
            if let Some(e) = eopt {
                return Err(Error::msg(format!(
                    "side question summarization failed: {}",
                    e.message()
                )));
            }
            let result = msg.text().trim().to_string();
            if result.is_empty()
                || stop == "max_tokens"
                || stop == "length"
                || !msg.tool_uses().is_empty()
            {
                return Err(Error::msg(
                    "side question summary was not fully generated; please retry",
                ));
            }
            let check = CompletionRequest {
                messages: vec![user_text(&result)],
                ..Default::default()
            };
            if estimate_input_tokens(&check) > 2200 {
                return Err(Error::msg(
                    "side question summary was not reduced to fit the budget; please retry",
                ));
            }
            prior = result;
            text = text[n..].to_string();
        }
        Ok(prior)
    }

    /// `contextBuilder.foldHistory`: summarize the oldest `count` recent exchanges into
    /// the running history summary, persist the advanced cursor, and drop them from the
    /// verbatim tail.
    async fn fold_history(&mut self, ctx: &Context, count: usize) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        self.progress("summarizing_history");
        let mut buf = String::new();
        for e in &self.recent[..count] {
            buf.push_str(&format!(
                "\n[Side question record {}, context time {}]\nUser: {}\nAssistant (historical answer): {}\n",
                e.ordinal,
                e.snapshot_at.format_rfc3339(),
                e.question,
                e.answer
            ));
        }
        let prior = self.memory.history.clone();
        let summary = self.summarize(ctx, prior, buf).await?;
        let mut memory = self.memory.clone();
        memory.history = summary;
        memory.through = self.recent[count - 1].ordinal;
        self.save(ctx, memory).await?;
        self.recent.drain(..count);
        Ok(())
    }

    /// `contextBuilder.loadHistory`: page through completed exchanges after the persisted
    /// cursor, folding older-than-`MaxRecentExchanges` history into the summary as it
    /// goes. Rejects a non-monotonic or non-completed cursor.
    async fn load_history(&mut self, ctx: &Context) -> Result<(), Error> {
        let load = match &self.replay.load {
            Some(l) => l.clone(),
            None => return Ok(()),
        };
        let mut after = self.memory.through;
        loop {
            if let Some(e) = ctx.err() {
                return Err(e);
            }
            let page = load(ctx.clone(), after)?;
            if page.is_empty() {
                return Ok(());
            }
            for e in page {
                if e.ordinal <= after || e.status != "completed" {
                    return Err(Error::msg("invalid side question history cursor"));
                }
                after = e.ordinal;
                self.recent.push(e);
            }
            let excess = self.recent.len() as i64 - MAX_RECENT_EXCHANGES as i64;
            self.fold_history(ctx, excess.max(0) as usize).await?;
        }
    }

    /// `contextBuilder.compactSnapshot`: keep the newest whole tool-groups of `base`
    /// within `keepTokens`, summarize the older prefix (cached by `key` + tail start),
    /// and return the summary message prepended to the retained suffix. Always summarizes
    /// at least the oldest group when a provider-reported overflow demands a change.
    async fn compact_snapshot(
        &mut self,
        ctx: &Context,
        base: &[Message],
        keep_tokens: i64,
        key: &str,
    ) -> Result<Vec<Message>, Error> {
        self.progress("compressing_snapshot");
        let groups = message_groups(base);
        let mut start = base.len();
        let mut used: i64 = 0;
        for g in groups.iter().rev() {
            let mreq = CompletionRequest {
                messages: g.clone(),
                ..Default::default()
            };
            let cost = estimate_input_tokens(&mreq);
            if used + cost > keep_tokens {
                break;
            }
            used += cost;
            start -= g.len();
        }
        // A provider-reported overflow must change the request even when the estimate
        // considers everything small enough to keep: summarize the oldest group.
        if start == 0 && !groups.is_empty() {
            start = groups[0].len();
        }
        if start == 0 {
            return Err(err_context_budget());
        }
        if self.memory.snapshot_key == key
            && self.memory.tail_start == start
            && !self.memory.snapshot_summary.is_empty()
        {
            return Ok(snapshot_summary_messages(
                &self.memory.snapshot_summary,
                &base[start..],
            ));
        }
        // Serialize only the prefix being summarized; the retained suffix stays in the
        // structured message representation (including signed thinking).
        let prefix = base[..start].to_vec();
        let data = serde_json::to_string(&prefix).map_err(|e| Error::msg(e.to_string()))?;
        let summary = self.summarize(ctx, String::new(), data).await?;
        let mut memory = self.memory.clone();
        memory.snapshot_key = key.to_string();
        memory.snapshot_summary = summary.clone();
        memory.tail_start = start;
        self.save(ctx, memory).await?;
        Ok(snapshot_summary_messages(&summary, &base[start..]))
    }

    /// `contextBuilder.prepare`: build the request for one attempt. Folds excess history,
    /// drops the oldest exchanges to fit, and — when still oversized — compacts the
    /// snapshot. `force` halves the budgets for the post-overflow retry.
    async fn prepare(
        &mut self,
        ctx: &Context,
        snapshot: &Snapshot,
        question: &str,
        options: ContextOptions,
        force: bool,
    ) -> Result<CompletionRequest, Error> {
        let mut req = clone_request(&snapshot.request)?;
        req.max_tokens = output_budget(&req, options.output_tokens);
        let base = messages_for_api(&req.messages);
        let mut limit = input_budget(snapshot, req.max_tokens);
        if force {
            limit /= 2;
        }
        self.info.input_budget = limit;
        self.info.output_tokens = req.max_tokens;
        // Reserve a bounded history allowance so a long side conversation cannot crowd
        // all primary evidence out. Both count and token limits are enforced.
        let history_limit = 16000i64.min(0.max(limit / 4));
        let mut count = 0usize;
        let mut cost: i64 = 0;
        for i in (0..self.recent.len()).rev() {
            let em = exchange_messages(&self.recent[i]);
            let ereq = CompletionRequest {
                messages: em,
                ..Default::default()
            };
            cost += estimate_input_tokens(&ereq);
            if cost > history_limit {
                count = i + 1;
                break;
            }
        }
        self.fold_history(ctx, count).await?;
        loop {
            let candidate = assemble(&req, &base, &self.memory.history, &self.recent, question);
            let est = estimate_input_tokens(&candidate);
            if est <= limit && !force {
                self.info.recent_exchanges = self.recent.len() as i64;
                self.info.history_summarized = !self.memory.history.is_empty();
                self.info.estimated_input_tokens = est;
                return Ok(candidate);
            }
            if force || self.recent.len() <= 2 {
                break;
            }
            let fold = (self.recent.len() / 2).max(1);
            self.fold_history(ctx, fold).await?;
        }
        let static_req = assemble(&req, &[], &self.memory.history, &self.recent, question);
        let available = limit - estimate_input_tokens(&static_req) - 2400;
        if available < 0 {
            return Err(err_context_budget());
        }
        let data = serde_json::to_string(snapshot).map_err(|e| Error::msg(e.to_string()))?;
        let hash = Sha256::digest(data.as_bytes());
        let mut key = String::with_capacity(hash.len() * 2);
        for b in hash.iter() {
            key.push_str(&format!("{:02x}", b));
        }
        let mut keep_tokens = 8000i64.min(available / 2);
        if force {
            keep_tokens /= 2;
        }
        let compacted = self.compact_snapshot(ctx, &base, keep_tokens, &key).await?;
        req = assemble(
            &req,
            &compacted,
            &self.memory.history,
            &self.recent,
            question,
        );
        if estimate_input_tokens(&req) > limit {
            return Err(err_context_budget());
        }
        self.info.recent_exchanges = self.recent.len() as i64;
        self.info.history_summarized = !self.memory.history.is_empty();
        self.info.snapshot_summarized = true;
        self.info.estimated_input_tokens = estimate_input_tokens(&req);
        Ok(req)
    }
}

impl SideQuestionService {
    /// `SideQuestionService.Respond`: prepare bounded context and answer, with at most
    /// one overflow recovery. Returns the answer, the final [`ContextInfo`], and any
    /// error. `update` (Go `func(Answer, ContextInfo)`) receives progress and streaming
    /// updates; the answer's usage already includes the summary calls' usage.
    pub async fn respond(
        &self,
        ctx: Context,
        snapshot: Snapshot,
        question: &str,
        replay: Replay,
        options: ContextOptions,
        update: Option<UpdateFn>,
    ) -> (Answer, ContextInfo, Option<Error>) {
        let mut window = snapshot.model.window_tokens;
        if window <= 0 {
            window = 200_000;
        }
        // `memory: replay.Memory` — the builder's working memory starts as a copy of
        // the persisted cursor/summaries carried by the replay handle.
        let memory = replay.memory.clone();
        let mut b = ContextBuilder {
            service: self.clone(),
            replay,
            memory,
            recent: Vec::new(),
            info: ContextInfo::default(),
            usage: Usage::default(),
            calls: 0,
            window,
            on_update: update,
        };
        // `defer func() { info = b.info; out.Usage.Add(b.usage) }()`: modeled by running
        // the body to completion, then folding the summary usage into the answer.
        let (mut out, err) = b.run(ctx, &snapshot, question, options).await;
        out.usage.add(b.usage);
        (out, b.info.clone(), err)
    }
}

impl ContextBuilder {
    /// The body of `Respond` after the builder is constructed. Mirrors the Go method's
    /// control flow: prepare, answer, and (once) retry on a context-overflow error.
    async fn run(
        &mut self,
        ctx: Context,
        snapshot: &Snapshot,
        question: &str,
        options: ContextOptions,
    ) -> (Answer, Option<Error>) {
        self.progress("preparing");
        if let Err(e) = self.load_history(&ctx).await {
            return (Answer::default(), Some(e));
        }
        let mut previous_size: i64 = 0;
        let mut attempt = 0;
        while attempt < 2 {
            let req = match self
                .prepare(&ctx, snapshot, question, options, attempt > 0)
                .await
            {
                Ok(r) => r,
                Err(e) => return (Answer::default(), Some(e)),
            };
            let est = estimate_input_tokens(&req);
            if attempt > 0 && est >= previous_size {
                return (
                    Answer::default(),
                    Some(Error::msg(
                        "side question compression could not further reduce the context; retries stopped",
                    )),
                );
            }
            previous_size = est;
            self.progress("answering");

            // Stream/answer with a progress callback that folds the summary usage into
            // each partial answer (Go `a.Usage.Add(b.usage)`).
            let streaming = snapshot.model.streaming;
            let provider_usage = self.usage;
            let info_snapshot = self.info.clone();
            let update = self.on_update.clone();
            let (a, e) = self
                .service
                .answer(
                    ctx.clone(),
                    req,
                    streaming,
                    Some(move |ans: Answer| {
                        let mut ans = ans;
                        ans.usage.add(provider_usage);
                        if let Some(u) = &update {
                            u(ans.clone(), info_snapshot.clone());
                        }
                    }),
                )
                .await;

            let overflow = match &e {
                Some(err) => is_context_overflow(&err.message()),
                None => false,
            };
            if attempt > 0 || !a.text.is_empty() || a.tool_use || !overflow || ctx.err().is_some() {
                return (a, e);
            }
            self.usage.add(a.usage);
            // Retry: a fresh (defaulted) answer, the forced-smaller budget, and a
            // progress tick. The failed attempt's usage is already folded in above.
            self.info.overflow_retried = true;
            self.progress("retrying");
            attempt += 1;
        }
        unreachable!("respond loop returns within two attempts")
    }
}
