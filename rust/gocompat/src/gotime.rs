//! Faithful port of the Go `time.Time.Format` cases the ARTEX units rely on.
//!
//! [`format_go_time`] renders an RFC3339 input as Go's `"2006-01-02 15:04:05"`
//! layout (the `report` Markdown header). The input is parsed directly into its
//! date/time components without timezone conversion, which matches Go's `Format`
//! when the time is UTC — the only case the record path produces.

/// Format a Go `time.Time` (RFC3339 input) as `"2006-01-02 15:04:05"`.
///
/// The input is an RFC3339 timestamp (or the zero time `"0001-01-01T00:00:00Z"`).
pub fn format_go_time(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "0001-01-01 00:00:00".to_string();
    }
    // Accept "2006-01-02T15:04:05Z" / "...+07:00" / fractional seconds.
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return s.to_string();
    }
    let date = &s[..10]; // YYYY-MM-DD
    let time = &s[11..19]; // HH:MM:SS
    format!("{date} {time}")
}