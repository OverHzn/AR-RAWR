//! Rust port of the Go `llmpool/` package — LLM failover with a process-wide
//! circuit-breaker registry.
//!
//! This module covers the **health/registry** half of the package plus the pure
//! failure-classification helpers: the circuit-breaker state machine (`Registry`,
//! `State`), the failover policy (`should_failover`), the HTTP-status extractor
//! (`status_of`), hard-failure detection (`is_hard_failure`), log trimming
//! (`trim_err`), and the cooldown ladder. It has no dependency on the async
//! `Provider` runtime — every input is a plain error/context value — so it is fully
//! deterministic and unit-testable. The failover `Pool` (which wraps
//! `llm::Provider` with streaming) lands in a later unit on top of this.
//!
//! Faithful to `llmpool/health.go` and the classification helpers in
//! `llmpool/pool.go`. Go's `time.Time`/`time.Now` are modeled with
//! `std::time::SystemTime` (wall clock, persistable); `time.Duration` with
//! `std::time::Duration`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use gocompat::strings::go_trim_space;

use crate::llm::{Context, Error};

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
