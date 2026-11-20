//! Deterministic tool-call risk scoring — port of `intercept/risk.go`.
//!
//! No model involved: computed when a pending row is read, never stored, so
//! changing the rules re-scores every open request.

use regex::Regex;

/// `RiskReport`: deterministic danger estimate of one pending tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskReport {
    /// 0..100.
    pub score: i64,
    /// `low` (<30) | `medium` (30..69) | `high` (>=70).
    pub level: String,
    /// One per matched signal, in table order.
    pub reasons: Vec<String>,
}

const RISK_MEDIUM_AT: i64 = 30;
const RISK_HIGH_AT: i64 = 70;
const RISK_MAX: i64 = 100;
const SHELL_BASE: i64 = 10;
const FILE_BASE: i64 = 30;
const FILE_SENSITIVE: i64 = 40;
const WEB_BASE: i64 = 10;
const WEB_MUTATING: i64 = 30;
const UNKNOWN_BASE: i64 = 30;

fn risk_level(score: i64) -> &'static str {
    if score >= RISK_HIGH_AT {
        "high"
    } else if score >= RISK_MEDIUM_AT {
        "medium"
    } else {
        "low"
    }
}

/// Public level accessor (Go test `riskLevel`).
pub fn risk_level_check(score: i64) -> &'static str {
    risk_level(score)
}

struct RiskSignal {
    reason: &'static str,
    points: i64,
    re: Regex,
}

fn re_signal(reason: &'static str, points: i64, pattern: &str) -> RiskSignal {
    RiskSignal {
        reason,
        points,
        re: Regex::new(pattern).expect("valid signal regex"),
    }
}

fn shell_signals() -> Vec<RiskSignal> {
    // Additive; each counts once, reasons follow this order.
    vec![
        re_signal(
            "destructive command (rm -r, dd, mkfs, shred, truncate, DROP TABLE, DELETE FROM, shutdown)",
            60,
            r"(?i)\brm\s+([^;&|]*\s)?(-[a-z]*r[a-z]*|--recursive)(\s|$)|\bdd\s+[^;&|]*\b(if|of)=|\bmkfs\b|\bshred\b|\btruncate\b|\bdrop\s+table\b|\bdelete\s+from\b|\bshutdown\b",
        ),
        re_signal(
            "privilege change (sudo, su, chmod 777, setcap)",
            40,
            r"(^|[\s;&|(])(sudo|su)(\s|$)|\bchmod\s+(-\w+\s+)*0?777\b|\bsetcap\b",
        ),
        re_signal(
            "reads secrets (/etc/shadow, .ssh, .aws/credentials, .git-credentials)",
            40,
            r"/etc/shadow|\.ssh/|\.aws/credentials|\.git-credentials",
        ),
        re_signal(
            "possible exfiltration (curl upload, scp, remote rsync, nc, socat)",
            40,
            r"\bcurl\b[^;&|]*\s(-d|--data[\w-]*|-F|--form|-T|--upload-file)(\s|=|$)|\bscp\b|\brsync\b[^;&|]*\s\S+:|\bnc\b|\bncat\b|\bsocat\b",
        ),
        re_signal(
            "reverse shell pattern (/dev/tcp, nc -e)",
            60,
            r"/dev/tcp/|\b(nc|ncat)\b[^;&|]*\s-e(\s|$)",
        ),
        re_signal(
            "network scanner (nmap, nuclei, sqlmap, ffuf, hydra, masscan, dalfox)",
            30,
            r"\b(nmap|nuclei|sqlmap|ffuf|hydra|masscan|dalfox)\b",
        ),
        RiskSignal {
            reason: "writes files (redirection or tee)",
            points: 15,
            re: Regex::new(r"a^").unwrap(), // handled specially via writes_file
        },
    ]
}

fn writes_file(cmd: &str) -> bool {
    let tee = Regex::new(r"(^|[|;&]\s*)tee\b").unwrap();
    if tee.is_match(cmd) {
        return true;
    }
    for (i, _) in cmd.char_indices() {
        // Go iterates bytes; '>' is ASCII so byte positions == char boundaries for it.
        if cmd.as_bytes().get(i) != Some(&b'>') {
            continue;
        }
        let mut rest = &cmd[i + 1..];
        if rest.starts_with('>') {
            rest = &rest[1..];
        }
        let rest = rest.trim_start_matches([' ', '\t']);
        if rest.is_empty() || rest.starts_with('&') || rest.starts_with("/dev/null") {
            continue;
        }
        return true;
    }
    false
}

fn risk_report(score: i64, reasons: Vec<String>) -> RiskReport {
    let score = score.min(RISK_MAX);
    RiskReport {
        score,
        level: risk_level(score).to_string(),
        reasons,
    }
}

/// `ScoreRisk`: score one tool call. Malformed input is medium, never a panic.
pub fn score_risk(tool: &str, input: &[u8]) -> RiskReport {
    let fields: serde_json::Value = match serde_json::from_slice::<serde_json::Value>(input) {
        Ok(v) if v.is_object() => v,
        _ => {
            return risk_report(
                UNKNOWN_BASE,
                vec!["tool input is not a valid JSON object".to_string()],
            )
        }
    };
    let str_field = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(s) = fields.get(*k).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
        String::new()
    };
    match tool {
        "Bash" | "shell_open" | "Monitor" | "shell_send" => {
            let cmd = str_field(&["command", "text"]);
            let mut score = SHELL_BASE;
            let mut reasons = Vec::new();
            for (idx, sig) in shell_signals().iter().enumerate() {
                let matched = if idx == 6 {
                    writes_file(&cmd)
                } else {
                    sig.re.is_match(&cmd)
                };
                if matched {
                    score += sig.points;
                    reasons.push(sig.reason.to_string());
                }
            }
            risk_report(score, reasons)
        }
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let mut score = FILE_BASE;
            let mut reasons = vec!["writes or edits a file".to_string()];
            let path = str_field(&["file_path", "path", "notebook_path"]);
            let sensitive = Regex::new(
                r"(^|/)(\.ssh|\.aws|\.gnupg)/|^/etc/|^/root/|^/boot/|/\.git-credentials$|/\.(bashrc|bash_profile|profile|zshrc)$|/authorized_keys$|(^|/)\.env$",
            )
            .unwrap();
            if sensitive.is_match(&path) {
                score += FILE_SENSITIVE;
                reasons
                    .push("sensitive path (system, credential or shell startup file)".to_string());
            }
            risk_report(score, reasons)
        }
        "WebFetch" | "web_search" => {
            let method = str_field(&["method"]).to_uppercase();
            if matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
                risk_report(
                    WEB_BASE + WEB_MUTATING,
                    vec![format!("mutating HTTP method {method}")],
                )
            } else {
                risk_report(WEB_BASE, Vec::new())
            }
        }
        _ if tool.starts_with("mcp__") => risk_report(
            UNKNOWN_BASE,
            vec!["MCP tool: no specific rules".to_string()],
        ),
        _ => risk_report(
            UNKNOWN_BASE,
            vec!["unrecognized tool: no specific rules".to_string()],
        ),
    }
}
