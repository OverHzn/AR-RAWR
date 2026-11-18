//! The Rust port of `intercept/`.
//!
//! Only the pure, value-typed functions are bridged here; everything that needs
//! a Bash parser (`mvdan.cc/sh`), IDNA punycode (`golang.org/x/net/idna`),
//! `net/netip`, `context.Context`, pointers or logging stays in Go (see
//! `rust-anything.json`'s `js_only`). Shared Go-stdlib behavior lives in the
//! `gocompat` crate.

#![forbid(unsafe_code)]

use gocompat::{filepath, gojson, strings};
use serde_json::{Value, json};

/// `db.ModelReasonPrefix`.
const MODEL_REASON_PREFIX: &str = "[model]";

/// `intercept.JudgeContextBoundary` (copied byte-for-byte from `intercept/prompt.go`).
const JUDGE_CONTEXT_BOUNDARY: &str = include_str!("judge_context_boundary.txt");

/// `intercept.JudgeOutputContract` (copied byte-for-byte from `intercept/prompt.go`).
const JUDGE_OUTPUT_CONTRACT: &str = include_str!("judge_output_contract.txt");

/// `argvMaxTokens`.
const ARGV_MAX_TOKENS: usize = 8;

/// The programs an argv allow rule may never name (`alwaysAllowDenylist`), as a
/// flat set of the already-normalised names (lowercase, no version suffix).
const ALWAYS_ALLOW_DENYLIST: &[&str] = &[
    // interpreters and shells
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "csh",
    "tcsh",
    "ash",
    "mksh",
    "rbash",
    "pwsh",
    "powershell",
    "python",
    "pypy",
    "ipython",
    "perl",
    "ruby",
    "irb",
    "node",
    "deno",
    "bun",
    "php",
    "lua",
    "luajit",
    "rscript",
    "tclsh",
    "wish",
    "expect",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "sed",
    "jshell",
    "osascript",
    // launchers: run their arguments (or a flag's value) as a command
    "env",
    "command",
    "builtin",
    "exec",
    "eval",
    "source",
    ".",
    "nohup",
    "timeout",
    "nice",
    "ionice",
    "stdbuf",
    "setsid",
    "chroot",
    "unshare",
    "nsenter",
    "systemd-run",
    "su",
    "sudo",
    "doas",
    "pkexec",
    "runuser",
    "setpriv",
    "sg",
    "watch",
    "busybox",
    "toybox",
    "xargs",
    "parallel",
    "make",
    "gmake",
    "mapfile",
    "trap",
    "script",
    "find",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "socat",
    "ncat",
    "nc",
    "netcat",
    "flock",
    "strace",
    "ltrace",
    "gdb",
    "taskset",
    "chrt",
    "unbuffer",
    "rlwrap",
    "firejail",
    "bwrap",
    "proot",
    "proxychains",
    "torsocks",
    "tar",
    "zip",
    "vim",
    "vi",
    "nvim",
    "emacs",
    "less",
    "more",
    "man",
    // package managers and build tools: run package scripts and hooks
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bunx",
    "corepack",
    "pip",
    "pipx",
    "uv",
    "uvx",
    "poetry",
    "composer",
    "gem",
    "bundle",
    "bundler",
    "rake",
    "cargo",
    "go",
    "mvn",
    "gradle",
    "dotnet",
    "apt",
    "apt-get",
    "dpkg",
    "dnf",
    "yum",
    "rpm",
    "pacman",
    "apk",
    "brew",
    "snap",
    "flatpak",
    // orchestration: run images, playbooks and plans
    "docker",
    "docker-compose",
    "podman",
    "nerdctl",
    "kubectl",
    "helm",
    "ansible",
    "ansible-playbook",
    "terraform",
    "pulumi",
    "vagrant",
    // destructive
    "rm",
    "dd",
    "mkfs",
    "mkswap",
    "wipefs",
    "fdisk",
    "sfdisk",
    "parted",
    "shred",
    "truncate",
    "chmod",
    "chown",
    "kill",
    "pkill",
    "killall",
    "systemctl",
    "service",
    "iptables",
    "ip6tables",
    "nft",
    "ufw",
    "mount",
    "umount",
    "swapoff",
    "shutdown",
    "reboot",
    "poweroff",
    "halt",
    "init",
    "telinit",
    "crontab",
    "insmod",
    "rmmod",
    "modprobe",
];

/// Port of `argvProgramName` (`intercept/alwaysallow_denylist.go:47`): basename,
/// lower case, `mkfs.*` collapsed to `mkfs`, trailing version (digits/dots) removed.
fn argv_program_name(tok: &str) -> String {
    let name = strings::go_to_lower(&filepath::path_base(tok));
    if name.starts_with("mkfs.") {
        return "mkfs".to_string();
    }
    let bytes = name.as_bytes();
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b'.' || bytes[end - 1].is_ascii_digit()) {
        end -= 1;
    }
    if end > 0 && end < bytes.len() {
        return name[..end].to_string();
    }
    name
}

/// Port of `ArgvAllowDenied` (`intercept/alwaysallow_denylist.go:66`).
pub fn argv_allow_denied(program: String) -> Result<bool, String> {
    let name = argv_program_name(&program);
    Ok(ALWAYS_ALLOW_DENYLIST.contains(&name.as_str()))
}

/// Port of `CheckArgvAllow` (`intercept/alwaysallow_denylist.go:72`).
pub fn check_argv_allow(tokens: Vec<String>) -> Result<(), String> {
    if !tokens.is_empty() && argv_allow_denied(tokens[0].clone())? {
        return Err(format!(
            "program {} cannot be always allowed: it can run arbitrary code or cause unbounded damage whatever its arguments",
            gojson::go_quote_string(&tokens[0])
        ));
    }
    Ok(())
}

/// Port of `argvArity` lookup (`intercept/argv.go:17`).
fn argv_arity(key: &str) -> Option<usize> {
    Some(match key {
        "cat" | "cd" | "ls" | "rm" | "grep" | "head" | "tail" | "echo" | "pwd" | "find"
        | "curl" | "wget" | "nmap" => 1,
        "git" | "npm" | "docker" | "kubectl" | "terraform" => 2,
        "npm run" | "docker compose" | "kubectl rollout" | "git config" | "gcloud" | "gh"
        | "aws" => 3,
        _ => return None,
    })
}

/// Port of `argvPrefixLen` (`intercept/argv.go:26`).
fn argv_prefix_len(tokens: &[String]) -> usize {
    let mut n = tokens.len();
    while n > 0 {
        if let Some(a) = argv_arity(&tokens[..n].join(" ")) {
            return a.min(tokens.len());
        }
        n -= 1;
    }
    1.min(tokens.len())
}

/// Port of `ArgvPrefixOf` (`intercept/argv.go:38`).
pub fn argv_prefix_of(tokens: Vec<String>) -> Result<Vec<String>, String> {
    Ok(tokens[..argv_prefix_len(&tokens)].to_vec())
}

/// Port of `checkArgvToken` (`intercept/argv.go:183`).
fn check_argv_token(tok: &str) -> Result<(), String> {
    if tok.is_empty() {
        return Err("a token is empty".to_string());
    }
    if tok.starts_with('-') {
        return Err(format!(
            "token {} is a flag; a prefix holds the program and its subcommands only",
            gojson::go_quote_string(tok)
        ));
    }
    if tok.chars().any(|c| ";&|><`$()".contains(c)) || tok.chars().any(strings::go_is_space) {
        return Err(format!(
            "token {} contains a shell metacharacter or a space",
            gojson::go_quote_string(tok)
        ));
    }
    Ok(())
}

/// Port of `ParseArgvPattern` (`intercept/argv.go:206`).
pub fn parse_argv_pattern(pattern: String) -> Result<Vec<String>, String> {
    let tokens: Vec<String> = match serde_json::from_str(&pattern) {
        Ok(t) => t,
        Err(_) => {
            // Reproduce Go's `encoding/json.Unmarshal` error text: a full syntax
            // scan first (which may fail inside a `{...}`), then a type error for
            // syntactically valid non-`[]string` input.
            let reason = match gojson::go_json_check_valid(&pattern) {
                Some(syntax) => syntax,
                None => "json: cannot unmarshal value into Go value of type []string".to_string(),
            };
            return Err(format!(
                "an argv pattern is a JSON array of strings, for example [\"git\",\"push\"]: {}",
                reason
            ));
        }
    };
    if tokens.is_empty() || tokens.len() > ARGV_MAX_TOKENS {
        return Err(format!(
            "an argv pattern has 1 to {} tokens, got {}",
            ARGV_MAX_TOKENS,
            tokens.len()
        ));
    }
    for tok in &tokens {
        check_argv_token(tok)?;
    }
    Ok(tokens)
}

/// Port of `judgeActionLabel` (`intercept/intercept.go:644`).
fn judge_action_label(action: &str) -> &str {
    match action {
        "allow" => "allow",
        "deny" => "block",
        "ask" => "escalated to manual approval",
        other => other,
    }
}

/// Port of `JudgeReason` (`intercept/intercept.go:633`).
pub fn judge_reason(action: String, message: String) -> Result<String, String> {
    if message.is_empty() {
        return Ok(format!(
            "{} {}",
            MODEL_REASON_PREFIX,
            judge_action_label(&action)
        ));
    }
    if !message.starts_with(MODEL_REASON_PREFIX) {
        return Ok(format!("{} {}", MODEL_REASON_PREFIX, message));
    }
    Ok(message)
}

/// Port of `EffectiveJudgePrompt` (`intercept/prompt.go:20`).
pub fn effective_judge_prompt(prompt: String) -> Result<String, String> {
    let mut prompt = prompt;
    if !prompt.contains(JUDGE_CONTEXT_BOUNDARY) {
        prompt.push_str("\n\n");
        prompt.push_str(JUDGE_CONTEXT_BOUNDARY);
    }
    if !prompt.contains(JUDGE_OUTPUT_CONTRACT) {
        prompt.push_str("\n\n");
        prompt.push_str(JUDGE_OUTPUT_CONTRACT);
    }
    Ok(prompt)
}

/// Port of `stripCodeFence` (`intercept/prompt.go:142`).
fn strip_code_fence(text: &str) -> String {
    let t = strings::go_trim_space(text);
    let tb = t.as_bytes();
    if tb.len() <= 6 || !t.starts_with("```") || !t.ends_with("```") {
        return t.to_string();
    }
    let mut t = strings::go_trim_space(&t[3..t.len() - 3]);
    if !t.starts_with('{') {
        // Drop the opening fence's language tag line (```json).
        if let Some(idx) = t.find('\n') {
            t = strings::go_trim_space(&t[idx + 1..]);
        }
    }
    t.to_string()
}

/// A successfully parsed judge reply, or `None` for Go's zero `Verdict{}`.
struct Verdict {
    action: String,
    reason: String,
}

/// Port of `ParseVerdict` (`intercept/prompt.go:162`): strict two-string-object
/// parse with the application's comment contract. Returns `None` (empty verdict)
/// for anything unparseable, mirroring Go's `Verdict{}`.
fn parse_verdict_impl(text: &str) -> Option<Verdict> {
    let stripped = strip_code_fence(text);

    // Parse the reply as a single JSON object with the same strictness Go's
    // `json.Decoder` token walk enforces: exactly the keys `decision` and
    // `comment`, each a JSON string, no duplicates, nothing trailing.
    let value: Value = serde_json::from_str(&stripped).ok()?;

    let obj = value.as_object()?;
    // Duplicate keys collapse in serde_json; re-scan the raw text to reject them.
    if has_duplicate_object_keys(&stripped) || obj.len() != 2 {
        return None;
    }
    let decision = obj.get("decision")?.as_str()?;
    let comment = obj.get("comment")?.as_str()?;
    if !obj.contains_key("decision") || !obj.contains_key("comment") {
        return None;
    }

    let reason = strings::go_trim_space(comment);
    if decision != "allow" && decision != "ask" && decision != "deny" {
        return None;
    }
    if reason.len() > 2400 {
        return None;
    }
    let body = reason.strip_prefix("Actual action: ")?;
    let (operation, rest) = body.split_once("; Consequence if successful: ")?;
    if strings::go_trim_space(operation).is_empty() {
        return None;
    }
    let (consequence, rule) = rest.split_once("; Matched rule: ")?;
    if strings::go_trim_space(consequence).is_empty() || strings::go_trim_space(rule).is_empty() {
        return None;
    }

    Some(Verdict {
        action: decision.to_string(),
        reason: reason.to_string(),
    })
}

/// Whether a JSON object's text declares any key more than once. serde_json keeps
/// only the last duplicate, so Go's "reject duplicates" rule needs a raw scan.
fn has_duplicate_object_keys(text: &str) -> bool {
    let mut keys = std::collections::HashSet::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut expect_key = false;
    // Walk only the top-level object's keys: track string tokens at depth 1.
    let mut current = String::new();
    for c in text.chars() {
        if in_string {
            if escape {
                current.push(c);
                escape = false;
                continue;
            }
            match c {
                '\\' => {
                    escape = true;
                    current.push(c);
                }
                '"' => {
                    in_string = false;
                    if depth == 1 && expect_key {
                        if !keys.insert(std::mem::take(&mut current)) {
                            return true;
                        }
                        expect_key = false;
                    } else {
                        current.clear();
                    }
                }
                _ => current.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                current.clear();
            }
            '{' | '[' => {
                depth += 1;
                if c == '{' && depth == 1 {
                    expect_key = true;
                }
            }
            '}' | ']' => depth -= 1,
            ',' if depth == 1 => expect_key = true,
            _ => {}
        }
    }
    false
}

/// Port of `ParseVerdict` (`intercept/prompt.go:162`).
pub fn parse_verdict(text: String) -> Result<Value, String> {
    Ok(match parse_verdict_impl(&text) {
        Some(v) => json!({ "Action": v.action, "Reason": v.reason }),
        None => json!({ "Action": "", "Reason": "" }),
    })
}
