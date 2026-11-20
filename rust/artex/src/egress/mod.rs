//! Pure egress helpers — slice of `egress/pace.go` + `egress/egress.go`.
//!
//! The governor's concurrency/rate-limiting machinery is deferred; this module
//! ports the deterministic config and URL/header parsing pieces.

use std::time::{Duration, SystemTime};

/// `PaceUser`: userinfo of the marker injected into a proxy URL.
pub const PACE_USER: &str = "artex-pace";

/// `MaxJitter`: cap of one send's jitter.
pub const MAX_JITTER_MS: u64 = 250;

/// `Profile`: one noise level (the zero / "off" profile is a strict passthrough).
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub rps: f64,
    pub burst: i32,
    pub max_per_host: i32,
    pub max_global: i32,
    pub jitter: f64,
    pub cache_ttl: Duration,
    pub max_retries: i32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub user_agent: String,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "off".to_string(),
            rps: 0.0,
            burst: 0,
            max_per_host: 0,
            max_global: 0,
            jitter: 0.0,
            cache_ttl: Duration::ZERO,
            max_retries: 0,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            user_agent: String::new(),
        }
    }
}

/// `Profiles`: named levels besides off.
pub fn profiles() -> std::collections::BTreeMap<&'static str, Profile> {
    let make =
        |name: &str, rps, burst, per_host, global, jitter, cache_ms, retries, base_ms, max_ms| {
            Profile {
                name: name.to_string(),
                rps,
                burst,
                max_per_host: per_host,
                max_global: global,
                jitter,
                cache_ttl: Duration::from_millis(cache_ms),
                max_retries: retries,
                base_delay: Duration::from_millis(base_ms),
                max_delay: Duration::from_millis(max_ms),
                user_agent: String::new(),
            }
        };
    [
        (
            "quiet",
            make("quiet", 2.0, 4, 1, 8, 0.5, 60_000, 3, 2_000, 60_000),
        ),
        (
            "balanced",
            make("balanced", 5.0, 10, 2, 16, 0.2, 30_000, 3, 1_000, 30_000),
        ),
        (
            "aggressive",
            make("aggressive", 20.0, 40, 4, 32, 0.1, 10_000, 2, 500, 10_000),
        ),
    ]
    .into_iter()
    .collect()
}

/// `ProfileByName`: case/space-insensitive name lookup. A profile other than
/// off carries `user_agent`; off is always a strict passthrough. A UA with a
/// control character is ignored (Go logs and drops it).
pub fn profile_by_name(name: &str, user_agent: &str) -> (Profile, bool) {
    let name = name.trim().to_lowercase();
    if name == "off" {
        return (Profile::default(), true);
    }
    let Some(mut p) = profiles().remove(name.as_str()) else {
        return (Profile::default(), false);
    };
    let ua = user_agent.trim();
    if !ua.chars().any(|c| c.is_control()) {
        p.user_agent = ua.to_string();
    }
    (p, true)
}

/// `ProfileFromEnv` (pure core): unset/off/unknown name -> off.
pub fn profile_from_env(name: &str, user_agent: &str) -> Profile {
    profile_by_name(name, user_agent).0
}

/// `MarkProxyURL`: add the marker userinfo unless `raw` is unparsable, has no
/// host, or already carries a userinfo.
pub fn mark_proxy_url(raw: &str) -> String {
    let Ok(mut u) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    if u.host_str().unwrap_or("").is_empty() || !u.username().is_empty() || u.password().is_some() {
        return raw.to_string();
    }
    // Go's url.URL keeps an empty path empty; the url crate serializes "/".
    let had_path = raw
        .split_once("://")
        .map(|(_, rest)| rest.contains('/'))
        .unwrap_or(false);
    u.set_username(PACE_USER).ok();
    u.set_password(Some(PACE_USER)).ok();
    let mut out = u.to_string();
    if !had_path && u.query().is_none() && u.fragment().is_none() && out.ends_with('/') {
        out.pop();
    }
    out
}

/// `IsPaceMarker`: whether a `Proxy-Authorization` value is Basic credentials
/// whose user is `PaceUser` (password is not checked).
pub fn is_pace_marker(proxy_authorization: &str) -> bool {
    let mut parts = proxy_authorization.trim().splitn(2, ' ');
    let (Some(scheme), Some(enc)) = (parts.next(), parts.next()) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("basic") {
        return false;
    }
    let Ok(dec) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, enc.trim())
    else {
        return false;
    };
    let user = dec.split(|&b| b == b':').next().unwrap_or(&[]);
    user == PACE_USER.as_bytes()
}

/// `HostKey`: lowercased host, with the port when it is not the scheme default.
pub fn host_key(u: &url::Url) -> String {
    let host = u
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_lowercase();
    let scheme = u.scheme();
    let default =
        (scheme == "http" && u.port() == Some(80)) || (scheme == "https" && u.port() == Some(443));
    match u.port() {
        Some(port) if !default => format!("{host}:{port}"),
        _ => host,
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse the `http.TimeFormat` HTTP date (RFC 1123, e.g. `Mon, 02 Jan 2006
/// 15:04:05 GMT`) into a Unix timestamp in seconds. Go's `http.ParseTime`
/// also accepts RFC 850 and asctime, but those are legacy and rare; only the
/// RFC 1123 form is handled here.
fn parse_http_time(v: &str) -> Option<i64> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let tail = v.trim().rsplit(',').next()?.trim();
    let parts: Vec<&str> = tail.split_whitespace().collect();
    if parts.len() != 5 || !parts[4].eq_ignore_ascii_case("gmt") {
        return None;
    }
    let day: i64 = parts[0].parse().ok()?;
    let mon = months
        .iter()
        .position(|m| m.eq_ignore_ascii_case(parts[1]))? as i64
        + 1;
    let year: i64 = parts[2].parse().ok()?;
    let clock: Vec<&str> = parts[3].split(':').collect();
    if clock.len() != 3 {
        return None;
    }
    let hh: i64 = clock[0].parse().ok()?;
    let mm: i64 = clock[1].parse().ok()?;
    let ss: i64 = clock[2].parse().ok()?;
    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=59).contains(&ss) {
        return None;
    }
    let days = days_from_civil(year, mon, day);
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// `backoff` (pure core): wait after a 429/503. Numeric Retry-After (seconds)
/// or an HTTP date, else BaseDelay doubled per attempt, capped at MaxDelay.
/// `ok=false` when Retry-After asks for more than MaxDelay.
pub fn backoff(
    retry_after: Option<&str>,
    attempt: u32,
    profile: &Profile,
    now: SystemTime,
) -> (Duration, bool) {
    if let Some(v) = retry_after.map(str::trim) {
        if !v.is_empty() {
            if let Ok(n) = v.parse::<i64>() {
                if n >= 0 {
                    let d = Duration::from_secs(n as u64);
                    return (d, d <= profile.max_delay);
                }
            }
            if let Some(ts) = parse_http_time(v) {
                let target = SystemTime::UNIX_EPOCH + Duration::from_secs(ts as u64);
                let d = target.duration_since(now).unwrap_or(Duration::ZERO);
                return (d, d <= profile.max_delay);
            }
        }
    }
    let mut d = profile
        .base_delay
        .checked_mul(1u32 << attempt)
        .unwrap_or(Duration::MAX);
    if d.is_zero() || d > profile.max_delay {
        d = profile.max_delay;
    }
    (d, true)
}
