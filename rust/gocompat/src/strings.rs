//! Faithful ports of the Go `strings` helpers the ARTEX units rely on.
//!
//! Each function mirrors the corresponding Go stdlib behavior closely enough for
//! byte-identical output on the recorded inputs:
//! - [`go_trim_space`]  = `strings.TrimSpace`
//! - [`go_trim_left`]   = `strings.TrimLeft(s, cutset)`
//! - [`go_fields_join`] = `strings.Join(strings.Fields(s), " ")`
//! - [`go_to_upper`] / [`go_to_lower`] = `strings.ToUpper` / `strings.ToLower`
//! - [`go_equal_fold`]  = `strings.EqualFold`

/// Go `strings.TrimSpace`.
pub fn go_trim_space(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// Go `strings.TrimLeft(s, cutset)`.
pub fn go_trim_left<'a>(s: &'a str, cutset: &str) -> &'a str {
    let chars: Vec<char> = cutset.chars().collect();
    s.trim_start_matches(|c: char| chars.contains(&c))
}

/// Go `strings.TrimRight(s, cutset)`.
pub fn go_trim_right<'a>(s: &'a str, cutset: &str) -> &'a str {
    let chars: Vec<char> = cutset.chars().collect();
    s.trim_end_matches(|c: char| chars.contains(&c))
}

/// Go `strings.TrimSuffix(s, suffix)`: strip one trailing `suffix` if present.
pub fn go_trim_suffix<'a>(s: &'a str, suffix: &str) -> &'a str {
    s.strip_suffix(suffix).unwrap_or(s)
}

/// Go `strings.HasSuffix(s, suffix)`.
pub fn go_has_suffix(s: &str, suffix: &str) -> bool {
    s.ends_with(suffix)
}

/// Go `strings.Join(strings.Fields(s), " ")`: collapse whitespace runs to one space.
pub fn go_fields_join(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Go `strings.ToUpper`: each rune under Go's simple 1:1 case mapping.
pub fn go_to_upper(s: &str) -> String {
    s.chars().map(crate::gocase::simple_to_upper).collect()
}

/// Go `strings.ToLower`: each rune under Go's simple 1:1 case mapping.
pub fn go_to_lower(s: &str) -> String {
    s.chars().map(crate::gocase::simple_to_lower).collect()
}

/// Go `strings.EqualFold`: Unicode case-folding comparison.
pub fn go_equal_fold(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}
/// Go `unicode.IsSpace`: the White_Space set (ASCII ws, NEL, NBSP, and the
/// Unicode Zs/Zl/Zp separators).
pub fn go_is_space(r: char) -> bool {
    matches!(r,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' |
        '\u{85}' | '\u{a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' |
        '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}
