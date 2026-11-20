//! Login/password-change rate limiter — pure port of `server/login_limit.go`.
//! Limits per client IP, never per account (an attacker must not be able to
//! lock the admin out by failing on purpose). State is in memory only.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

pub const LOGIN_MAX_ATTEMPTS: i32 = 10;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(60);
pub const LOGIN_BASE_BLOCK: Duration = Duration::from_secs(60);
pub const LOGIN_MAX_BLOCK: Duration = Duration::from_secs(15 * 60);
pub const LOGIN_SWEEP_EVERY: Duration = Duration::from_secs(60);
pub const LOGIN_MAX_ENTRIES: usize = 10_000;
pub const PWD_LIMIT_TOKEN_PREFIX: &str = "pwd:";

#[derive(Debug, Clone)]
struct Entry {
    attempts: i32,
    window_start: Duration,
    blocked_until: Duration,
    block: Duration,
    last_seen: Duration,
}

/// A clock returning monotonically increasing elapsed time (injectable in tests).
pub type Clock = dyn Fn() -> Duration + Send + Sync;

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    last_sweep: Duration,
}

pub struct LoginLimiter {
    now: Box<Clock>,
    inner: Mutex<Inner>,
}

impl LoginLimiter {
    pub fn new(now: Box<Clock>) -> Self {
        LoginLimiter {
            now,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// `allow`: record one attempt; 0 means proceed, otherwise the wait.
    pub fn allow(&self, key: &str) -> Duration {
        let now = (self.now)();
        let mut l = self.inner.lock().unwrap();
        Self::sweep(&mut l, now);
        let e = l.entries.entry(key.to_string()).or_insert_with(|| Entry {
            attempts: 0,
            window_start: now,
            blocked_until: Duration::ZERO,
            block: Duration::ZERO,
            last_seen: now,
        });
        e.last_seen = now;
        if now < e.blocked_until {
            e.block = (e.block * 2).min(LOGIN_MAX_BLOCK);
            e.blocked_until = now + e.block;
            return e.block;
        }
        if now - e.window_start >= LOGIN_WINDOW {
            e.attempts = 0;
            e.window_start = now;
        }
        if e.attempts >= LOGIN_MAX_ATTEMPTS {
            e.block = if e.block.is_zero() {
                LOGIN_BASE_BLOCK
            } else {
                (e.block * 2).min(LOGIN_MAX_BLOCK)
            };
            e.blocked_until = now + e.block;
            e.attempts = 0;
            e.window_start = now;
            return e.block;
        }
        e.attempts += 1;
        Duration::ZERO
    }

    pub fn reset(&self, key: &str) {
        self.inner.lock().unwrap().entries.remove(key);
    }

    fn sweep(l: &mut Inner, now: Duration) {
        if now - l.last_sweep < LOGIN_SWEEP_EVERY {
            return;
        }
        l.last_sweep = now;
        l.entries.retain(|_, e| now - e.last_seen < LOGIN_MAX_BLOCK);
        if l.entries.len() < LOGIN_MAX_ENTRIES {
            return;
        }
        let mut oldest: Vec<(String, Duration)> = l
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.last_seen))
            .collect();
        oldest.sort_by_key(|(_, t)| *t);
        let excess = l.entries.len() - LOGIN_MAX_ENTRIES;
        for (k, _) in oldest.into_iter().take(excess) {
            l.entries.remove(&k);
        }
    }
}

/// `passwordChangeWait`: both the IP and the token prefix are tracked, so a
/// stolen token alone does not survive an IP rotation (or vice versa).
pub fn password_change_wait(limiter: &LoginLimiter, ip: &str, token: &str) -> Duration {
    let tok = format!("{PWD_LIMIT_TOKEN_PREFIX}{token}");
    limiter.allow(ip).max(limiter.allow(&tok))
}

pub fn password_change_reset(limiter: &LoginLimiter, ip: &str, token: &str) {
    limiter.reset(ip);
    limiter.reset(&format!("{PWD_LIMIT_TOKEN_PREFIX}{token}"));
}
