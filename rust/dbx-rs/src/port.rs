//! Rust port of the unexported, pure helper functions in the Go `db/` package
//! that are dependency-free (no `net/netip`, `net/url`, publicsuffix, or the DSL
//! parser). Shared Go-stdlib pieces would come from `gocompat`, but none are
//! needed here.

use gocompat::strings;

/// `nullIfEmpty` (`db/...`): `None` (JSON `null`) for the empty string, otherwise
/// the string itself — used to store a nullable text column.
pub fn null_if_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// `isIdentStart` (`db/asset_dsl.go`): whether byte `c` may start a DSL identifier
/// (`_`, `a`–`z`, `A`–`Z`).
pub fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

/// `isIdentChar` (`db/asset_dsl.go`): whether byte `c` may continue a DSL
/// identifier (an identifier start, or a digit `0`–`9`).
pub fn is_ident_char(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// `marshalStringArray` (`db/...`): render a Go `[]string` as a Postgres text[]
/// literal — empty → `{}`, otherwise `{"a","b"}`, with only `"` escaped to
/// `\"` (exactly Go's `strings.ReplaceAll(s, "\"", "\\\"")`).
pub fn marshal_string_array(items: &[String]) -> String {
    if items.is_empty() {
        return "{}".to_string();
    }
    let body: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.replace('"', "\\\"")))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// `nullableInt` (`db/...`): `None` (JSON `null`) for 0, otherwise the integer —
/// used to store a nullable integer column.
pub fn nullable_int(v: i64) -> Option<i64> {
    if v == 0 { None } else { Some(v) }
}

// ---------------------------------------------------------------------------
// Batch 9y: pure scalar/string helpers in `db/` (no net/netip/url/DSL deps).
// Go-stdlib string semantics come from `gocompat::strings`.
// ---------------------------------------------------------------------------

/// `MinPlanHeartbeatSeconds` (`db/tasks.go`): the planner heartbeat floor (600 s).
pub const MIN_PLAN_HEARTBEAT_SECONDS: i64 = 600;

/// `truncateRunes` (`db/...`): trim, then cut to at most `n` runes with a trailing
/// ellipsis when truncated. Callers keep `n >= 0`.
pub fn truncate_runes(s: &str, n: i64) -> String {
    let s = strings::go_trim_space(s);
    let r: Vec<char> = s.chars().collect();
    if (r.len() as i64) <= n {
        return s.to_string();
    }
    let mut out: String = r[..n as usize].iter().collect();
    out.push('…');
    out
}

/// `normalizeKeyword` (`db/...`): lower-cased, whitespace-collapsed keyword.
pub fn normalize_keyword(value: &str) -> String {
    strings::go_to_lower(&strings::go_fields_join(value))
}

/// `NormalizeCoverageField` (`db/...`): map every control rune and the Unicode
/// line/paragraph separators to a space, then collapse whitespace and trim.
pub fn normalize_coverage_field(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|r| {
            if r.is_control() || r == '\u{2028}' || r == '\u{2029}' {
                ' '
            } else {
                r
            }
        })
        .collect();
    strings::go_fields_join(&mapped)
}

/// `normRiskArea` (`db/...`): lower-cased [`normalize_coverage_field`].
pub fn norm_risk_area(s: &str) -> String {
    strings::go_to_lower(&normalize_coverage_field(s))
}

/// `normalizeHeartbeat` (`db/...`): clamp a heartbeat interval up to the floor.
pub fn normalize_heartbeat(sec: i64) -> i64 {
    if sec < MIN_PLAN_HEARTBEAT_SECONDS {
        MIN_PLAN_HEARTBEAT_SECONDS
    } else {
        sec
    }
}

/// `normalizeRuleAction` (`db/...`): `"allow"` stays `"allow"`, everything else is
/// `"block"`.
pub fn normalize_rule_action(action: &str) -> String {
    if action == "allow" {
        "allow".to_string()
    } else {
        "block".to_string()
    }
}

/// `sameOptionalInt64` (`db/...`): both nil, or both set and equal. Modeled with
/// `Option<i64>` (its `PartialEq` matches exactly).
pub fn same_optional_i64(a: Option<i64>, b: Option<i64>) -> bool {
    a == b
}

/// `isUTF8Start` (`db/...`): whether byte `b` is not a UTF-8 continuation byte
/// (`b & 0xC0 != 0x80`).
pub fn is_utf8_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

/// `companyNKey` (`db/...`): whitespace-collapsed, lower-cased company identity.
pub fn company_n_key(name: &str) -> String {
    strings::go_fields_join(&strings::go_to_lower(name))
}

/// `taskTemplateName` (`db/...`): whitespace-collapsed template name.
pub fn task_template_name(name: &str) -> String {
    strings::go_fields_join(name)
}

/// `taskTemplateNKey` (`db/...`): lower-cased [`task_template_name`] (unique index
/// identity).
pub fn task_template_n_key(name: &str) -> String {
    strings::go_to_lower(&task_template_name(name))
}

/// `lessonExtractCompanyID` (`db/...`): the single company id when exactly one is
/// present, else 0.
pub fn lesson_extract_company_id(company_ids: &[i64]) -> i64 {
    if company_ids.len() == 1 {
        company_ids[0]
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Batch 9z: pure scalar/string helpers in `db/` (deterministic, no net/netip,
// map-order, or random deps). Go-stdlib strings come from `gocompat::strings`.
// ---------------------------------------------------------------------------

/// `secretPrefix` (`db/secrets.go`): marker prefixed to sealed secret columns.
pub const SECRET_PREFIX: &str = "enc:v1:";

/// `assetKey` (`db/...`): the `"a:<id>"` containment key for an asset id.
pub fn asset_key(id: i64) -> String {
    format!("a:{id}")
}

/// `companyKey` (`db/...`): the `"c:<id>"` containment key for a company id.
pub fn company_key(id: i64) -> String {
    format!("c:{id}")
}

/// `isEncrypted` (`db/secrets.go`): whether `v` carries the [`SECRET_PREFIX`].
pub fn is_encrypted(v: &str) -> bool {
    v.starts_with(SECRET_PREFIX)
}

/// `sameMainSegment` (`db/...`): nil reads as 0; equal after that defaulting.
pub fn same_main_segment(a: Option<i64>, b: Option<i64>) -> bool {
    a.unwrap_or(0) == b.unwrap_or(0)
}

/// `assetIDContainments` (`db/...`): bracket each id (`[1]`, `[2]`, …).
pub fn asset_id_containments(ids: &[i64]) -> Vec<String> {
    ids.iter().map(|id| format!("[{id}]")).collect()
}

/// `mergeAgentKeys` (`db/...`): de-duplicate `current` then `incoming`, dropping
/// empties, preserving first-seen order.
pub fn merge_agent_keys(current: &[String], incoming: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(current.len() + incoming.len());
    for group in [current, incoming] {
        for key in group {
            if key.is_empty() || seen.contains(key) {
                continue;
            }
            seen.insert(key.clone());
            out.push(key.clone());
        }
    }
    out
}

/// `placeholders` (`db/...`): build `$start,$start+1,…` placeholders and echo the
/// ids back as the argument list. Returns `(placeholders, args)`.
pub fn placeholders(start: i64, ids: &[i64]) -> (String, Vec<i64>) {
    let ph: Vec<String> = (0..ids.len())
        .map(|i| format!("${}", start + i as i64))
        .collect();
    (ph.join(","), ids.to_vec())
}

/// `lessonText` (`db/...`): map control runes (r < 32 or r == 127) to spaces,
/// collapse whitespace, then cut to at most `limit` runes (no ellipsis). `limit
/// <= 0` means no cut.
pub fn lesson_text(s: &str, limit: i64) -> String {
    let mapped: String = s
        .chars()
        .map(|r| {
            if (r as u32) < 32 || r as u32 == 127 {
                ' '
            } else {
                r
            }
        })
        .collect();
    let collapsed = strings::go_fields_join(&mapped);
    if limit > 0 {
        let runes: Vec<char> = collapsed.chars().collect();
        if runes.len() as i64 > limit {
            return runes[..limit as usize].iter().collect();
        }
    }
    collapsed
}

/// `truncateNotifyError` (`db/...`): cap at 500 bytes, backing off to a UTF-8
/// boundary so the frontend never renders half a rune, then append an ellipsis.
pub fn truncate_notify_error(msg: &str) -> String {
    const MAX: usize = 500;
    if msg.len() <= MAX {
        return msg.to_string();
    }
    let bytes = msg.as_bytes();
    let mut cut = MAX;
    while cut > 0 && !is_utf8_start(bytes[cut]) {
        cut -= 1;
    }
    let mut out = String::from(&msg[..cut]);
    out.push('…');
    out
}

/// `malformedIPAssetWarningText` (`db/...`): the "N assets have an ip field …"
/// warning, plus " and N more" when `total` exceeds the listed `samples`.
pub fn malformed_ip_asset_warning_text(total: i64, samples: &[String]) -> String {
    let mut warning = format!(
        "{total} assets have an ip field that is not a valid IP; skipped for IP/CIDR scope matching (these assets will not be attributed to a company by subnet rules): {}",
        samples.join(", ")
    );
    if total > samples.len() as i64 {
        warning += &format!(" and {} more", total - samples.len() as i64);
    }
    warning
}

/// `isIPLiteral` (`db/...`): colon → IPv6 literal; otherwise exactly four
/// dot-separated integer parts (no 0–255 range check, matching Go `Atoi`).
pub fn is_ip_literal(host: &str) -> bool {
    if host.contains(':') {
        return true;
    }
    if host.is_empty() {
        return false;
    }
    for part in host.split('.') {
        if part.is_empty() || !go_atoi_ok(part) {
            return false;
        }
    }
    host.matches('.').count() == 3
}

/// Whether Go's `strconv.Atoi(s)` succeeds: optional sign then base-10 digits.
fn go_atoi_ok(s: &str) -> bool {
    !s.is_empty() && s.parse::<i64>().is_ok()
}
