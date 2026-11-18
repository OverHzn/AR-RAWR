//! The Rust port of `egress/`.
//!
//! Faithful ports of the pure value functions of the egress governor: the noise
//! profile lookup (`ProfileByName`, `ProfileFromEnv`, `CurrentProfile`) and the
//! recording-proxy pacing markers (`MarkProxyURL`, `IsPaceMarker`). The
//! pointer/`context.Context`/generic entry points (`New`, `Current`, `SetProfile`,
//! `HostKey`, `Do`, `Dynamic`, `Wait`) stay in Go. `rust-anything diff` checks
//! behavior against what the source recorded.

#![forbid(unsafe_code)]

use gocompat::encoding::base64_std_decode;
use gocompat::neturl::{Userinfo, url_parse};
use gocompat::strings::{go_equal_fold, go_to_lower, go_trim_space};
use serde_json::{Value, json};

/// `PaceUser` (`egress/pace.go:19`): the user and password of the pacing marker.
const PACE_USER: &str = "artex-pace";

/// `time.Duration` units as Go's `encoding/json` emits them (integer nanoseconds).
const NS_PER_MS: i64 = 1_000_000;
const NS_PER_SEC: i64 = 1_000_000_000;

/// Go `unicode.IsControl`: the C0 (U+0000–U+001F), DEL (U+007F) and C1
/// (U+0080–U+009F) control ranges.
fn is_control(c: char) -> bool {
    matches!(c as u32, 0x00..=0x1f | 0x7f..=0x9f)
}

/// A Go `float64` field as `encoding/json` writes it: a whole value with no
/// fractional part (Go uses the shortest `'f'` form, so `2.0` serializes as `2`).
fn go_float(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

/// The `Profile` struct (`egress/egress.go:37`) as Go's `encoding/json` marshals
/// it: every field (no `omitempty`), `time.Duration` as integer nanoseconds.
#[allow(clippy::too_many_arguments)]
fn profile_json(
    name: &str,
    rps: f64,
    burst: i64,
    max_per_host: i64,
    max_global: i64,
    jitter: f64,
    cache_ttl_ns: i64,
    max_retries: i64,
    base_delay_ns: i64,
    max_delay_ns: i64,
    user_agent: &str,
) -> Value {
    json!({
        "Name": name,
        "RPS": go_float(rps),
        "Burst": burst,
        "MaxPerHost": max_per_host,
        "MaxGlobal": max_global,
        "Jitter": go_float(jitter),
        "CacheTTL": cache_ttl_ns,
        "MaxRetries": max_retries,
        "BaseDelay": base_delay_ns,
        "MaxDelay": max_delay_ns,
        "UserAgent": user_agent,
    })
}

/// The zero `Profile{Name: "off"}` — a strict passthrough (all levers zero).
fn off_profile() -> Value {
    profile_json("off", 0.0, 0, 0, 0, 0.0, 0, 0, 0, 0, "")
}

/// `ARTEX_TARGET_USER_AGENT`, trimmed; "" when unset or when it contains a
/// control character (`ProfileByName` ignores such a value).
fn target_user_agent() -> String {
    match std::env::var("ARTEX_TARGET_USER_AGENT") {
        Ok(raw) => {
            let ua = go_trim_space(&raw);
            if ua.chars().any(is_control) {
                String::new()
            } else {
                ua.to_string()
            }
        }
        Err(_) => String::new(),
    }
}

/// `Profiles[name]` with `ARTEX_TARGET_USER_AGENT` applied; `(off, false)` when the
/// name (lower-cased, trimmed) is not a known profile.
fn profile_lookup(name: &str) -> (Value, bool) {
    let name = go_to_lower(go_trim_space(name));
    if name == "off" {
        return (off_profile(), true);
    }
    let ua = target_user_agent();
    let profile = match name.as_str() {
        // quiet: RPS 2, Burst 4, MaxPerHost 1, MaxGlobal 8, Jitter 0.5,
        // CacheTTL 1m, MaxRetries 3, BaseDelay 2s, MaxDelay 1m.
        "quiet" => profile_json(
            "quiet",
            2.0,
            4,
            1,
            8,
            0.5,
            NS_PER_SEC,
            3,
            2 * NS_PER_SEC,
            NS_PER_SEC,
            &ua,
        ),
        // balanced: RPS 5, Burst 10, MaxPerHost 2, MaxGlobal 16, Jitter 0.2,
        // CacheTTL 30s, MaxRetries 3, BaseDelay 1s, MaxDelay 30s.
        "balanced" => profile_json(
            "balanced",
            5.0,
            10,
            2,
            16,
            0.2,
            30 * NS_PER_SEC,
            3,
            NS_PER_SEC,
            30 * NS_PER_SEC,
            &ua,
        ),
        // aggressive: RPS 20, Burst 40, MaxPerHost 4, MaxGlobal 32, Jitter 0.1,
        // CacheTTL 10s, MaxRetries 2, BaseDelay 500ms, MaxDelay 10s.
        "aggressive" => profile_json(
            "aggressive",
            20.0,
            40,
            4,
            32,
            0.1,
            10 * NS_PER_SEC,
            2,
            500 * NS_PER_MS,
            10 * NS_PER_SEC,
            &ua,
        ),
        _ => return (off_profile(), false),
    };
    (profile, true)
}

/// Port of `ProfileByName` (`egress/egress.go:62`).
///
/// Returns `[Profile, ok]`: `ok` is false for any name but off/quiet/balanced/
/// aggressive (any case); the profile is `off` when `ok` is false.
pub fn profile_by_name(name: String) -> Result<Value, String> {
    let (profile, ok) = profile_lookup(&name);
    Ok(json!([profile, ok]))
}

/// Port of `ProfileFromEnv` (`egress/egress.go:80`).
///
/// `ARTEX_NOISE_PROFILE`, trimmed; unset, "off" or unknown resolves to `off`.
pub fn profile_from_env() -> Result<Value, String> {
    let raw = std::env::var("ARTEX_NOISE_PROFILE").unwrap_or_default();
    let (profile, _) = profile_lookup(go_trim_space(&raw));
    Ok(profile)
}

/// Port of `CurrentProfile` (`egress/egress.go:116`).
///
/// `CurrentProfile` is `Current().p`; with no `SetProfile` in the process,
/// `Current` builds `New(ProfileFromEnv())`, so this is `ProfileFromEnv`.
pub fn current_profile() -> Result<Value, String> {
    profile_from_env()
}

/// Port of `MarkProxyURL` (`egress/pace.go:24`).
///
/// `raw` with the marker userinfo; unchanged when it does not parse, has no host,
/// or already carries a userinfo.
pub fn mark_proxy_url(raw: String) -> Result<String, String> {
    let mut u = match url_parse(&raw) {
        Ok(u) => u,
        Err(_) => return Ok(raw),
    };
    if u.host.is_empty() || u.user.is_some() {
        return Ok(raw);
    }
    u.user = Some(Userinfo::user_password(PACE_USER, PACE_USER));
    Ok(u.to_url_string())
}

/// `strings.Cut(s, sep)` with a one-character separator: `(before, after, found)`.
fn cut(s: &str, sep: char) -> (&str, &str, bool) {
    match s.split_once(sep) {
        Some((before, after)) => (before, after, true),
        None => (s, "", false),
    }
}

/// Port of `IsPaceMarker` (`egress/pace.go:35`).
///
/// Reports whether `proxyAuthorization` is Basic credentials for `PaceUser`.
pub fn is_pace_marker(proxy_authorization: String) -> Result<bool, String> {
    // scheme, enc, ok := Cut(TrimSpace(v), " "); require ok && scheme == "Basic".
    let (scheme, enc, found) = cut(go_trim_space(&proxy_authorization), ' ');
    if !found || !go_equal_fold(scheme, "Basic") {
        return Ok(false);
    }
    // dec, err := base64.StdEncoding.DecodeString(TrimSpace(enc)); err -> false.
    let dec = match base64_std_decode(go_trim_space(enc)) {
        Ok(dec) => dec,
        Err(_) => return Ok(false),
    };
    // user, _, _ := Cut(string(dec), ":"); user == PaceUser (byte-exact, as Go).
    let user = match dec.iter().position(|&b| b == b':') {
        Some(i) => &dec[..i],
        None => &dec[..],
    };
    Ok(user == PACE_USER.as_bytes())
}
