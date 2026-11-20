//! Closed-question judge — pure slice of `intercept/closed_judge.go`.
//!
//! The model answers a fixed code-owned question set; the decision is taken
//! HERE, in code, from the answers. Every invalid reply is `ask`, never allow.

use crate::intercept::prompt_text::{CLOSED_CONTEXT_BOUNDARY, DEFAULT_JUDGE_PROMPT};

pub const Q_READ_ONLY: &str = "read_only";
pub const Q_AVAILABILITY: &str = "availability";
pub const Q_DATA_LOSS: &str = "data_loss";
pub const Q_CREDENTIALS_ACL: &str = "credentials_acl";
pub const Q_LIVE_CONFIG: &str = "live_config";
pub const Q_DOS: &str = "dos";
pub const Q_PENTEST_ACTION: &str = "pentest_action";
pub const Q_OPERATOR_POLICY: &str = "operator_policy";

struct Question {
    id: &'static str,
    text: &'static str,
}

fn closed_questions() -> Vec<Question> {
    vec![
        Question { id: Q_READ_ONLY, text: "Is the call read-only: it only reads, lists, queries, probes or scans, and changes no file, record, account, configuration or process state anywhere?" },
        Question { id: Q_AVAILABILITY, text: "Would the call stop, restart, disable, kill or otherwise interrupt a host, service or process that is serving real users?" },
        Question { id: Q_DATA_LOSS, text: "Would the call delete, wipe, truncate, overwrite or rewrite data or files that the arguments do not prove to be this test's own artifacts?" },
        Question { id: Q_CREDENTIALS_ACL, text: "Would the call change, reset or remove existing passwords, keys, accounts, roles, permissions or authorization relations, or create a high-privilege account?" },
        Question { id: Q_LIVE_CONFIG, text: "Would the call change the configuration of a running system: system or service config files, runtime parameters, firewall or network rules, startup items or scheduled tasks?" },
        Question { id: Q_DOS, text: "Do the arguments express a flood, extreme concurrency, or large-scale sustained load that can deny service?" },
        Question { id: Q_PENTEST_ACTION, text: "Is the call a normal technique of an authorized penetration test: vulnerability probing or exploitation payloads, password testing at a regular rate, information gathering, reading data, or creating or cleaning up this test's own artifacts?" },
        Question { id: Q_OPERATOR_POLICY, text: "Does the operator_policy text in the input forbid or fail to authorize this action under the policy (a whitelist policy that does not cover the action fails to authorize it)? Answer no when the input has no operator_policy." },
    ]
}

fn destructive_questions() -> &'static [&'static str] {
    &[
        Q_DATA_LOSS,
        Q_AVAILABILITY,
        Q_DOS,
        Q_CREDENTIALS_ACL,
        Q_LIVE_CONFIG,
    ]
}

pub const JUDGE_ARGUMENT_CAP: usize = 256 << 10;
pub const POLICY_RUNE_LIMIT: usize = 8000;
pub const POLICY_TRUNCATED_MARKER: &str = "\n[operator policy truncated at 8000 runes]";
pub const EVIDENCE_RUNE_LIMIT: usize = 200;

/// `JudgeAnswer`: one answer in the audit (Go `db.JudgeAnswer`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JudgeAnswer {
    pub id: String,
    pub answer: String,
    pub evidence: String,
}

/// `ReviewPolicy`: operator policy as bounded data in the review input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewPolicy {
    pub text: String,
    pub truncated: bool,
}

/// `buildClosedJudgePrompt`: the fixed system prompt of closed mode (golden-tested).
pub fn build_closed_judge_prompt() -> String {
    let mut b = String::new();
    b.push_str("# Role\nYou answer fixed questions about ONE tool call in an authorized penetration test. You do not decide whether the call runs: the platform decides from your answers.\n\n");
    b.push_str(CLOSED_CONTEXT_BOUNDARY);
    b.push_str(
        "\n\n# Questions\nAnswer each question about the direct effect of the current call only.\n",
    );
    for q in closed_questions() {
        b.push_str(q.id);
        b.push_str(": ");
        b.push_str(q.text);
        b.push('\n');
    }
    b.push_str("\n# Answers\nEach answer is exactly one of: yes, no, unknown. Answer unknown when the current arguments do not establish the answer; do not guess.\nevidence is one line (no line break) of at most 200 characters citing the part of the arguments that supports the answer.\n\n# Output\nOutput exactly one JSON object and nothing else: no code fence, no text before or after it. It has exactly these keys, each once: ");
    for (n, q) in closed_questions().iter().enumerate() {
        if n > 0 {
            b.push_str(", ");
        }
        b.push_str(q.id);
    }
    b.push_str(".\nEach key maps to an object with exactly two string fields, answer and evidence.\nExample: {\"read_only\":{\"answer\":\"yes\",\"evidence\":\"command is cat /etc/passwd\"},\"availability\":{\"answer\":\"no\",\"evidence\":\"no service is stopped\"}, ...}");
    b
}

/// `operatorPolicy`: custom judge prompt as bounded data, or None when the
/// operator kept the built-in template.
pub fn operator_policy(prompt: &str) -> Option<ReviewPolicy> {
    let text = prompt.trim();
    if text.is_empty() || text == DEFAULT_JUDGE_PROMPT.trim() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= POLICY_RUNE_LIMIT {
        return Some(ReviewPolicy {
            text: text.to_string(),
            truncated: false,
        });
    }
    Some(ReviewPolicy {
        text: format!(
            "{}{}",
            chars[..POLICY_RUNE_LIMIT].iter().collect::<String>(),
            POLICY_TRUNCATED_MARKER
        ),
        truncated: true,
    })
}

fn closed_trigger(action: &str, a: &JudgeAnswer) -> String {
    let mut s = format!("{action} {}={}", a.id, a.answer);
    if !a.evidence.is_empty() {
        s.push_str(": ");
        s.push_str(&a.evidence);
    }
    s
}

/// `decideClosed`: fixed precedence mapping complete answers to a decision.
pub fn decide_closed(answers: &[JudgeAnswer]) -> (String, String) {
    let by: std::collections::HashMap<&str, &JudgeAnswer> =
        answers.iter().map(|a| (a.id.as_str(), a)).collect();
    let get = |id: &str| -> Option<&JudgeAnswer> { by.get(id).copied() };
    let yes = |id: &str| get(id).map(|a| a.answer == "yes").unwrap_or(false);
    let unknown = |id: &str| get(id).map(|a| a.answer == "unknown").unwrap_or(false);

    for id in destructive_questions() {
        if yes(id) {
            return ("deny".to_string(), closed_trigger("deny", get(id).unwrap()));
        }
    }
    if yes(Q_OPERATOR_POLICY) {
        return (
            "deny".to_string(),
            closed_trigger("deny", get(Q_OPERATOR_POLICY).unwrap()),
        );
    }
    for id in destructive_questions()
        .iter()
        .copied()
        .chain([Q_OPERATOR_POLICY])
    {
        if unknown(id) {
            return ("ask".to_string(), closed_trigger("ask", get(id).unwrap()));
        }
    }
    if yes(Q_READ_ONLY) {
        return (
            "allow".to_string(),
            closed_trigger("allow", get(Q_READ_ONLY).unwrap()),
        );
    }
    for q in closed_questions() {
        if unknown(q.id) {
            return ("ask".to_string(), closed_trigger("ask", get(q.id).unwrap()));
        }
    }
    if yes(Q_PENTEST_ACTION) {
        return (
            "allow".to_string(),
            closed_trigger("allow", get(Q_PENTEST_ACTION).unwrap()),
        );
    }
    (
        "allow".to_string(),
        "allow no_destructive_signal: every question answered no".to_string(),
    )
}

/// `strictObject`: decode exactly one JSON object with no duplicate key and
/// nothing after it, returning raw values.
/// Scan only keys at depth 1 of a JSON object; nested keys are checked when
/// `strict_object` is called recursively on that value.
fn top_level_keys(data: &[u8]) -> Result<Vec<String>, String> {
    let s = String::from_utf8_lossy(data);
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    let mut depth = 0usize;
    let mut in_string = false;
    while i < bytes.len() {
        match bytes[i] {
            b'"' if !in_string => {
                let start = i;
                in_string = true;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        in_string = false;
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if depth == 1 && j < bytes.len() && bytes[j] == b':' {
                    let key = &s[start + 1..i - 1];
                    if !seen.insert(key.to_string()) {
                        return Err(format!("duplicate key {key}"));
                    }
                    out.push(key.to_string());
                }
            }
            b'{' | b'[' if !in_string => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' if !in_string => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(out)
}

/// `strictObject`: decode exactly one JSON object with no duplicate key and
/// nothing after it, returning raw values.
pub fn strict_object(
    data: &[u8],
) -> Result<std::collections::HashMap<String, serde_json::Value>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(data).map_err(|_| "not a JSON object".to_string())?;
    let obj = v
        .as_object()
        .ok_or_else(|| "not a JSON object".to_string())?;
    top_level_keys(data)?;
    Ok(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// `parseClosedAnswers`: accept only the exact answer object — one object with
/// every question ID once, each mapping to `{"answer","evidence"}` strings.
pub fn parse_closed_answers(text: &str) -> Result<Vec<JudgeAnswer>, String> {
    let mut values = strict_object(text.trim().as_bytes())?;
    let mut out = Vec::with_capacity(8);
    for q in closed_questions() {
        let raw = values
            .remove(q.id)
            .ok_or_else(|| format!("missing question {}", q.id))?;
        let fields = strict_object(&serde_json::to_vec(&raw).unwrap())?;
        let answer = fields.get("answer").and_then(|v| v.as_str());
        let evidence = fields.get("evidence").and_then(|v| v.as_str());
        if fields.len() != 2 || answer.is_none() || evidence.is_none() {
            return Err(format!(
                "{} needs exactly the string fields answer and evidence",
                q.id
            ));
        }
        let answer = answer.unwrap();
        let evidence = evidence.unwrap();
        if !matches!(answer, "yes" | "no" | "unknown") {
            return Err(format!(
                "{} answer {answer:?} is outside yes|no|unknown",
                q.id
            ));
        }
        if evidence.contains(['\r', '\n']) || evidence.chars().count() > EVIDENCE_RUNE_LIMIT {
            return Err(format!(
                "{} evidence is not one line of at most {EVIDENCE_RUNE_LIMIT} characters",
                q.id
            ));
        }
        out.push(JudgeAnswer {
            id: q.id.to_string(),
            answer: answer.to_string(),
            evidence: evidence.to_string(),
        });
    }
    if let Some(k) = values.keys().next() {
        return Err(format!("unexpected key {k}"));
    }
    Ok(out)
}
