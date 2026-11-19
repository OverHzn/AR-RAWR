//! Minimal RFC3339 reader/writer for the db layer — Go `time.Time` JSON codec.
//!
//! Go marshals `time.Time` to JSON as a quoted RFC3339 string
//! (`2006-01-02T15:04:05.999999999Z07:00`). The archive manifest
//! (`task_archives.aggregate_stats`) stores such timestamps, and the metering
//! merge (`mergeArchivedSkillStats`) compares them against live `SystemTime`
//! values, so the db layer needs an RFC3339 ⇄ `SystemTime` codec. There is no
//! `chrono`/`time` crate available offline, so this is a small, self-contained
//! implementation covering the subset Go emits/parses here (UTC `Z` or numeric
//! offset, optional fractional seconds).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parse an RFC3339 timestamp into a [`SystemTime`]. Returns `None` on any
/// malformed input (the caller treats an unparseable archive stamp as "absent",
/// mirroring Go's `json.Unmarshal` failing the whole row — but we are lenient
/// per-field so a single bad stamp cannot drop an otherwise-usable summary).
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let b = s.as_bytes();
    // Minimal: YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM). Require at least the date,
    // the 'T', and the time; a zone is mandatory in RFC3339.
    if b.len() < 20 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if b.get(4) != Some(&b'-') {
        return None;
    }
    let month: i64 = s.get(5..7)?.parse().ok()?;
    if b.get(7) != Some(&b'-') {
        return None;
    }
    let day: i64 = s.get(8..10)?.parse().ok()?;
    if b.get(10) != Some(&b'T') && b.get(10) != Some(&b't') {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if b.get(13) != Some(&b':') {
        return None;
    }
    let min: i64 = s.get(14..16)?.parse().ok()?;
    if b.get(16) != Some(&b':') {
        return None;
    }
    let sec: i64 = s.get(17..19)?.parse().ok()?;

    // Optional fractional seconds: '.' followed by 1..=9 digits.
    let mut idx = 19;
    let mut nanos: i64 = 0;
    if b.get(idx) == Some(&b'.') {
        idx += 1;
        let start = idx;
        while idx < b.len() && b[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac = s.get(start..idx)?;
        if frac.is_empty() || frac.len() > 9 {
            return None;
        }
        // Right-pad to nanoseconds (e.g. ".5" → 500_000_000).
        let scaled = format!("{:0<9}", frac);
        nanos = scaled.get(0..9)?.parse().ok()?;
    }

    // Zone: 'Z'/'z' or ±HH:MM.
    let offset_secs: i64 = match b.get(idx) {
        Some(&b'Z') | Some(&b'z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            idx += 1;
            let oh: i64 = s.get(idx..idx + 2)?.parse().ok()?;
            if b.get(idx + 2) != Some(&b':') {
                return None;
            }
            let om: i64 = s.get(idx + 3..idx + 5)?.parse().ok()?;
            let total = oh * 3600 + om * 60;
            if sign == b'+' {
                total
            } else {
                -total
            }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day)?;
    let local_secs = days * 86_400 + hour * 3600 + min * 60 + sec;
    // UTC = local - offset.
    let utc_secs = local_secs - offset_secs;
    let base = if utc_secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(utc_secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs((-utc_secs) as u64)
    };
    Some(base + Duration::new(0, nanos as u32))
}

/// Format a [`SystemTime`] as a UTC RFC3339 string with nanosecond precision,
/// matching Go's `time.Time` JSON shape (`2006-01-02T15:04:05.999999999Z`).
pub fn format_rfc3339(t: SystemTime) -> String {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = dur.as_secs() as i64;
    let nanos = dur.subsec_nanos();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    if nanos == 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
    } else {
        let frac = format!("{nanos:09}");
        let frac = frac.trim_end_matches('0');
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{frac}Z")
    }
}

/// Days since the Unix epoch for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`). Returns `None` for an out-of-range month/day.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of [`days_from_civil`]: `(year, month, day)` for days since the epoch.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_various() {
        for s in [
            "2024-01-02T03:04:05Z",
            "2024-01-02T03:04:05.5Z",
            "2024-01-02T03:04:05.123456789Z",
            "1970-01-01T00:00:00Z",
            "2024-06-15T12:00:00+07:00",
            "2024-06-15T12:00:00-05:30",
        ] {
            let t = parse_rfc3339(s).unwrap_or_else(|| panic!("parse {s}"));
            let out = format_rfc3339(t);
            let t2 = parse_rfc3339(&out).unwrap_or_else(|| panic!("reparse {out}"));
            assert_eq!(t, t2, "roundtrip {s} -> {out}");
        }
    }

    #[test]
    fn known_epoch() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z").unwrap(), UNIX_EPOCH);
        // 1970-01-01T00:00:00+01:00 is one hour earlier in UTC.
        assert_eq!(
            parse_rfc3339("1970-01-01T01:00:00+01:00").unwrap(),
            UNIX_EPOCH
        );
        // Differential value from Go: json.Unmarshal(`2024-06-15T12:00:00+07:00`)
        // → Unix() == 1718427600.
        assert_eq!(
            parse_rfc3339("2024-06-15T12:00:00+07:00")
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1_718_427_600
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_rfc3339("").is_none());
        assert!(parse_rfc3339("not-a-time").is_none());
        assert!(parse_rfc3339("2024-13-01T00:00:00Z").is_none());
    }
}

/// Serde adapter: `Option<SystemTime>` ⇄ a Go-style RFC3339 JSON string.
///
/// Used by [`crate::db::skill_usage::SkillStat`] and the archive aggregate so a
/// Go `time.Time` JSON field round-trips through the manifest. `None` ⇄ JSON
/// `null`; a present value ⇄ [`format_rfc3339`]/`parse_rfc3339`.
pub mod opt_system_time_rfc3339 {
    use super::{format_rfc3339, parse_rfc3339};
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::SystemTime;

    pub fn serialize<S: Serializer>(
        v: &Option<SystemTime>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match v {
            None => s.serialize_none(),
            Some(t) => s.serialize_str(&format_rfc3339(*t)),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<SystemTime>, D::Error> {
        let raw = Option::<String>::deserialize(d)?;
        match raw {
            None => Ok(None),
            Some(s) => Ok(parse_rfc3339(&s)),
        }
    }
}
