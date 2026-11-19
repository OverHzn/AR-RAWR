//! Rust port of the Go `llmpool/` package — LLM failover with a process-wide
//! circuit-breaker registry.
//!
//! This module covers the circuit-breaker **registry** (`Registry`, `State`), the
//! pure failure-classification helpers (`should_failover`, `status_of`,
//! `is_hard_failure`, `trim_err`, the cooldown ladder), and the failover `Pool` —
//! an `llm::Provider` decorator that drives an ordered chain of members, failing
//! over on retryable errors and recording health via the registry. The registry
//! half is fully deterministic (plain error/context inputs); the `Pool` is async
//! (it wraps `llm::Provider` with streaming) and is exercised with scripted fake
//! providers.
//!
//! Faithful to `llmpool/health.go` and `llmpool/pool.go`. Go's
//! `time.Time`/`time.Now` are modeled with `std::time::SystemTime` (wall clock,
//! persistable); `time.Duration` with `std::time::Duration`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::stream::BoxStream;
use futures::StreamExt;
use gocompat::strings::go_trim_space;

use async_trait::async_trait;

use crate::llm::{
    CompletionRequest, ContentBlock, Context, Error, Message, Provider, StreamEvent, Usage,
};

/// The default number of consecutive TRANSIENT failures (429 / 5xx / network) that
/// trip the breaker. Deterministic failures trip on the first regardless. Mirrors
/// `llmpool.softTripAfter` (exported here so tests/spec can reference it).
pub const SOFT_TRIP_AFTER: i64 = 3;

/// The cooling-off ladder, indexed by trip count: the 1st trip cools for a minute,
/// the 2nd for five, everything after that for half an hour. Mirrors `llmpool.backoff`.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
];

/// `llmpool.State`: one profile's circuit-breaker state.
///
/// `open_until` / `last_at` are `Option` to model Go's zero `time.Time` (a zero time
/// is "unset"; `Open()` is false). `open_until = None` == the zero value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Consecutive failures; reset by a success (and folded into a trip).
    pub fails: i64,
    /// Total trips; indexes the backoff ladder.
    pub trips: i64,
    /// Zero / past = closed (usable). `None` models the Go zero time.
    pub open_until: Option<SystemTime>,
    /// The last failure message (trimmed).
    pub last_error: String,
    /// When the last failure was recorded.
    pub last_at: Option<SystemTime>,
}

impl State {
    /// `State.Open`: whether the breaker is currently open (profile should be
    /// skipped). Uses the current wall clock, like Go's `time.Now()`.
    pub fn open(&self) -> bool {
        self.open_at(SystemTime::now())
    }

    /// `State.Open` evaluated against an explicit `now` (for deterministic tests).
    pub fn open_at(&self, now: SystemTime) -> bool {
        matches!(self.open_until, Some(until) if now < until)
    }
}

/// `Registry.tripAfter` (pure): the effective consecutive-transient-failure
/// threshold. `0` means "use the default"; a negative override yields `0`, which
/// `trip` reads as "never soft-trip".
fn trip_after(soft_trip: i64) -> i64 {
    if soft_trip == 0 {
        SOFT_TRIP_AFTER
    } else {
        soft_trip.max(0)
    }
}

/// `Registry.coolFor` (pure): the cooling-off window for the `trips`-th trip
/// (1-based) — the fixed override when set, else the ladder (last rung repeats).
fn cool_for(cooldown: Duration, trips: i64) -> Duration {
    if cooldown > Duration::ZERO {
        return cooldown;
    }
    let idx = (trips - 1).max(0) as usize;
    if idx < BACKOFF.len() {
        BACKOFF[idx]
    } else {
        BACKOFF[BACKOFF.len() - 1]
    }
}

/// `statusOf` + `llmpool.statusRe` (`status (\d{3})`): the leftmost HTTP status
/// carried by an error's text, or 0 if there isn't one.
///
/// Reproduces Go's `regexp.MustCompile(\`status (\d{3})\`).FindStringSubmatch`:
/// scan left to right for the literal `status ` followed by exactly three digits;
/// the first position that matches wins. A `status ` not followed by three digits
/// is skipped and the scan continues (so a later `status NNN` can still match), and
/// `status 5000` yields `500` (the first three digits), exactly as the regexp does.
fn status_of(s: &str) -> u16 {
    let b = s.as_bytes();
    const NEEDLE: &[u8] = b"status ";
    if b.len() < NEEDLE.len() {
        return 0;
    }
    for i in 0..=b.len() - NEEDLE.len() {
        if &b[i..i + NEEDLE.len()] == NEEDLE {
            let d = &b[i + NEEDLE.len()..];
            if d.len() >= 3
                && d[0].is_ascii_digit()
                && d[1].is_ascii_digit()
                && d[2].is_ascii_digit()
            {
                return (d[0] - b'0') as u16 * 100
                    + (d[1] - b'0') as u16 * 10
                    + (d[2] - b'0') as u16;
            }
        }
    }
    0
}

/// `shouldFailover`: whether an error justifies trying the next profile.
///
/// Never fails over on context cancellation (user stop / task timeout) — burning a
/// backup key on it would waste credit and pollute the termination diagnosis. Never
/// on 400 (a malformed or over-long request fails identically everywhere). No
/// status → a transport-level failure (reset / DNS / timeout) → fail over. 401/402/
/// 403/404/408/429 and any 5xx → fail over.
fn should_failover(ctx: &Context, err: Option<&Error>) -> bool {
    let err = match err {
        Some(e) => e,
        None => return false,
    };
    if ctx.err().is_some() || err.is_context_err() {
        return false;
    }
    match status_of(&err.message()) {
        0 => true,    // transport-level
        400 => false, // bad / over-long request
        401 | 402 | 403 | 404 | 408 | 429 => true,
        c if c >= 500 => true,
        _ => false,
    }
}

/// `isHardFailure`: whether the failure is deterministic (the profile will keep
/// failing until a human fixes it). Hard failures open the breaker on the first
/// occurrence.
fn is_hard_failure(err: &Error) -> bool {
    matches!(status_of(&err.message()), 401..=404)
}

/// `trimErr`: shorten an error for logs/UI — provider bodies can be long. Collapses
/// newlines to spaces, trims, and caps at 300 bytes backing off to a UTF-8 rune
/// boundary, appending an ellipsis.
fn trim_err(err: &Error) -> String {
    let replaced = err.message().replace('\n', " ");
    let s = go_trim_space(&replaced);
    if s.len() > 300 {
        let bytes = s.as_bytes();
        let mut cut = 300;
        while cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
            cut -= 1; // back off to a rune start (Go: !utf8.RuneStart)
        }
        format!("{}\u{2026}", &s[..cut])
    } else {
        s.to_string()
    }
}

type PersistFn = Box<dyn Fn(i64, &State) + Send + Sync>;
type ForgetFn = Box<dyn Fn(i64) + Send + Sync>;

struct RegistryInner {
    m: HashMap<i64, State>,
    persist: Option<PersistFn>,
    forget: Option<ForgetFn>,
    /// 0 = use [`SOFT_TRIP_AFTER`]; negative = never soft-trip.
    soft_trip: i64,
    /// Zero = use the [`BACKOFF`] ladder.
    cooldown: Duration,
}

/// `llmpool.Registry`: the circuit-breaker state of every profile, keyed by profile
/// id. Process-wide and outliving any single Pool, so rebuilding the pool never
/// clears what we've learned about which backends are broken.
///
/// The interior is behind a [`Mutex`]; `persist`/`forget` (when set) run **while the
/// lock is held**, exactly as in Go, so a later update to the same profile cannot
/// enqueue ahead of an in-flight one. The callbacks must not call back into the
/// registry or block on I/O indefinitely (they only mirror state to the DB).
pub struct Registry {
    inner: Mutex<RegistryInner>,
}

impl Registry {
    /// `NewRegistry`: build an empty registry. `persist`/`forget` may be `None`
    /// (no DB mirroring).
    pub fn new(persist: Option<PersistFn>, forget: Option<ForgetFn>) -> Self {
        Registry {
            inner: Mutex::new(RegistryInner {
                m: HashMap::new(),
                persist,
                forget,
                soft_trip: 0,
                cooldown: Duration::ZERO,
            }),
        }
    }

    /// `Registry.SetPolicy`: override the breaker's two knobs. `soft_trip`: how many
    /// consecutive transient failures trip it (0 = default; negative = transient
    /// failures never trip, leaving only the deterministic ones). `cooldown`: a fixed
    /// cooling-off window (0 = the ladder).
    pub fn set_policy(&self, soft_trip: i64, cooldown: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.soft_trip = soft_trip;
        inner.cooldown = cooldown;
    }

    /// `Registry.Restore`: seed state loaded from the DB at startup (bypasses
    /// persistence).
    pub fn restore(&self, id: i64, st: State) {
        let mut inner = self.inner.lock().unwrap();
        inner.m.insert(id, st);
    }

    /// `Registry.Get`: a copy of one profile's state (zero `State` if unknown).
    pub fn get(&self, id: i64) -> State {
        let inner = self.inner.lock().unwrap();
        inner.m.get(&id).cloned().unwrap_or_default()
    }

    /// `Registry.IsOpen`: whether a profile is in its cooling-off window.
    pub fn is_open(&self, id: i64) -> bool {
        self.get(id).open()
    }

    /// `Registry.Pass`: a successful call — clear the failure counters so a recovered
    /// profile is fully trusted again (and drop the persisted row).
    pub fn pass(&self, id: i64) {
        let mut inner = self.inner.lock().unwrap();
        let clean = match inner.m.get(&id) {
            None => return, // nothing to write
            Some(st) => st.fails == 0 && st.trips == 0 && st.open_until.is_none(),
        };
        if clean {
            return; // already clean
        }
        inner.m.remove(&id);
        if let Some(forget) = &inner.forget {
            forget(id);
        }
    }

    /// `Registry.Trip`: record a failed call. `hard = true` marks a deterministic
    /// failure (no credit, invalid key, missing model) which opens the breaker
    /// immediately; `hard = false` is a transient one needing `soft_trip` in a row.
    /// Returns whether this call is what opened the breaker (so the caller can log
    /// it once).
    pub fn trip(&self, id: i64, err_msg: &str, hard: bool) -> bool {
        let mut inner = self.inner.lock().unwrap();
        // Read the policy knobs (Copy) before borrowing the map mutably.
        let soft = trip_after(inner.soft_trip);
        let cooldown = inner.cooldown;
        let now = SystemTime::now();

        let (tripped, snapshot) = {
            let st = inner.m.entry(id).or_default();
            st.fails += 1;
            st.last_error = err_msg.to_string();
            st.last_at = Some(now);
            let tripped = hard || (soft > 0 && st.fails >= soft);
            if tripped {
                st.trips += 1;
                st.open_until = Some(now + cool_for(cooldown, st.trips));
                st.fails = 0; // counted into this trip; start fresh for the half-open probe
            }
            (tripped, st.clone())
        };
        if let Some(persist) = &inner.persist {
            persist(id, &snapshot);
        }
        tripped
    }

    /// `Registry.RecordCall`: apply the failover health policy to one provider call.
    ///
    /// - a completed call (`err == None`) clears the state with [`Registry::pass`];
    /// - a failure that does not trigger failover (context cancellation, a
    ///   deterministic 4xx) is ignored on a stream, and clears the state on an atomic
    ///   call;
    /// - a failover-eligible stream failure trips only before the first event
    ///   (`committed == false`); after output reached the caller the state is left
    ///   alone;
    /// - a failover-eligible atomic failure trips.
    ///
    /// `id == 0` is a no-op (no profile to key). Returns whether this call opened the
    /// breaker and the shortened error stored with it.
    pub fn record_call(
        &self,
        ctx: &Context,
        id: i64,
        stream: bool,
        committed: bool,
        err: Option<&Error>,
    ) -> (bool, String) {
        if id == 0 {
            return (false, String::new());
        }
        let err = match err {
            None => {
                self.pass(id);
                return (false, String::new());
            }
            Some(e) => e,
        };
        if !should_failover(ctx, Some(err)) {
            if !stream {
                self.pass(id);
            }
            return (false, String::new());
        }
        if stream && committed {
            return (false, String::new());
        }
        let msg = trim_err(err);
        let tripped = self.trip(id, &msg, is_hard_failure(err));
        (tripped, msg)
    }

    /// `Registry.Reset`: clear one profile's state — the UI's "Recover now" action.
    pub fn reset(&self, id: i64) {
        let mut inner = self.inner.lock().unwrap();
        inner.m.remove(&id);
        if let Some(forget) = &inner.forget {
            forget(id);
        }
    }

    /// `Registry.Snapshot`: a copy of all tracked state, for the status API.
    pub fn snapshot(&self) -> HashMap<i64, State> {
        let inner = self.inner.lock().unwrap();
        inner.m.clone()
    }
}

// === Failover pool (llmpool/pool.go) ===

/// `llmpool.RankActive`: the Rank the caller gives the chain head (the active
/// profile) so it always outranks any configured priority. Mirrors
/// `int(^uint(0)>>1) - 1`.
pub const RANK_ACTIVE: i64 = i64::MAX - 1;

/// `llmpool.ErrExhausted`: returned when every member of the chain failed. The
/// full error is an [`Error::Exhausted`] whose text starts with this.
pub const ERR_EXHAUSTED: &str = "LLM failover: all profiles unavailable";

/// `llmpool.Member`: one LLM profile in the chain, already built into a provider.
/// Members are immutable after construction; all mutable state lives in the shared
/// [`Registry`].
#[derive(Clone)]
pub struct Member {
    /// `llm_profiles.id`.
    pub id: i64,
    /// Profile name, for logs / UI.
    pub name: String,
    pub model: String,
    /// "anthropic" | "openai".
    pub format: String,
    /// The profile's configured priority (display only).
    pub priority: i64,
    /// `is_default` (display only).
    pub active: bool,
    /// The ordering key: higher goes first; members sharing a Rank take turns
    /// leading. The caller encodes "active profile heads the chain" as
    /// [`RANK_ACTIVE`], above every user-settable priority.
    pub rank: i64,
    /// The profile's context window in tokens. A member whose window can't hold the
    /// request is skipped rather than made to fail on it.
    pub window_tokens: i64,
    pub prov: Arc<dyn Provider>,
}

/// `llmpool.Pool`: an [`Provider`] that fails over across an ordered chain of
/// members. Safe for concurrent use: members are immutable after construction and
/// all mutable state lives in the shared [`Registry`].
pub struct Pool {
    members: Vec<Member>,
    health: Arc<Registry>,
    /// Rotates the starting point within an equal-priority group.
    rr: AtomicU64,
}

impl Pool {
    /// `llmpool.New`: build a Pool over `members` (already in chain order). Returns
    /// `None` when the chain is empty (callers then use the bare provider). A
    /// single-member chain is still a valid Pool.
    pub fn new(members: Vec<Member>, health: Option<Arc<Registry>>) -> Option<Pool> {
        if members.is_empty() {
            return None;
        }
        let health = health.unwrap_or_else(|| Arc::new(Registry::new(None, None)));
        Some(Pool {
            members,
            health,
            rr: AtomicU64::new(0),
        })
    }

    /// `Pool.Members`: the chain in order (read-only).
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// `Pool.Head`: the first member of the chain.
    pub fn head(&self) -> &Member {
        &self.members[0]
    }

    /// `Pool.order`: pick the members to try, in order — skip those in a
    /// cooling-off window and those whose context window can't hold this request,
    /// then rotate within each equal-priority group. Never empty: if everything is
    /// filtered out, the head is tried anyway (stalling the engine is worse than one
    /// more failed request).
    ///
    /// Exposed so callers (and tests) can introspect the chain the pool would drive.
    pub fn order(&self, req: &CompletionRequest) -> Vec<Member> {
        let est = estimate_tokens(req) as i64;
        let mut open: Vec<Member> = Vec::new();
        for m in &self.members {
            if self.health.is_open(m.id) {
                continue;
            }
            if m.window_tokens > 0 && est > m.window_tokens {
                continue; // would 400 on length — not a useful failover target
            }
            open.push(m.clone());
        }
        if open.is_empty() {
            return self.members[..1].to_vec(); // last resort: probe the head
        }
        // Go: rotateGroups(open, p.rr.Add(1)-1); fetch_add returns the pre-add value,
        // which equals Go's Add(1)-1.
        rotate_groups(open, self.rr.fetch_add(1, Ordering::SeqCst))
    }
}

/// The chain-exhaustion error: `fmt.Errorf("%w: %v", ErrExhausted, last)`.
fn exhausted_error(last_err: Option<Error>) -> Error {
    let last = last_err
        .map(|e| e.message())
        .unwrap_or_else(|| ERR_EXHAUSTED.to_string());
    Error::Exhausted(format!("{ERR_EXHAUSTED}: {last}"))
}

/// `rotateGroups`: rotate each run of equal-ranked members by `n`, so profiles
/// sharing a priority take turns going first. The chain head has its own Rank and
/// always stays at the front.
fn rotate_groups(inp: Vec<Member>, n: u64) -> Vec<Member> {
    let mut out: Vec<Member> = Vec::with_capacity(inp.len());
    let mut i = 0;
    while i < inp.len() {
        let mut j = i + 1;
        while j < inp.len() && inp[j].rank == inp[i].rank {
            j += 1;
        }
        let g = &inp[i..j];
        if g.len() > 1 {
            let off = (n % g.len() as u64) as usize;
            for k in 0..g.len() {
                out.push(g[(k + off) % g.len()].clone());
            }
        } else {
            out.push(g[0].clone());
        }
        i = j;
    }
    out
}

/// `estimateTokens`: roughly size a request so members whose context window clearly
/// can't hold it are skipped. Deliberately crude (~3.5 chars/token), biased to
/// over-estimate slightly; it only needs to tell "fits" from "nowhere near".
fn estimate_tokens(req: &CompletionRequest) -> usize {
    let mut n: usize = 0;
    for s in &req.system {
        n += s.len();
    }
    for m in &req.messages {
        n += blocks_len(&m.content);
    }
    for t in &req.tools {
        // InputSchema is a decoded map; charge a flat ~120 chars per top-level
        // property, plus the name/description length.
        n += t.name.len() + t.description.len() + t.input_schema_len() * 120;
    }
    n * 2 / 7 // ≈ len/3.5
}

fn blocks_len(bs: &[ContentBlock]) -> usize {
    let mut n = 0;
    for b in bs {
        // Go `len(b.Input)` is the raw JSON byte length; the model keeps a decoded
        // `Value`, so use its compact serialization length as the closest estimate.
        let input_len = b.input.as_ref().map_or(0, |v| v.to_string().len());
        n += b.text.len() + b.thinking.len() + input_len + b.name.len();
        if !b.content.is_empty() {
            n += blocks_len(&b.content);
        }
    }
    n
}

#[async_trait]
impl Provider for Pool {
    async fn complete(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error> {
        let order = self.order(&req);
        let n = order.len();
        let mut last_err: Option<Error> = None;
        for (i, m) in order.iter().enumerate() {
            match m.prov.complete(ctx.clone(), req.clone()).await {
                Ok((msg, sr, usage)) => {
                    self.health.record_call(&ctx, m.id, false, false, None); // Pass
                    return Ok((msg, sr, usage));
                }
                Err(e) => {
                    if !should_failover(&ctx, Some(&e)) {
                        // Non-failover error (cancel, deterministic 4xx): surface as-is
                        // without failing over. RecordCall clears state on atomic calls.
                        self.health.record_call(&ctx, m.id, false, false, Some(&e));
                        return Err(e);
                    }
                    let (tripped, _msg) =
                        self.health.record_call(&ctx, m.id, false, false, Some(&e));
                    if tripped {
                        eprintln!(
                            "[llmpool] profile {:?}({}) breaker tripped",
                            m.name, m.model
                        );
                    }
                    last_err = Some(e);
                    if i + 1 < n {
                        let nx = &order[i + 1];
                        eprintln!(
                            "[llmpool] LLM failover: {:?}({}) -> {:?}({}), reason: {}",
                            m.name,
                            m.model,
                            nx.name,
                            nx.model,
                            trim_err(last_err.as_ref().unwrap())
                        );
                    }
                }
            }
        }
        Err(exhausted_error(last_err))
    }

    fn stream(
        &self,
        ctx: Context,
        req: CompletionRequest,
    ) -> BoxStream<'static, Result<StreamEvent, Error>> {
        let order = self.order(&req);
        let health = self.health.clone();
        Box::pin(async_stream::stream! {
            // The one hard rule: a member may only be abandoned BEFORE it has
            // yielded any event. Once text or a tool_use reached the caller, a
            // mid-stream failure is surfaced as-is (the harness resume logic owns it).
            let mut last_err: Option<Error> = None;
            let n = order.len();
            for (i, m) in order.iter().enumerate() {
                let mut emitted = false;
                let mut failed: Option<Error> = None;
                {
                    let mut s = m.prov.stream(ctx.clone(), req.clone());
                    while let Some(item) = s.next().await {
                        match item {
                            // Safe window: nothing reached the caller yet.
                            Err(e) if !emitted && should_failover(&ctx, Some(&e)) => {
                                failed = Some(e);
                                break;
                            }
                            Ok(ev) => {
                                emitted = true;
                                yield Ok(ev);
                            }
                            Err(e) => {
                                // Terminal error (mid-stream, or non-failover): hand it
                                // to the caller and stop.
                                yield Err(e);
                                return;
                            }
                        }
                    }
                }
                match failed {
                    None => {
                        // Completed (or delivered a non-failover terminal error): Pass.
                        health.record_call(&ctx, m.id, true, false, None);
                        return;
                    }
                    Some(e) => {
                        let (tripped, _msg) =
                            health.record_call(&ctx, m.id, true, false, Some(&e));
                        if tripped {
                            eprintln!(
                                "[llmpool] profile {:?}({}) breaker tripped",
                                m.name, m.model
                            );
                        }
                        last_err = Some(e);
                        if i + 1 < n {
                            let nx = &order[i + 1];
                            eprintln!(
                                "[llmpool] LLM failover: {:?}({}) -> {:?}({}), reason: {}",
                                m.name,
                                m.model,
                                nx.name,
                                nx.model,
                                trim_err(last_err.as_ref().unwrap())
                            );
                        }
                    }
                }
            }
            let last = last_err
                .map(|e| e.message())
                .unwrap_or_else(|| ERR_EXHAUSTED.to_string());
            yield Err(Error::Exhausted(format!("{ERR_EXHAUSTED}: {last}")));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_of_matches_regexp() {
        assert_eq!(status_of("openai: status 500: down"), 500);
        assert_eq!(status_of("status 400: bad"), 400);
        assert_eq!(status_of("status 5000"), 500); // regexp captures the first 3 digits
        assert_eq!(status_of("status 50"), 0); // needs exactly 3 digits
        assert_eq!(status_of("no status here"), 0);
        assert_eq!(status_of(""), 0);
        assert_eq!(status_of("a status 429 then status 503"), 429); // leftmost wins
        assert_eq!(status_of("status abc status 404"), 404); // skip non-digit, find a later match
    }

    #[test]
    fn trim_err_caps_at_300_bytes_on_rune_boundary() {
        // Collapses newlines and trims surrounding space.
        assert_eq!(trim_err(&Error::msg("  hello\nworld  ")), "hello world");
        // Exactly 300 bytes of ASCII: kept whole, plus the ellipsis.
        let ascii = "a".repeat(350);
        assert_eq!(
            trim_err(&Error::msg(ascii)),
            format!("{}\u{2026}", "a".repeat(300))
        );
        // A 300-byte cut that lands mid-codepoint backs off to the rune start.
        // "a" + 150×"€" (3 bytes) = 451 bytes; byte 300 is inside char #99, so the
        // cut backs off to byte 298 = "a" + 99×"€".
        let mixed = format!("a{}", "\u{20ac}".repeat(150));
        assert_eq!(
            trim_err(&Error::msg(mixed)),
            format!("a{}\u{2026}", "\u{20ac}".repeat(99))
        );
        // Port of TestTrimErrKeepsValidUTF8: a CJK body cut at 300 bytes must stay
        // valid UTF-8 (else PostgreSQL rejects the breaker's last_error) and end
        // with the ellipsis.
        let cjk = Error::msg(format!(
            "openai: status 503: {}",
            "\u{670d}\u{52a1}\u{6682}\u{65f6}\u{4e0d}\u{53ef}\u{7528}\u{ff0c}\u{8bf7}\u{7a0d}\u{540e}\u{91cd}\u{8bd5}\u{3002}".repeat(10)
        ));
        let got = trim_err(&cjk);
        assert!(
            got.is_char_boundary(got.len())
                && got.ends_with('\u{2026}')
                && got.len() <= 300 + '\u{2026}'.len_utf8(),
            "trim_err = {got:?} ({} bytes), want ≤300 bytes plus …",
            got.len()
        );
        // The cut must not split a multi-byte rune: re-decoding the bytes is infallible.
        assert_eq!(std::str::from_utf8(got.as_bytes()).unwrap(), got);
    }

    #[test]
    fn should_failover_classifies_statuses() {
        let ctx = Context::background();
        // Failover-eligible: transport (no status) and these codes.
        for code in [0u16, 401, 402, 403, 404, 408, 429, 500, 503] {
            let e = if code == 0 {
                Error::msg("transport reset by peer")
            } else {
                Error::msg(format!("provider: status {code}: body"))
            };
            assert!(
                should_failover(&ctx, Some(&e)),
                "status {code} should fail over"
            );
        }
        // Never fails over: 400, other 4xx, cancellation, nil, cancelled ctx.
        for code in [400u16, 418, 200] {
            let e = Error::msg(format!("provider: status {code}: body"));
            assert!(
                !should_failover(&ctx, Some(&e)),
                "status {code} should not fail over"
            );
        }
        assert!(!should_failover(&ctx, Some(&Error::Canceled)));
        assert!(!should_failover(&ctx, Some(&Error::DeadlineExceeded)));
        assert!(!should_failover(&ctx, None));
        assert!(!should_failover(
            &Context::canceled(),
            Some(&Error::msg("status 500"))
        ));
    }

    #[test]
    fn is_hard_failure_only_deterministic_4xx() {
        for code in [401u16, 402, 403, 404] {
            assert!(is_hard_failure(&Error::msg(format!("status {code}"))));
        }
        for code in [400u16, 408, 429, 500, 0] {
            let e = if code == 0 {
                Error::msg("reset")
            } else {
                Error::msg(format!("status {code}"))
            };
            assert!(!is_hard_failure(&e), "status {code} is not a hard failure");
        }
    }
}
