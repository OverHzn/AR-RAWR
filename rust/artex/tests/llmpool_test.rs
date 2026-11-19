//! Rust port of the pure (Pool-free) Go `llmpool` health tests — the behavioral
//! spec for the circuit-breaker registry and its failure classification.
//!
//! Ported from `llmpool/health_record_test.go` (`TestRecordCallPolicy`),
//! `llmpool/health_policy_test.go`, and `llmpool/health_order_test.go`. The Pool
//! tests (`TestPoolFeedsRecordCallPolicy`, and all of `pool_test.go`) depend on the
//! async `llm::Provider` failover loop and land with the Pool unit.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use artex::llm::{Context, Error};
use artex::llmpool::{Registry, State};

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
