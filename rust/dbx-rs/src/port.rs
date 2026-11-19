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

// ---------------------------------------------------------------------------
// Batch 9ad: pure predicate/scan helpers in `db/` (deterministic, no net/url).
// ---------------------------------------------------------------------------

/// `ModelReasonPrefix` (`db/intercept.go`).
pub const MODEL_REASON_PREFIX: &str = "[model]";

/// `legacyModelReasonPrefix` (`db/intercept.go`) — the legacy Chinese marker.
pub const LEGACY_MODEL_REASON_PREFIX: &str = "[模型]";

/// `sealedJSONKey` (`db/secrets.go`): the JSON key wrapping a sealed secret.
pub const SEALED_JSON_KEY: &str = "__artex_enc";

/// `isModelReason` (`db/intercept.go`): whether `reason` carries the model marker
/// (current lower-cased `[model]` prefix or the legacy `[模型]` prefix as-is).
pub fn is_model_reason(reason: &str) -> bool {
    strings::go_to_lower(reason).starts_with(MODEL_REASON_PREFIX)
        || reason.starts_with(LEGACY_MODEL_REASON_PREFIX)
}

/// `inheritedIntentTerminal` (`db/...`): whether `state` is one of the four
/// terminal intent states.
pub fn inherited_intent_terminal(state: &str) -> bool {
    matches!(state, "done" | "blocked" | "exhausted" | "stopped")
}

/// `assetDisplayName` (`db/...`): the human-facing name for an asset, chosen by
/// type with a first-non-blank fallback.
pub fn asset_display_name(
    typ: &str,
    domain: &str,
    ip: &str,
    url: &str,
    app_name: &str,
    bundle_id: &str,
) -> String {
    fn pick(vals: &[&str]) -> String {
        for v in vals {
            if !strings::go_trim_space(v).is_empty() {
                return (*v).to_string();
            }
        }
        String::new()
    }
    match typ {
        "root_domain" | "subdomain" => domain.to_string(),
        "ip" => ip.to_string(),
        "app" => pick(&[app_name, bundle_id]),
        "service" | "endpoint" => pick(&[url, domain, ip]),
        _ => pick(&[domain, ip, url, app_name]),
    }
}

/// `isKnownField` (`db/asset_dsl.go`): whether `f` (lower-cased) is a recognized
/// DSL field — the union of the string/array/numeric field maps plus the id
/// columns.
pub fn is_known_field(f: &str) -> bool {
    let f = strings::go_to_lower(f);
    matches!(
        f.as_str(),
        "domain"
            | "root_domain"
            | "ip"
            | "url"
            | "page_title"
            | "title"
            | "icp"
            | "service_name"
            | "app_name"
            | "bundle_id"
            | "category"
            | "app_icp"
            | "method"
            | "service_type"
            | "record_type"
            | "technology"
            | "technologies"
            | "tech"
            | "port"
            | "status_code"
            | "status"
            | "company_id"
            | "task_id"
    )
}

/// `sealedJSON` (`db/secrets.go`): decode a JSON object that wraps exactly one
/// [`SEALED_JSON_KEY`] string value which is encrypted; returns `(value, true)`
/// or `("", false)`. Uses the already-portable [`is_encrypted`].
pub fn sealed_json(raw: &str) -> (String, bool) {
    let needle = format!("\"{SEALED_JSON_KEY}\"");
    if !raw.contains(&needle) {
        return (String::new(), false);
    }
    let v: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return (String::new(), false),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return (String::new(), false),
    };
    if obj.len() != 1 {
        return (String::new(), false);
    }
    let s = match obj.get(SEALED_JSON_KEY).and_then(|x| x.as_str()) {
        Some(s) => s,
        None => return (String::new(), false),
    };
    if !is_encrypted(s) {
        return (String::new(), false);
    }
    (s.to_string(), true)
}

/// `looksLikeIPAddress` (`db/...`): a heuristic (not `net/netip`) that recognises
/// IPv6-looking colon strings and dotted IPv4-ish strings.
pub fn looks_like_ip_address(value: &str) -> bool {
    let value = strings::go_trim_space(value);
    if value.contains("://") || value.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if value.matches(':').count() >= 2 {
        let mut valid_segments = 0;
        for part in value.split(':') {
            if part.is_empty() {
                if valid_segments > 0 || value.starts_with("::") {
                    return true;
                }
                continue;
            }
            if part.len() > 4 {
                return false;
            }
            for r in part.chars() {
                if !(r.is_ascii_digit() || ('a'..='f').contains(&r) || ('A'..='F').contains(&r)) {
                    return false;
                }
            }
            valid_segments += 1;
            if valid_segments >= 2 {
                return true;
            }
        }
        return false;
    }
    if !value.contains('.') {
        return false;
    }
    for r in value.chars() {
        if r != '.' && !r.is_ascii_digit() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Batch 9ak: task asset/company/category input normalizers in `db/` (pure
// de-dup / validate / normalise). Error strings reproduce the Go sentinel wraps
// (`ErrTaskCompanyIDsInvalid`, `ErrTaskAssetInvalid`, `ErrTaskCategoryInvalid`).
// ---------------------------------------------------------------------------

const MAX_TASK_COMPANY_COUNT: usize = 32;
const MAX_TASK_ASSET_MUTATION_COUNT: usize = 100;
const MAX_TASK_CATEGORY_NAME_RUNES: usize = 80;
const MAX_TASK_ASSET_SUMMARY_RUNES: usize = 500;
const DEFAULT_TASK_ASSET_SOURCE: &str = "system";

/// `NormalizeTaskCompanyIDs` (`db/tasks.go`): de-duplicate `ids` keeping
/// first-seen order, rejecting non-positive ids and more than
/// [`MAX_TASK_COMPANY_COUNT`] uniques. `Ok(None)` mirrors Go's `nil` slice for
/// an empty input, so it serialises as JSON `null`, not `[]`.
pub fn normalize_task_company_ids(ids: &[i64]) -> Result<Option<Vec<i64>>, String> {
    if ids.is_empty() {
        return Ok(None);
    }
    let mut seen = std::collections::HashSet::new();
    let mut normalized: Vec<i64> = Vec::new();
    for &id in ids {
        if id <= 0 {
            return Err("invalid task company ids: company id must be positive".to_string());
        }
        if !seen.insert(id) {
            continue;
        }
        normalized.push(id);
        if normalized.len() > MAX_TASK_COMPANY_COUNT {
            return Err(format!(
                "invalid task company ids: got more than {MAX_TASK_COMPANY_COUNT} unique companies"
            ));
        }
    }
    Ok(Some(normalized))
}

/// `normalizeTaskAssetIDs` (`db/task_assets.go`): de-duplicate `ids` keeping
/// first-seen order, rejecting an empty input, non-positive ids and more than
/// [`MAX_TASK_ASSET_MUTATION_COUNT`] uniques.
pub fn normalize_task_asset_ids(ids: &[i64]) -> Result<Vec<i64>, String> {
    if ids.is_empty() {
        return Err("invalid task asset association: asset_ids is required".to_string());
    }
    let mut seen = std::collections::HashSet::new();
    let mut normalized: Vec<i64> = Vec::new();
    for &id in ids {
        if id <= 0 {
            return Err("invalid task asset association: asset id must be positive".to_string());
        }
        if !seen.insert(id) {
            continue;
        }
        normalized.push(id);
        if normalized.len() > MAX_TASK_ASSET_MUTATION_COUNT {
            return Err(format!(
                "invalid task asset association: at most {MAX_TASK_ASSET_MUTATION_COUNT} assets per request"
            ));
        }
    }
    Ok(normalized)
}

/// `normalizeTaskCategoryName` (`db/task_categories.go`): collapse whitespace in
/// `name` and return `(name, lower(name))`, rejecting empty or over-long names.
pub fn normalize_task_category_name(name: &str) -> Result<(String, String), String> {
    let name = strings::go_fields_join(name);
    if name.is_empty() {
        return Err("invalid task category: name is required".to_string());
    }
    if name.chars().count() > MAX_TASK_CATEGORY_NAME_RUNES {
        return Err(format!(
            "invalid task category: name exceeds {MAX_TASK_CATEGORY_NAME_RUNES} characters"
        ));
    }
    let lower = strings::go_to_lower(&name);
    Ok((name, lower))
}

/// `normalizeTaskAssetSource` (`db/task_assets.go`): lower-case/trim `source`
/// (defaulting to `"system"` when empty) and trim `summary`, rejecting an
/// over-long summary.
pub fn normalize_task_asset_source(
    source: &str,
    summary: &str,
) -> Result<(String, String), String> {
    let mut source = strings::go_trim_space(&strings::go_to_lower(source)).to_string();
    if source.is_empty() {
        source = DEFAULT_TASK_ASSET_SOURCE.to_string();
    }
    let summary = strings::go_trim_space(summary).to_string();
    if summary.chars().count() > MAX_TASK_ASSET_SUMMARY_RUNES {
        return Err(format!(
            "invalid task asset association: source summary exceeds {MAX_TASK_ASSET_SUMMARY_RUNES} characters"
        ));
    }
    Ok((source, summary))
}

// ---------------------------------------------------------------------------
// Batch 9al: byte-level text cleaners + a tiny token reader in `db/`. `utf8Clean`
// reproduces Go `strings.ToValidUTF8(s, "\uFFFD")` (each maximal run of invalid
// bytes collapses to a single replacement), `jsonbClean` strips JSON `\u0000`
// escapes (respecting backslash parity), and `readToken` is the byte-wise scanner.
// ---------------------------------------------------------------------------

/// Width of the valid UTF-8 rune starting at `b[i]`, or `None` if the bytes there
/// are not a valid encoding. Mirrors Go `utf8.DecodeRuneInString` acceptance
/// (rejects overlong forms, surrogates and > U+10FFFF, returning width 1 invalid).
fn rune_width(b: &[u8], i: usize) -> Option<usize> {
    let b0 = b[i];
    if b0 < 0x80 {
        return Some(1);
    }
    let cont = |x: u8| (0x80..=0xBF).contains(&x);
    match b0 {
        0xC2..=0xDF => (i + 1 < b.len() && cont(b[i + 1])).then_some(2),
        0xE0 => {
            (i + 2 < b.len() && (0xA0..=0xBF).contains(&b[i + 1]) && cont(b[i + 2])).then_some(3)
        }
        0xE1..=0xEC | 0xEE..=0xEF => {
            (i + 2 < b.len() && cont(b[i + 1]) && cont(b[i + 2])).then_some(3)
        }
        0xED => {
            (i + 2 < b.len() && (0x80..=0x9F).contains(&b[i + 1]) && cont(b[i + 2])).then_some(3)
        }
        0xF0 => (i + 3 < b.len()
            && (0x90..=0xBF).contains(&b[i + 1])
            && cont(b[i + 2])
            && cont(b[i + 3]))
        .then_some(4),
        0xF1..=0xF3 => {
            (i + 3 < b.len() && cont(b[i + 1]) && cont(b[i + 2]) && cont(b[i + 3])).then_some(4)
        }
        0xF4 => (i + 3 < b.len()
            && (0x80..=0x8F).contains(&b[i + 1])
            && cont(b[i + 2])
            && cont(b[i + 3]))
        .then_some(4),
        _ => None,
    }
}

/// Go `strings.ToValidUTF8(b, "\uFFFD")` over bytes: replace each maximal run of
/// invalid UTF-8 bytes with a single U+FFFD.
fn to_valid_utf8(b: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut invalid = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            out.push(c);
            invalid = false;
            i += 1;
            continue;
        }
        match rune_width(b, i) {
            Some(w) => {
                out.extend_from_slice(&b[i..i + w]);
                invalid = false;
                i += w;
            }
            None => {
                i += 1;
                if !invalid {
                    invalid = true;
                    out.extend_from_slice(replacement);
                }
            }
        }
    }
    out
}

/// `utf8Clean` (`db/...`): drop NUL bytes, then make the result valid UTF-8 by
/// replacing each run of invalid bytes with U+FFFD.
pub fn utf8_clean(s: &[u8]) -> Vec<u8> {
    let cleaned: Vec<u8> = if s.contains(&0) {
        s.iter().copied().filter(|&b| b != 0).collect()
    } else {
        s.to_vec()
    };
    to_valid_utf8(&cleaned, "\u{FFFD}".as_bytes())
}

/// `jsonbClean` (`db/...`): strip JSON `\u0000` escape sequences from `b`,
/// tracking backslash parity so an escaped backslash (`\\u0000`) is left intact.
pub fn jsonb_clean(b: &[u8]) -> Vec<u8> {
    let needle = |i: usize| {
        i + 5 < b.len()
            && b[i] == b'\\'
            && b[i + 1] == b'u'
            && b[i + 2] == b'0'
            && b[i + 3] == b'0'
            && b[i + 4] == b'0'
            && b[i + 5] == b'0'
    };
    if !b.windows(6).any(|w| w == b"\\u0000") {
        return b.to_vec();
    }
    let mut out = Vec::with_capacity(b.len());
    let mut bs = 0u32;
    let mut i = 0;
    while i < b.len() {
        if bs.is_multiple_of(2) && needle(i) {
            i += 6;
            bs = 0;
            continue;
        }
        if b[i] == b'\\' {
            bs += 1;
        } else {
            bs = 0;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Go `unicode.IsSpace(rune(c))` for a single byte `c` (Latin-1 range): the
/// ASCII whitespace plus NEL (0x85) and NBSP (0xA0).
fn go_is_space_byte(c: u8) -> bool {
    matches!(c, 0x09..=0x0D | 0x20 | 0x85 | 0xA0)
}

/// `readToken` (`db/...`): read one token from `s` starting at byte `pos` — a
/// `"..."`-quoted run, or a run up to the next whitespace / `(` / `)` — returning
/// `(token, next_pos)`.
pub fn read_token(s: &str, pos: usize) -> (String, usize) {
    let b = s.as_bytes();
    if pos >= b.len() {
        return (String::new(), pos);
    }
    if b[pos] == b'"' {
        let mut i = pos + 1;
        while i < b.len() && b[i] != b'"' {
            i += 1;
        }
        let val = String::from_utf8_lossy(&b[pos + 1..i]).into_owned();
        if i < b.len() {
            i += 1;
        }
        return (val, i);
    }
    let mut i = pos;
    while i < b.len() && !go_is_space_byte(b[i]) && b[i] != b'(' && b[i] != b')' {
        i += 1;
    }
    (String::from_utf8_lossy(&b[pos..i]).into_owned(), i)
}

// ---------------------------------------------------------------------------
// Batch 9am: intercept-source classifier in `db/` (reuses `isModelReason`).
// ---------------------------------------------------------------------------

/// `interceptSource` (`db/...`): classify an intercept decision's origin — `"rule"`
/// when a rule id is set, else `"model"` when the reason is a model reason, else
/// `"unknown"`.
pub fn intercept_source(rule_id: i64, reason: &str) -> String {
    if rule_id != 0 {
        return "rule".to_string();
    }
    if is_model_reason(reason) {
        return "model".to_string();
    }
    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// Batch 9ao: the asset-ids archive query in `db/` (a constant SQL string).
// ---------------------------------------------------------------------------

/// `archiveAssetIDsQuery` (`db/...`): the constant SQL that resolves the asset
/// ids archived for a task (unions direct links, exploration anchors and finding
/// `asset_ids`), returned byte-for-byte.
pub fn archive_asset_ids_query() -> String {
    r#"SELECT id FROM assets WHERE $1=ANY(task_ids)
UNION SELECT link.asset_id FROM task_asset_links link WHERE link.task_id=$1
UNION SELECT anchor.asset_id FROM exploration_anchors anchor
      JOIN exploration_nodes node ON node.id=anchor.node_id WHERE node.exploration_id=$2
UNION SELECT value::bigint FROM findings finding
      CROSS JOIN LATERAL jsonb_array_elements_text(
        CASE WHEN jsonb_typeof(finding.asset_ids)='array' THEN finding.asset_ids ELSE '[]'::jsonb END
      ) value WHERE finding.task_id=$1 AND value ~ '^[0-9]+$'"#.to_string()
}
