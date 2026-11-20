//! Network-target extraction from tool inputs — port of `intercept/target_extract.go`.
//!
//! The gate detects a DECLARED target; it cannot prevent an undeclared connection.

use std::net::IpAddr;

use regex::bytes::Regex;

use crate::intercept::target::valid_hostname;
use crate::intercept::target::{
    normalize_host, normalize_target, Target, TARGET_CIDR, TARGET_DOMAIN, TARGET_IP,
};

const EXECUTE_EXTRA_TOOL: &str = "ExecuteExtraTool";

fn no_target_tools() -> std::collections::HashSet<&'static str> {
    [
        "web_search",
        "Read",
        "Write",
        "Edit",
        "MultiEdit",
        "LS",
        "Glob",
        "Grep",
        "TodoWrite",
        "sleep",
        "TaskOutput",
        "TaskStop",
        "TaskList",
        "graph_overview",
        "list_findings",
        "list_facts",
        "node_detail",
        "expand_digest",
        "get_worker_output",
        "get_worker_trace",
        "search_all_worker_traces",
        "add_hint",
        "add_intent",
        "steer_work",
        "kill_work",
        "set_goals",
        "set_constraints",
        "list_goals",
        "prove_goal",
        "goal_met",
        "insert_assets",
        "add_company_scope",
        "add_task_scope",
        "list_assets",
        "list_untested_assets",
        "list_companies",
        "report_finding",
        "record_fact",
        "save_task_credential",
        "list_task_credentials",
        "ingest_bbot",
        "ingest_scan_output",
        "record_coverage",
        "list_tasks",
        "list_llm_profiles",
        "spawn_task",
        "pause_task",
        "get_task_graph",
        "list_task_findings",
        "add_task_hint",
        "get_task_worker_trace",
        "list_task_worker_traces",
        "search_task_worker_traces",
        "get_task_node_detail",
        "update_finding_report",
        "get_finding_traffic",
        "bind_finding_traffic",
        "get_finding_retest_context",
        "record_finding_retest_result",
        "delete_assets_by_host",
        "create_skill",
        "update_skill_file",
        "ask_operator",
        "traffic_search",
        "traffic_get",
        "traffic_blob",
        "traffic_messages",
    ]
    .into_iter()
    .collect()
}

#[derive(Debug, Clone, Default)]
struct TargetList {
    targets: Vec<Target>,
    seen: std::collections::HashSet<Target>,
    urls: Vec<String>,
}

impl TargetList {
    fn add(&mut self, t: Target) {
        if self.seen.insert(t.clone()) {
            self.targets.push(t);
        }
    }
}

fn declared_fields(input: &[u8]) -> (String, String, String) {
    let v: serde_json::Value = serde_json::from_slice(input).unwrap_or(serde_json::Value::Null);
    let get = |k: &str| -> String {
        // Go's json.Unmarshal into a struct is case-insensitive and last-wins.
        if let Some(o) = v.as_object() {
            for (key, val) in o {
                if key.eq_ignore_ascii_case(k) {
                    if let Some(s) = val.as_str() {
                        return s.to_string();
                    }
                }
            }
        }
        String::new()
    };
    (get("url"), get("command"), get("text"))
}

/// Collect every JSON string literal (keys + values) in document order, like Go's
/// `json.Decoder.Token()` — including both values of a duplicated key.
fn scan_json_strings(input: &[u8], out: &mut Vec<String>) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Value,
        Key,
    }
    let mut stack: Vec<Mode> = vec![Mode::Value];
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'{' | b'[' => {
                stack.push(if input[i] == b'{' {
                    Mode::Key
                } else {
                    Mode::Value
                });
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                i += 1;
            }
            b',' => {
                // After a comma in an object the next token is a key again.
                if let Some(m) = stack.last_mut() {
                    if *m == Mode::Key {
                        *m = Mode::Value;
                    }
                }
                i += 1;
            }
            b':' => {
                if let Some(m) = stack.last_mut() {
                    *m = Mode::Value;
                }
                i += 1;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < input.len() {
                    if input[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if input[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                if let Ok(s) = serde_json::from_slice::<String>(&input[start..i]) {
                    out.push(s);
                }
                // After reading a key, the next token is a value.
                if let Some(m) = stack.last_mut() {
                    if *m == Mode::Key {
                        *m = Mode::Value;
                    }
                }
            }
            _ => i += 1,
        }
    }
}

const SHELL_META: &[u8] = b"|;&(){},\\`";

fn url_hosts(authority: &str) -> Vec<String> {
    let host = |s: &str| -> String {
        let s = s.rsplit_once('@').map(|x| x.1).unwrap_or(s);
        let end = s
            .bytes()
            .position(|c| SHELL_META.contains(&c))
            .unwrap_or(s.len());
        s[..end].to_string()
    };
    let mut out = vec![host(authority)];
    if let Some(i) = authority.bytes().position(|c| SHELL_META.contains(&c)) {
        out.push(host(&authority[..i]));
        out.push(host(&authority.replace('\\', "")));
    }
    out
}

fn regexes() -> (Regex, Regex, Regex, Regex, Regex) {
    (
        Regex::new(r#"(?i)\b([a-z][a-z0-9+.\-]*://)([^\s/?#"'<>`]*)[^\s"'<>`]*"#).unwrap(),
        Regex::new(r"\[([0-9A-Fa-f:.]+(?:%[\w.\-]+)?)\](?::(\d{1,5}))?").unwrap(),
        Regex::new(r"\d{1,3}(?:\.\d{1,3}){3}(?:/\d{1,2}|:\d{1,5})?").unwrap(),
        Regex::new(r"[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*(?:%[\w.\-]+)?(?:/\d{1,3})?").unwrap(),
        Regex::new(r#"(?i)(?:localhost|[a-z0-9_](?:[a-z0-9_\-]*[a-z0-9_])?(?:\.[a-z0-9_](?:[a-z0-9_\-]*[a-z0-9_])?)*\.[a-z][a-z0-9\-]*[a-z0-9]):\d{1,5}"#).unwrap(),
    )
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn ends_token(b: &[u8], i: usize) -> bool {
    if i >= b.len() {
        return true;
    }
    if is_word(b[i]) {
        return false;
    }
    b[i] != b'.' || i + 1 >= b.len() || !is_word(b[i + 1])
}

fn valid_port(s: &str) -> i32 {
    match s.parse::<i32>() {
        Ok(n) if (1..=65535).contains(&n) => n,
        _ => 0,
    }
}

fn unmap_addr(s: &str) -> Option<IpAddr> {
    let s = s.split('%').next().unwrap_or(s);
    match s.parse::<IpAddr>().ok()? {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => Some(IpAddr::V4(v4)),
            None => Some(IpAddr::V6(v6)),
        },
        other => Some(other),
    }
}

/// `scan` reads targets of one command string, blanking each form once read.
fn scan(list: &mut TargetList, s: &str) {
    if s.is_empty() {
        return;
    }
    let mut b = s.as_bytes().to_vec();
    let (re_url, re_bracketed, re_ipv4, re_ipv6, re_hostport) = regexes();

    struct Match {
        whole: (usize, usize),
        groups: Vec<Option<(usize, usize)>>,
    }
    let collect = |re: &Regex, b: &[u8]| -> Vec<Match> {
        re.captures_iter(b)
            .map(|c| Match {
                whole: (c.get(0).unwrap().start(), c.get(0).unwrap().end()),
                groups: (0..c.len())
                    .map(|i| c.get(i).map(|m| (m.start(), m.end())))
                    .collect(),
            })
            .collect()
    };
    let blank = |b: &mut [u8], from: usize, to: usize| {
        for x in b.iter_mut().take(to).skip(from) {
            *x = b' ';
        }
    };
    let slice = |b: &[u8], r: Option<(usize, usize)>| -> String {
        r.map(|(a, z)| String::from_utf8_lossy(&b[a..z]).into_owned())
            .unwrap_or_default()
    };

    // URL with a scheme: scheme + authority + rest (rest blanked).
    let mut url_tokens: Vec<String> = Vec::new();
    for m in collect(&re_url, &b) {
        let scheme = slice(&b, m.groups.get(1).copied().flatten());
        let authority = slice(&b, m.groups.get(2).copied().flatten());
        let mut found = false;
        for host in url_hosts(&authority) {
            if let Some(t) = normalize_target(&format!("{scheme}{host}")) {
                if t.kind != TARGET_DOMAIN || valid_hostname(&t.host) {
                    list.add(t);
                    found = true;
                }
            }
        }
        if found {
            url_tokens.push(s[m.whole.0..m.whole.1].to_string());
        }
        let (auth_start, auth_end) = m
            .groups
            .get(2)
            .copied()
            .flatten()
            .unwrap_or((m.whole.0, m.whole.1));
        blank(&mut b, m.whole.0, auth_start);
        blank(&mut b, auth_end, m.whole.1);
    }
    list.urls.extend(url_tokens);

    // [IPv6] and [IPv6]:port.
    for m in collect(&re_bracketed, &b) {
        let addr = slice(&b, m.groups.get(1).copied().flatten());
        let Some(ip) = unmap_addr(&addr) else {
            continue;
        };
        let mut t = Target {
            kind: TARGET_IP.to_string(),
            host: ip.to_string(),
            port: 0,
        };
        if let Some((a, z)) = m.groups.get(2).copied().flatten() {
            t.port = valid_port(&String::from_utf8_lossy(&b[a..z]));
        }
        list.add(t);
        blank(&mut b, m.whole.0, m.whole.1);
    }

    // IPv6 with at least two colons.
    for m in collect(&re_ipv6, &b) {
        let (start, end) = m.whole;
        if start > 0 && is_word(b[start - 1])
            || end < b.len() && (is_word(b[end]) || b[end] == b':')
        {
            continue;
        }
        let text = String::from_utf8_lossy(&b[start..end]).into_owned();
        let Some((kind, host)) = normalize_host(&text, false) else {
            continue;
        };
        if kind == TARGET_DOMAIN || host == "::" {
            continue;
        }
        list.add(Target {
            kind,
            host,
            port: 0,
        });
        blank(&mut b, start, end);
    }

    // IPv4, IPv4:port, IPv4/len.
    for m in collect(&re_ipv4, &b) {
        let (start, end) = m.whole;
        if start > 0 && (is_word(b[start - 1]) || b[start - 1] == b'.') {
            continue;
        }
        let suffix_pos = b[start..end]
            .iter()
            .position(|c| *c == b'/' || *c == b':')
            .map(|i| start + i);
        let addr_end = suffix_pos.unwrap_or(end);
        let mut token_end = end;
        if !ends_token(&b, token_end) {
            token_end = addr_end;
            if !ends_token(&b, token_end) {
                continue;
            }
        }
        let addr = String::from_utf8_lossy(&b[start..addr_end]).into_owned();
        let Some(ip) = addr.parse::<IpAddr>().ok() else {
            continue;
        };
        let mut t = Target {
            kind: TARGET_IP.to_string(),
            host: ip.to_string(),
            port: 0,
        };
        if token_end > addr_end {
            let suffix = String::from_utf8_lossy(&b[addr_end + 1..token_end]).into_owned();
            match b[addr_end] {
                b'/' => {
                    let cidr = String::from_utf8_lossy(&b[start..token_end]).into_owned();
                    if let Some((a, p)) = cidr.split_once('/') {
                        if let (Ok(ip), Ok(ones)) = (a.parse::<IpAddr>(), p.parse::<u32>()) {
                            let bits = if ip.is_ipv4() { 32 } else { 128 };
                            if ones <= bits {
                                let masked = match ip {
                                    IpAddr::V4(v) => {
                                        let mask = if ones == 0 {
                                            0u32
                                        } else {
                                            u32::MAX << (32 - ones)
                                        };
                                        std::net::Ipv4Addr::from(u32::from(v) & mask).to_string()
                                    }
                                    IpAddr::V6(v) => {
                                        let mask = if ones == 0 {
                                            0u128
                                        } else {
                                            u128::MAX << (128 - ones)
                                        };
                                        std::net::Ipv6Addr::from(u128::from(v) & mask).to_string()
                                    }
                                };
                                t = Target {
                                    kind: TARGET_CIDR.to_string(),
                                    host: format!("{masked}/{ones}"),
                                    port: 0,
                                };
                            }
                        }
                    }
                    let _ = suffix;
                }
                b':' => t.port = valid_port(&suffix),
                _ => {}
            }
        }
        list.add(t);
        blank(&mut b, start, token_end);
    }

    // name:port.
    for m in collect(&re_hostport, &b) {
        let (start, end) = m.whole;
        if start > 0 && (is_word(b[start - 1]) || matches!(b[start - 1], b'.' | b'-' | b'/')) {
            continue;
        }
        if !ends_token(&b, end) {
            continue;
        }
        let text = String::from_utf8_lossy(&b[start..end]).into_owned();
        let Some((host, port)) = text.split_once(':') else {
            continue;
        };
        let n = valid_port(port);
        let Some((_, host)) = normalize_host(host, false) else {
            continue;
        };
        if n == 0 {
            continue;
        }
        list.add(Target {
            kind: TARGET_DOMAIN.to_string(),
            host,
            port: n,
        });
        blank(&mut b, start, end);
    }
}

fn extract_declared(tool_name: &str, input: &[u8]) -> TargetList {
    let (mut tool, mut data) = (tool_name.to_string(), input.to_vec());
    if tool == EXECUTE_EXTRA_TOOL {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(input) {
            if let (Some(name), Some(params)) =
                (v.get("tool_name").and_then(|x| x.as_str()), v.get("params"))
            {
                if !name.is_empty() && name != EXECUTE_EXTRA_TOOL {
                    tool = name.to_string();
                    data = serde_json::to_vec(params).unwrap_or_default();
                }
            }
        }
    }
    if no_target_tools().contains(tool.as_str()) {
        return TargetList::default();
    }
    let mut out = TargetList::default();
    match tool.as_str() {
        "WebFetch" => {
            let (url, _, _) = declared_fields(&data);
            if let Some(t) = normalize_target(&url) {
                out.add(t);
                out.urls.push(url.trim().to_string());
            }
        }
        "Bash" | "shell_open" | "Monitor" => {
            let (_, cmd, _) = declared_fields(&data);
            scan(&mut out, &cmd);
        }
        "shell_send" => {
            let (_, _, text) = declared_fields(&data);
            scan(&mut out, &text);
        }
        _ => {
            let mut strings = Vec::new();
            scan_json_strings(&data, &mut strings);
            for s in strings {
                scan(&mut out, &s);
            }
        }
    }
    out
}

/// `ExtractTargets`: network targets a tool call declares, normalized, deduped,
/// in order of appearance.
pub fn extract_targets(tool_name: &str, input: &[u8]) -> Vec<Target> {
    extract_declared(tool_name, input).targets
}
