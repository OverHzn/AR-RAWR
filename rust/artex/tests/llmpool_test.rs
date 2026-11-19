//! Rust port of the `llmpool` Go tests — the behavioral spec for the circuit-breaker
//! registry, its failure classification, and the failover Pool.
//!
//! Ported from `llmpool/health_record_test.go` (`TestRecordCallPolicy`),
//! `llmpool/health_policy_test.go`, `llmpool/health_order_test.go`, and the
//! provider-facing specs in `llmpool/pool_test.go` (failover, breaker, resume,
//! context-cancel, and chain-order). Fakes are scripted providers. The db-backed
//! `TestPoolFeedsRecordCallPolicy` needs the agent `Workspace` persistence callbacks
//! and lands with the agent unit.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use artex::llm::{
    CompletionRequest, Context, Error, Message, Provider, StreamEvent, Usage, SE_TEXT_DELTA,
};
use artex::llmpool::{Member, Pool, Registry, State};
use async_trait::async_trait;
use futures::StreamExt;

/// `time.Until(st.OpenUntil)` in Rust: how long until the breaker closes.
fn remaining(st: &State) -> Duration {
    st.open_until.map_or(Duration::ZERO, |t| {
        t.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
    })
}

/// Port of `TestRecordCallPolicy`: RecordCall is the policy a provider call applies
/// when no Pool wraps it. A completed call clears the state; a failover-eligible
/// failure trips; a non-failover failure neither trips nor, on a stream, clears.
#[test]
fn record_call_policy() {
    let ctx = Context::background();
    let soft = Error::msg("openai: status 500: down");
    let bad = Error::msg("openai: status 400: bad request");

    let seed = || {
        let reg = Registry::new(None, None);
        reg.record_call(&ctx, 7, false, false, Some(&soft));
        let st = reg.get(7);
        assert_eq!((st.fails, st.trips), (1, 0), "seed: want one soft failure");
        reg
    };

    // success clears
    {
        let reg = seed();
        reg.record_call(&ctx, 7, false, false, None);
        let st = reg.get(7);
        assert_eq!((st.fails, st.trips), (0, 0), "after success, want clean");
        assert!(!st.open());
    }
    // atomic non-failover clears
    {
        let reg = seed();
        reg.record_call(&ctx, 7, false, false, Some(&bad));
        let st = reg.get(7);
        assert_eq!(
            (st.fails, st.trips),
            (0, 0),
            "after an atomic 400, want clean"
        );
    }
    // stream non-failover keeps
    {
        let reg = seed();
        reg.record_call(&ctx, 7, true, false, Some(&bad));
        let st = reg.get(7);
        assert_eq!(
            (st.fails, st.trips),
            (1, 0),
            "after a stream 400, want the failure kept"
        );
    }
    // stream cancel keeps
    {
        let reg = seed();
        reg.record_call(&ctx, 7, true, false, Some(&Error::Canceled));
        let st = reg.get(7);
        assert_eq!(
            (st.fails, st.trips),
            (1, 0),
            "after a cancel, want the failure kept"
        );
    }
    // committed stream failure keeps
    {
        let reg = seed();
        reg.record_call(&ctx, 7, true, true, Some(&soft));
        let st = reg.get(7);
        assert_eq!(
            (st.fails, st.trips),
            (1, 0),
            "after a committed failure, want it ignored"
        );
    }
    // pre-commit stream failure counts
    {
        let reg = Registry::new(None, None);
        reg.record_call(&ctx, 7, true, false, Some(&soft));
        let st = reg.get(7);
        assert_eq!((st.fails, st.trips), (1, 0), "want one soft failure");
        assert!(!st.open());
    }
    // hard failure trips at once
    {
        let reg = Registry::new(None, None);
        reg.record_call(
            &ctx,
            7,
            false,
            false,
            Some(&Error::msg("openai: status 401: bad key")),
        );
        let st = reg.get(7);
        assert_eq!(st.trips, 1, "want a trip after one hard failure");
        assert!(st.open(), "breaker should be open");
    }
    // profile 0 is a no-op
    {
        let reg = Registry::new(None, None);
        reg.record_call(&ctx, 0, false, false, Some(&soft));
        assert_eq!(reg.snapshot().len(), 0, "profile 0 records no state");
    }
}

/// Port of `TestSetPolicyOverridesThresholdAndCooldown`.
#[test]
fn set_policy_overrides_threshold_and_cooldown() {
    let reg = Registry::new(None, None);
    reg.set_policy(2, Duration::from_secs(90));
    assert!(
        !reg.trip(1, "429", false),
        "tripped on the first transient failure"
    );
    assert!(
        reg.trip(1, "429", false),
        "should trip on transient failure #2"
    );
    let d = remaining(&reg.get(1));
    assert!(
        (Duration::from_secs(80)..=Duration::from_secs(90)).contains(&d),
        "cooldown={d:?}, want ~90s"
    );
    // Every later trip keeps the same fixed window instead of climbing the ladder.
    reg.trip(1, "429", true);
    let d = remaining(&reg.get(1));
    assert!(
        (Duration::from_secs(80)..=Duration::from_secs(90)).contains(&d),
        "second cooldown={d:?}, want ~90s (fixed)"
    );
}

/// Port of `TestSetPolicyDisablesSoftTrip`.
#[test]
fn set_policy_disables_soft_trip() {
    let reg = Registry::new(None, None);
    reg.set_policy(-1, Duration::ZERO);
    for i in 1..=10 {
        assert!(
            !reg.trip(1, "429", false),
            "transient failure #{i} tripped, want never"
        );
    }
    assert!(
        !reg.is_open(1),
        "breaker should stay closed for transient failures"
    );
    assert!(
        reg.trip(1, "no credit", true),
        "a hard failure must still trip immediately"
    );
    let d = remaining(&reg.get(1));
    assert!(
        (Duration::from_secs(50)..=Duration::from_secs(60)).contains(&d),
        "cooldown={d:?}, want the default first rung (~1min)"
    );
}

/// Port of `TestZeroPolicyKeepsDefaults`.
#[test]
fn zero_policy_keeps_defaults() {
    let reg = Registry::new(None, None);
    reg.set_policy(0, Duration::ZERO);
    for i in 1..artex::llmpool::SOFT_TRIP_AFTER {
        assert!(
            !reg.trip(1, "429", false),
            "tripped after {i} transient failures"
        );
    }
    assert!(
        reg.trip(1, "429", false),
        "should trip on failure #{SOFT}",
        SOFT = artex::llmpool::SOFT_TRIP_AFTER
    );
    let d = remaining(&reg.get(1));
    assert!(
        (Duration::from_secs(50)..=Duration::from_secs(60)).contains(&d),
        "cooldown={d:?}, want the first ladder rung (~1min)"
    );
}

/// Port of `TestBackoffLadderGrows`: the cooldown ladder is non-decreasing and
/// plateaus at its last rung (~30m).
#[test]
fn backoff_ladder_grows() {
    let reg = Registry::new(None, None);
    let mut prev = Duration::ZERO;
    for i in 0..4i64 {
        reg.reset(1);
        reg.restore(
            1,
            State {
                trips: i,
                ..Default::default()
            },
        );
        reg.trip(1, "402", true);
        let d = remaining(&reg.get(1));
        // Non-decreasing, with a second of slack: the ladder plateaus and each Trip
        // stamps its own time.Now().
        if i > 0 && d + Duration::from_secs(1) < prev {
            panic!(
                "trip #{} cools for {d:?}, shorter than the previous {prev:?}",
                i + 1
            );
        }
        prev = d;
    }
    assert!(
        prev >= Duration::from_secs(25 * 60),
        "ladder tops out at {prev:?}, want ~30m"
    );
}

/// Port of `TestExpiredWindowIsClosed`: an expired cooling-off window reads closed.
#[test]
fn expired_window_is_closed() {
    let reg = Registry::new(None, None);
    reg.restore(
        1,
        State {
            trips: 1,
            open_until: Some(SystemTime::now() - Duration::from_secs(1)),
            ..Default::default()
        },
    );
    assert!(!reg.is_open(1), "expired window should read as closed");
}

/// Port of `TestHealthCallbacksStayInStateOrder`: persist and forget run under the
/// registry lock, so a second update on the same profile cannot enqueue until the
/// first callback returns — the queue cannot observe the later state first.
#[test]
fn callbacks_stay_in_state_order() {
    let entered = Arc::new(AtomicI32::new(0));
    let (first_in_tx, first_in_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));

    let block = {
        let entered = entered.clone();
        let first_in_tx = first_in_tx.clone();
        let release_rx = release_rx.clone();
        move || {
            if entered.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = first_in_tx.send(());
                // Hold the registry lock (we are inside persist) until released,
                // exactly like the Go test's blocking callback.
                let _ = release_rx.lock().unwrap().recv();
            }
        }
    };

    let persist = {
        let block = block.clone();
        move |_id: i64, _st: &State| block()
    };
    let forget = {
        let block = block.clone();
        move |_id: i64| block()
    };

    let reg = Arc::new(Registry::new(
        Some(Box::new(persist)),
        Some(Box::new(forget)),
    ));

    let reg_t = reg.clone();
    let tripper = thread::spawn(move || {
        reg_t.trip(1, "down", true);
    });

    first_in_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Trip did not reach persist");

    let reg_p = reg.clone();
    let (passed_tx, passed_rx) = mpsc::channel::<()>();
    let passer = thread::spawn(move || {
        reg_p.pass(1);
        let _ = passed_tx.send(());
    });

    assert!(
        passed_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "Pass enqueued while Trip's persist was still running"
    );

    release_tx.send(()).unwrap();

    passed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Pass did not finish after Trip released the lock");

    tripper.join().unwrap();
    passer.join().unwrap();

    assert_eq!(
        entered.load(Ordering::SeqCst),
        2,
        "callbacks, want Trip then Pass"
    );
}

// === Scripted fake providers + Pool tests (llmpool/pool_test.go) ===

/// A scripted provider: `script[i]` is what the i-th call yields — some events, then
/// optionally an error. Indexing past the end repeats the last entry, so "always
/// succeeds" / "always 402" behaves that way across repeated calls (Go `at`).
struct FakeProv {
    calls: Arc<AtomicU64>,
    events: Vec<Vec<StreamEvent>>,
    errs: Vec<Option<Error>>,
}

impl FakeProv {
    fn new(events: Vec<Vec<StreamEvent>>, errs: Vec<Option<Error>>) -> Arc<Self> {
        Arc::new(FakeProv {
            calls: Arc::new(AtomicU64::new(0)),
            events,
            errs,
        })
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// Go `at(s, n)`: script entry n, repeating the last once the script runs out.
    fn at<T: Clone>(s: &[T], n: u64) -> Option<T> {
        if s.is_empty() {
            return None;
        }
        let idx = if (n as usize) >= s.len() {
            s.len() - 1
        } else {
            n as usize
        };
        Some(s[idx].clone())
    }
}

#[async_trait]
impl Provider for FakeProv {
    async fn complete(
        &self,
        _ctx: Context,
        _req: CompletionRequest,
    ) -> Result<(Message, String, Usage), Error> {
        // Mirror `stream` so Complete and Stream drive the same script.
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = Self::at(&self.events, n).unwrap_or_default();
        if let Some(Some(e)) = Self::at(&self.errs, n) {
            return Err(e);
        }
        let mut text = String::new();
        for ev in &events {
            if ev.ty == SE_TEXT_DELTA {
                text.push_str(&ev.text);
            }
        }
        let msg = Message {
            role: "assistant".to_string(),
            content: vec![artex::llm::text_block(&text)],
        };
        Ok((msg, "end_turn".to_string(), Usage::default()))
    }

    fn stream(
        &self,
        _ctx: Context,
        _req: CompletionRequest,
    ) -> futures::stream::BoxStream<'static, Result<StreamEvent, Error>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = Self::at(&self.events, n).unwrap_or_default();
        let err = Self::at(&self.errs, n).flatten();
        Box::pin(async_stream::stream! {
            for ev in events {
                yield Ok(ev);
            }
            if let Some(e) = err {
                yield Err(e);
            }
        })
    }
}

fn text_delta(s: &str) -> StreamEvent {
    StreamEvent {
        ty: SE_TEXT_DELTA.to_string(),
        text: s.to_string(),
        ..Default::default()
    }
}

/// `failProv`: fails every call (before any event) with a `status <code>` error.
fn fail_prov(name: &str, code: u16) -> Arc<FakeProv> {
    let _ = name;
    FakeProv::new(
        vec![],
        vec![Some(Error::msg(format!("anthropic: status {code}: nope")))],
    )
}

/// `okProv`: streams `text` (one text delta) on every call.
fn ok_prov(name: &str, text: &str) -> Arc<FakeProv> {
    let _ = name;
    FakeProv::new(vec![vec![text_delta(text)]], vec![None])
}

/// A pool member over `prov` with the given chain `rank` and context window.
/// Mirrors the Go `member()` helper (`Model = "m"+name`, `WindowTokens = 0`).
fn member(id: i64, name: &str, rank: i64, window: i64, prov: Arc<FakeProv>) -> Member {
    Member {
        id,
        name: name.to_string(),
        model: format!("m{name}"),
        format: "anthropic".to_string(),
        priority: 0,
        active: false,
        rank,
        window_tokens: window,
        prov,
    }
}

/// Drain a stream into its concatenated text and terminal error (Go `drain`).
async fn drain(
    mut s: futures::stream::BoxStream<'static, Result<StreamEvent, Error>>,
) -> (String, Option<Error>) {
    let mut text = String::new();
    while let Some(item) = s.next().await {
        match item {
            Ok(e) if e.ty == SE_TEXT_DELTA => text.push_str(&e.text),
            Ok(_) => {}
            Err(e) => return (text, Some(e)),
        }
    }
    (text, None)
}

/// `TestFailoverOnNoCredit`: failover to the next member on a retryable error.
#[tokio::test]
async fn failover_on_no_credit() {
    let a = fail_prov("a", 402);
    let b = ok_prov("b", "hello");
    let p = Pool::new(
        vec![
            member(1, "a", 10, 0, a.clone()),
            member(2, "b", 5, 0, b.clone()),
        ],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();

    let (text, err) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert!(err.is_none(), "expected failover to succeed, got {err:?}");
    assert_eq!(text, "hello");
    assert_eq!((a.calls(), b.calls()), (1, 1), "want calls a=1 b=1");
}

/// `TestHardFailureTripsBreakerImmediately`: a 402 opens the breaker so the next
/// request skips that profile entirely.
#[tokio::test]
async fn hard_failure_trips_breaker_immediately() {
    let a = fail_prov("a", 402);
    let b = ok_prov("b", "x");
    let reg = Arc::new(Registry::new(None, None));
    let p = Pool::new(
        vec![
            member(1, "a", 10, 0, a.clone()),
            member(2, "b", 5, 0, b.clone()),
        ],
        Some(reg.clone()),
    )
    .unwrap();

    let _ = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert!(
        reg.is_open(1),
        "402 should have tripped the breaker on the first failure"
    );
    let _ = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert_eq!(a.calls(), 1, "tripped profile was called again");
    assert_eq!(b.calls(), 2, "fallback should have been used twice");
}

/// `TestNoFailoverAfterEmit`: once a member emits an event, a mid-stream failure is
/// surfaced as-is (the harness resume logic owns it); no second member is tried.
#[tokio::test]
async fn no_failover_after_emit() {
    let a = FakeProv::new(
        vec![vec![text_delta("partial")]],
        vec![Some(Error::msg("anthropic: status 500: mid-stream drop"))],
    );
    let b = ok_prov("b", "full");
    let p = Pool::new(
        vec![member(1, "a", 10, 0, a), member(2, "b", 5, 0, b.clone())],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();

    let (text, err) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert!(
        err.is_some(),
        "mid-stream error should surface, not be swallowed by a failover"
    );
    assert_eq!(text, "partial", "want the partial output");
    assert_eq!(
        b.calls(),
        0,
        "fell over after emitting output — would duplicate the turn"
    );
}

/// `TestNoFailoverOnCancel`: cancelling a task must not burn a backup key, nor be
/// diagnosed as an LLM fault.
#[tokio::test]
async fn no_failover_on_cancel() {
    let a = FakeProv::new(vec![], vec![Some(Error::Canceled)]);
    let b = ok_prov("b", "x");
    let reg = Arc::new(Registry::new(None, None));
    let p = Pool::new(
        vec![member(1, "a", 10, 0, a), member(2, "b", 5, 0, b.clone())],
        Some(reg.clone()),
    )
    .unwrap();

    let ctx = Context::canceled();
    let (_text, err) = drain(p.stream(ctx, CompletionRequest::default())).await;
    assert!(
        err.as_ref().is_some_and(Error::is_context_err),
        "want context.Canceled, got {err:?}"
    );
    assert_eq!(b.calls(), 0, "failed over on cancellation");
    assert_eq!(
        reg.get(1).fails,
        0,
        "cancellation counted as a profile failure"
    );
}

/// `TestSkipsMembersTooSmallForRequest`: a member whose window can't hold the
/// request is skipped rather than sent a guaranteed-400 round-trip.
#[tokio::test]
async fn skips_members_too_small_for_request() {
    let small = ok_prov("small", "s");
    let big = ok_prov("big", "b");
    let p = Pool::new(
        vec![
            member(1, "small", 10, 1000, small.clone()),
            member(2, "big", 5, 1_000_000, big),
        ],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();

    let req = CompletionRequest {
        messages: vec![artex::llm::user_text(&"x".repeat(100_000))],
        ..Default::default()
    };
    let (text, err) = drain(p.stream(Context::background(), req)).await;
    assert!(err.is_none(), "unexpected error: {err:?}");
    assert_eq!(text, "b", "want the large-window member to serve it");
    assert_eq!(
        small.calls(),
        0,
        "sent a request to a member that can't hold it"
    );
}

/// `TestAllTrippedStillProbesHead`: when every member is tripped, probing the head
/// beats stalling the engine outright.
#[tokio::test]
async fn all_tripped_still_probes_head() {
    let a = ok_prov("a", "a");
    let b = ok_prov("b", "b");
    let reg = Registry::new(None, None);
    reg.trip(1, "402", true);
    reg.trip(2, "402", true);
    let p = Pool::new(
        vec![member(1, "a", 10, 0, a.clone()), member(2, "b", 5, 0, b)],
        Some(Arc::new(reg)),
    )
    .unwrap();

    let (text, err) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert!(err.is_none(), "unexpected error: {err:?}");
    assert_eq!(
        (text.as_str(), a.calls()),
        ("a", 1),
        "want the head probed as a last resort"
    );
}

/// `TestExhaustedChainReportsClearly`: when every member fails, the error wraps
/// `ErrExhausted` (so `errors.Is` / `Error::is_exhausted` holds).
#[tokio::test]
async fn exhausted_chain_reports_clearly() {
    let a = fail_prov("a", 402);
    let b = fail_prov("b", 402);
    let p = Pool::new(
        vec![member(1, "a", 10, 0, a), member(2, "b", 5, 0, b)],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();

    let (_text, err) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert!(
        err.as_ref().is_some_and(Error::is_exhausted),
        "err = {err:?}, want it to wrap ErrExhausted"
    );
}

/// `TestEqualRankRotates`: equal-rank members take turns leading, so duplicate-key
/// profiles share the load. The RankActive head fails; the two rank-5 members split.
#[tokio::test]
async fn equal_rank_rotates() {
    let a = ok_prov("a", "a");
    let b = ok_prov("b", "b");
    let p = Pool::new(
        vec![
            member(
                1,
                "head",
                artex::llmpool::RANK_ACTIVE,
                0,
                fail_prov("head", 402),
            ),
            member(2, "a", 5, 0, a),
            member(3, "b", 5, 0, b),
        ],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();

    let mut count_a = 0;
    let mut count_b = 0;
    for _ in 0..4 {
        let (txt, _) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
        match txt.as_str() {
            "a" => count_a += 1,
            "b" => count_b += 1,
            other => panic!("unexpected text {other:?}"),
        }
    }
    assert!(
        count_a > 0 && count_b > 0,
        "same-rank members did not rotate: a={count_a} b={count_b}"
    );
}

/// `TestHeadAlwaysFirst`: the head keeps its own (active) rank and never joins a
/// rotation group, so it is always tried first.
#[tokio::test]
async fn head_always_first() {
    let head = ok_prov("head", "H");
    let p = Pool::new(
        vec![
            member(1, "head", artex::llmpool::RANK_ACTIVE, 0, head),
            member(2, "a", 5, 0, ok_prov("a", "a")),
            member(3, "b", 5, 0, ok_prov("b", "b")),
        ],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();
    for _ in 0..5 {
        let (txt, _) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
        assert_eq!(txt, "H", "head was not tried first");
    }
}

/// `TestSingleMemberPassthrough`: a single-member chain behaves exactly like the
/// bare provider.
#[tokio::test]
async fn single_member_passthrough() {
    let a = ok_prov("a", "solo");
    let p = Pool::new(
        vec![member(1, "a", artex::llmpool::RANK_ACTIVE, 0, a)],
        Some(Arc::new(Registry::new(None, None))),
    )
    .unwrap();
    let (text, err) = drain(p.stream(Context::background(), CompletionRequest::default())).await;
    assert!(
        err.is_none() && text == "solo",
        "got {text:?} / {err:?}, want solo / nil"
    );
}

/// `TestNewEmptyChainIsNil`: an empty chain yields no Pool, so callers use the bare
/// provider.
#[test]
fn new_empty_chain_is_nil() {
    assert!(
        Pool::new(vec![], None).is_none(),
        "empty chain should yield no Pool"
    );
}
