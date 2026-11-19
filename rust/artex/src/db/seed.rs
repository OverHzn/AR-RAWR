//! Built-in seed data + the idempotent `seedBuiltins` boot step — port of
//! `db/db.go` (`builtinAgents` + the seed functions) and the target-gate
//! exemption seed from `db/target_pins.go`.
//!
//! On every `Open`, after the schema and migrations are applied, `seed_builtins`
//! inserts the fixed built-in agents and their prompt-variable catalog, default-on
//! interactive-shell for the runtime agents, the built-in browser MCP (see
//! [`crate::db::browser_mcp`]), the built-in skill visibility bindings, the
//! built-in safety intercept rules (v1/v2/v3), the built-in asset blocklist, and
//! the initial target-gate exemption list. Each seed that ships default rows is
//! gated by a settings flag so a user's later disable/edit/delete is never
//! resurrected on restart.

use crate::db::browser_mcp::{BROWSER_MCP_COMMAND, BROWSER_MCP_NAME, BROWSER_MCP_SEED_ARGS};
use crate::db::error::{Error, Result};
use crate::db::DB;

/// `promptVar` — one row of an agent's prompt-variable catalog.
struct PromptVar {
    name: &'static str,
    desc: &'static str,
    example: &'static str,
    source: &'static str,
}

/// `builtinAgent` — one built-in agent row and its variable catalog.
struct BuiltinAgent {
    key: &'static str,
    name: &'static str,
    role: &'static str,
    desc: &'static str,
    vars: &'static [PromptVar],
    interactive_shell: bool,
    /// `runSeconds`: `None` = use the seed default (1200), `Some(0)` = unlimited.
    run_seconds: Option<i32>,
}

/// `builtinAgents` — the fixed built-in agents (mirrors docs §5(a)). Built-in
/// tools are not stored in the DB; only agents + the variable catalog are seeded.
const BUILTIN_AGENTS: &[BuiltinAgent] = &[
    BuiltinAgent {
        key: "goals",
        name: "Goal Decomposer",
        role: "goals",
        desc: "Breaks a penetration testing task's goal into several independent, verifiable sub-goals.",
        vars: &[PromptVar {
            name: "EngagementDescription",
            desc: "Task description (test target/background)",
            example: "Test the example.com site",
            source: "exploration",
        }],
        interactive_shell: false,
        run_seconds: None,
    },
    BuiltinAgent {
        key: "planner",
        name: "Planner",
        role: "planner",
        desc: "Reads the posture, judges goals, and only adds exploration intents when there is a genuinely uncovered new direction (one planning loop per task).",
        vars: &[
            PromptVar {
                name: "Goal",
                desc: "Overall task goal",
                example: "Gain admin access to example.com",
                source: "exploration",
            },
            PromptVar {
                name: "AssetSummary",
                desc: "Asset count/type distribution summary (optional)",
                example: "domain:3 ip:5 site:2",
                source: "distilled",
            },
        ],
        interactive_shell: false,
        run_seconds: None,
    },
    BuiltinAgent {
        key: "mainagent",
        name: "Main",
        role: "main",
        desc: "Human-machine interface: observes progress and turns human intent into hints or high-priority intents.",
        vars: &[
            PromptVar {
                name: "Goal",
                desc: "Current task goal",
                example: "Gain admin access to example.com",
                source: "exploration",
            },
            PromptVar {
                name: "AssetSummary",
                desc: "Initial posture summary (optional)",
                example: "domain:3 ip:5",
                source: "distilled",
            },
            PromptVar {
                name: "FindingsSummary",
                desc: "Confirmed vulnerability summary (optional)",
                example: "high:1 medium:2",
                source: "distilled",
            },
        ],
        interactive_shell: false,
        run_seconds: None,
    },
    BuiltinAgent {
        key: "worker",
        name: "Worker",
        role: "worker",
        desc: "Picks up one intent and executes it, writes discovered facts/vulnerabilities back to the knowledge graph, then stops.",
        vars: &[
            PromptVar {
                name: "ProxyAddr",
                desc: "Recorded proxy address (drives the if/else dual wording)",
                example: "127.0.0.1:8080",
                source: "runtime",
            },
            PromptVar {
                name: "WorkerName",
                desc: "Worker self-identifier (optional)",
                example: "worker-1",
                source: "runtime",
            },
        ],
        interactive_shell: false,
        run_seconds: None,
    },
    BuiltinAgent {
        key: "auto",
        name: "Auto",
        role: "assistant",
        desc: "Platform operations assistant: manages tasks (create/view/pause/hint) and assets with tools, and can create/modify skills, custom tools, and MCP.",
        vars: &[],
        interactive_shell: false,
        run_seconds: None,
    },
    BuiltinAgent {
        key: "pentest",
        name: "Penetration Testing",
        role: "assistant",
        desc: "Solo pentest agent: single-handedly walks the full chain from recon → attack surface → deep exploitation → verification → wrap-up, with its own planning, execution, and adversarial verification.",
        vars: &[],
        interactive_shell: true,
        run_seconds: Some(0),
    },
];

/// `builtinSkillVisibility` — shipped skill dir → the built-in agent keys that
/// should see it by default. Skills omitted here ship invisible by default.
const BUILTIN_SKILL_VISIBILITY: &[(&str, &[&str])] =
    &[("api-recon", &["auto", "pentest", "worker"])];

/// A built-in safety intercept rule (v1): `target` ∈ {tool_name, tool_input},
/// `typ` ∈ {string, regex}.
struct V1Rule {
    name: &'static str,
    target: &'static str,
    typ: &'static str,
    pattern: &'static str,
    action: &'static str,
    message: &'static str,
    priority: i32,
}

/// `seedDefaultInterceptRules` v1 data — system-destructive commands, DB-
/// destructive operations, and destructive HTTP requests.
const V1_RULES: &[V1Rule] = &[
    // ── System-destructive commands (priority 100) ───────────────────────────
    V1Rule {
        name: "[built-in] Recursive force delete rm -rf",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\brm\b.{0,80}(?:-[a-z]*r[a-z]*f[a-z]*|-[a-z]*f[a-z]*r[a-z]*|--recursive|--no-preserve-root)"#,
        action: "deny",
        message: "Recursive force delete is forbidden (rm -rf / rm --recursive); it can permanently damage the system or the target machine environment",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Delete critical system directories",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\brm\b[^"'\n]{0,60}["'\s](/|/etc|/bin|/usr|/boot|/var|/lib|/sys|/proc|/dev|/sbin|/root)"#,
        action: "deny",
        message: "Deleting critical system paths is forbidden",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Disk formatting mkfs",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\bmkfs\b"#,
        action: "deny",
        message: "Formatting disks is forbidden (mkfs)",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Overwrite disk device dd",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\bdd\b[^|\n]{0,100}\bof=\s*/dev/[a-zA-Z]"#,
        action: "deny",
        message: "Using dd to overwrite disk devices is forbidden",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Fork bomb",
        target: "tool_input",
        typ: "regex",
        pattern: r#":\(\)\s*\{[^}]*:\|:"#,
        action: "deny",
        message: "Executing a fork bomb is forbidden",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Shutdown / reboot",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\b(?:shutdown|reboot|halt|poweroff|init\s+[06])\b"#,
        action: "deny",
        message: "Executing shutdown or reboot commands is forbidden",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Kill all processes",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\bkill\s+-9\s+-1\b|\bkillall\s+-9\b"#,
        action: "deny",
        message: "kill -9 -1 or killall -9 is forbidden (kills all processes)",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Disk wipe shred / wipe",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\b(?:shred|wipe)\b[^|\n]{0,80}/dev/[a-zA-Z]"#,
        action: "deny",
        message: "Running shred/wipe erasure against disk devices is forbidden",
        priority: 100,
    },
    V1Rule {
        name: "[built-in] Flush firewall rules",
        target: "tool_input",
        typ: "regex",
        pattern: r#"\biptables\s+(?:-F|--flush)\b|\bnft\s+flush\s+ruleset\b"#,
        action: "deny",
        message: "Flushing firewall rules is forbidden (iptables -F / nft flush)",
        priority: 100,
    },
    // ── Database-destructive operations (priority 90) ────────────────────────
    V1Rule {
        name: "[built-in] SQL DROP DATABASE / TABLE / SCHEMA",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\bDROP\s+(?:DATABASE|TABLE|SCHEMA|INDEX|VIEW|TABLESPACE|USER|ROLE)\b"#,
        action: "deny",
        message: "DROP operations are forbidden; they can irreversibly destroy database objects",
        priority: 90,
    },
    V1Rule {
        name: "[built-in] SQL TRUNCATE",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\bTRUNCATE\s+(?:TABLE\s+)?\w"#,
        action: "deny",
        message: "TRUNCATE is forbidden; it can wipe all data in a table",
        priority: 90,
    },
    V1Rule {
        name: "[built-in] MongoDB drop / dropDatabase",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\.(?:dropDatabase|dropCollection|drop)\s*\("#, 
        action: "deny",
        message: "MongoDB drop operations are forbidden",
        priority: 90,
    },
    V1Rule {
        name: "[built-in] Redis FLUSHALL / FLUSHDB",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\b(?:FLUSHALL|FLUSHDB)\b"#,
        action: "deny",
        message: "Redis FLUSHALL / FLUSHDB is forbidden; it can wipe all cached data",
        priority: 90,
    },
    // ── Destructive HTTP requests (priority 80) ──────────────────────────────
    V1Rule {
        name: "[built-in] curl / wget sending DELETE requests",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\bcurl\b[^|\n&;"]{0,300}(?:-X\s*DELETE|--request\s+DELETE|-XDELETE)|\bwget\b[^|\n&;"]{0,300}--method[=\s]+DELETE"#,
        action: "deny",
        message: "Sending HTTP DELETE requests via curl/wget is forbidden; it may delete data on the target system",
        priority: 80,
    },
    V1Rule {
        name: "[built-in] Python HTTP client DELETE (requests/httpx/aiohttp)",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)\b(?:requests|httpx|aiohttp|urllib\.request)\.delete\s*\(|session\.delete\s*\(|client\.delete\s*\("#, 
        action: "deny",
        message: "Sending DELETE requests with Python HTTP clients is forbidden",
        priority: 80,
    },
    V1Rule {
        name: "[built-in] Declaring the HTTP DELETE method in scripts (JS/generic)",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)axios\.delete\s*\(|method\s*[:=]\s*['"]DELETE['"]"#,
        action: "deny",
        message: "Declaring and sending HTTP DELETE requests in scripts is forbidden",
        priority: 80,
    },
    V1Rule {
        name: "[built-in] Bulk clear / wipe endpoint paths",
        target: "tool_input",
        typ: "regex",
        pattern: r#"(?i)/(?:clear|wipe|flush|purge|truncate|drop|destroy|factory[-_]reset|reset[-_]all)(?:[/?#"'\s]|$)"#,
        action: "deny",
        message: "Calling bulk-clear or destroy-style endpoints is forbidden (/clear /wipe /flush /purge etc.)",
        priority: 80,
    },
];

/// A v2 rule: same columns but with an explicit `enabled` flag (the exfil rule
/// ships disabled).
struct V2Rule {
    name: &'static str,
    pattern: &'static str,
    action: &'static str,
    message: &'static str,
    enabled: bool,
    priority: i32,
}

/// `seedDefaultInterceptRulesV2` data — the two safety patterns migrated out of
/// the old guard.go floor into ordinary, user-editable rules.
const V2_RULES: &[V2Rule] = &[
    V2Rule {
        name: "[built-in] Destructive system commands",
        pattern: r#"(?i)\b(rm\s+-rf\s+/|mkfs|dd\s+if=|:\(\)\s*\{|shutdown|reboot|>\s*/dev/sd)"#,
        action: "deny",
        message: "Destructive command rejected (rm -rf / / mkfs / dd / fork bomb / shutdown-reboot / overwriting disk devices)",
        enabled: true,
        priority: 100,
    },
    V2Rule {
        name: "[built-in] Data exfiltration pipe",
        pattern: r#"(?i)(curl|wget|nc|ncat)\b[^|]*\b(\|\s*(curl|wget|nc))"#,
        action: "deny",
        message: "Suspected data exfiltration pipe rejected (command output piped out via curl/wget/nc)",
        enabled: false,
        priority: 80,
    },
];

/// `deleteEndpointPathPattern` — the v3 delete-endpoint path rule. Requires a
/// separator after the verb so /delivery, /details, /delta, /delegate do not
/// match, while /deleteAll, /delete_user, /delete-user do.
const DELETE_ENDPOINT_PATH_PATTERN: &str =
    r#"(?i)/(?:(?:delete|remove|unlink|erase|destroy)[-\w]*|del)(?:[/?#"'\s]|$)"#;

/// A target-gate exemption: an initial package-source host the runtime image
/// pulls from, so a package install does not ask for approval.
struct TargetExemption {
    host: &'static str,
    port: i64,
    note: &'static str,
}

/// The initial target-gate exemption list (the package sources of the runtime
/// image).
const TARGET_GATE_EXEMPTIONS: &[TargetExemption] = &[
    TargetExemption {
        host: "pypi.org",
        port: 443,
        note: "[built-in] Python package index",
    },
    TargetExemption {
        host: "files.pythonhosted.org",
        port: 443,
        note: "[built-in] Python package files",
    },
    TargetExemption {
        host: "registry.npmjs.org",
        port: 443,
        note: "[built-in] npm registry",
    },
    TargetExemption {
        host: "deb.debian.org",
        port: 80,
        note: "[built-in] Debian packages",
    },
    TargetExemption {
        host: "deb.debian.org",
        port: 443,
        note: "[built-in] Debian packages",
    },
];

/// `SettingTargetExemptHosts` — the settings key holding the exemption JSON list.
pub(crate) const SETTING_TARGET_EXEMPT_HOSTS: &str = "target_gate_exempt_hosts";

/// `seedBuiltins`: insert the fixed built-in agents, their variable catalog, the
/// runtime defaults, the browser MCP, skill visibility, intercept rules, asset
/// blocklist, and target-gate exemptions. Idempotent; default-row seeds are gated
/// by settings flags so a user's later edits are never overwritten.
pub(crate) async fn seed_builtins(db: &DB) -> Result<()> {
    for a in BUILTIN_AGENTS {
        let run_seconds = a.run_seconds;
        let agent_id: i64 = db
            .query_one(
                "INSERT INTO agents(key, name, description, role, builtin, enabled, interactive_shell, run_seconds)
                 VALUES ($1, $2, NULLIF($3,''), $4, true, true, $5, COALESCE($6, 1200))
                 ON CONFLICT (key) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description
                 RETURNING id",
                &[
                    &a.key,
                    &a.name,
                    &a.desc,
                    &a.role,
                    &a.interactive_shell,
                    &run_seconds,
                ],
            )
            .await
            .map_err(|e| Error::msg(format!("agent {}: {e}", a.key)))?
            .get(0);
        for v in a.vars {
            db.execute(
                "INSERT INTO agent_prompt_vars(agent_id, var_name, description, example, source)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (agent_id, var_name) DO UPDATE
                 SET description = EXCLUDED.description, example = EXCLUDED.example, source = EXCLUDED.source",
                &[&agent_id, &v.name, &v.desc, &v.example, &v.source],
            )
            .await
            .map_err(|e| Error::msg(format!("agent {} var {}: {e}", a.key, v.name)))?;
        }
    }
    // Drop catalog entries for renamed variables so the white-list no longer
    // advertises a name templates can't resolve (EngagementTitle→Description,
    // and the leftover per-agent 'Now' after it became a global runtime var).
    db.execute(
        "DELETE FROM agent_prompt_vars WHERE var_name IN ('EngagementTitle', 'CoverageGaps', 'Now')",
        &[],
    )
    .await
    .map_err(|e| Error::msg(format!("cleanup renamed vars: {e}")))?;

    // Default-on interactive_shell for the runtime agents, ONCE (respects a later
    // user toggle-off). goals (one-shot decomposer) stays off.
    if db.get_setting("interactive_shell_default_v1").await?.0 != "true" {
        db.execute(
            "UPDATE agents SET interactive_shell=true WHERE key IN ('planner','worker','mainagent','auto')",
            &[],
        )
        .await
        .map_err(|e| Error::msg(format!("seed interactive_shell defaults: {e}")))?;
        let _ = db.set_setting("interactive_shell_default_v1", "true").await;
    }

    // Built-in browser (Playwright) MCP, disabled by default, inserted only if
    // absent so user edits are never clobbered.
    let seed_args: serde_json::Value = serde_json::from_str(BROWSER_MCP_SEED_ARGS)
        .map_err(|e| Error::msg(format!("encode browser mcp seed args: {e}")))?;
    let empty_env = serde_json::json!({});
    db.execute(
        "INSERT INTO mcp_servers(name, transport, command, args, env, enabled)
         VALUES ($1, 'stdio', $2, $3, $4, false)
         ON CONFLICT (name) DO NOTHING",
        &[
            &BROWSER_MCP_NAME,
            &BROWSER_MCP_COMMAND,
            &seed_args,
            &empty_env,
        ],
    )
    .await
    .map_err(|e| Error::msg(format!("seed browser mcp: {e}")))?;
    db.migrate_stock_browser_mcp()
        .await
        .map_err(|e| Error::msg(format!("migrate browser mcp: {e}")))?;

    seed_builtin_skill_visibility(db).await?;
    seed_default_intercept_rules(db).await?;
    seed_default_intercept_rules_v2(db).await?;
    seed_default_intercept_rules_v3(db).await?;
    seed_default_asset_intercept_rules(db).await?;
    seed_target_gate_exemptions(db).await?;
    Ok(())
}

/// `seedBuiltinSkillVisibility`: bind the shipped built-in skills to their default
/// agents. Insert-if-absent so a user's later toggle-off (enabled=false row) is
/// never resurrected.
async fn seed_builtin_skill_visibility(db: &DB) -> Result<()> {
    for (skill, agent_keys) in BUILTIN_SKILL_VISIBILITY {
        for key in *agent_keys {
            db.execute(
                "INSERT INTO agent_skill_visibility(agent_id, skill_name, enabled)
                 SELECT id, $2, true FROM agents WHERE key=$1
                 ON CONFLICT (agent_id, skill_name) DO NOTHING",
                &[key, skill],
            )
            .await
            .map_err(|e| Error::msg(format!("skill {skill} → agent {key}: {e}")))?;
        }
    }
    Ok(())
}

/// `seedDefaultInterceptRules` (v1): built-in safety intercept rules, once.
async fn seed_default_intercept_rules(db: &DB) -> Result<()> {
    if db.get_setting("intercept_default_rules_v1").await?.0 == "done" {
        return Ok(());
    }
    for r in V1_RULES {
        db.execute(
            "INSERT INTO intercept_rules(name, enabled, priority, match_target, match_type, pattern, action, message, timeout_enabled, timeout_seconds, timeout_action)
             VALUES ($1, true, $2, $3, $4, $5, $6, $7, false, 60, 'deny')
             ON CONFLICT DO NOTHING",
            &[&r.name, &r.priority, &r.target, &r.typ, &r.pattern, &r.action, &r.message],
        )
        .await
        .map_err(|e| Error::msg(format!("rule {:?}: {e}", r.name)))?;
    }
    db.set_setting("intercept_default_rules_v1", "done").await
}

/// `seedDefaultInterceptRulesV2`: the two safety patterns migrated out of the
/// guard.go floor, once. The exfil rule ships disabled.
async fn seed_default_intercept_rules_v2(db: &DB) -> Result<()> {
    if db.get_setting("intercept_default_rules_v2").await?.0 == "done" {
        return Ok(());
    }
    for r in V2_RULES {
        db.execute(
            "INSERT INTO intercept_rules(name, enabled, priority, match_target, match_type, pattern, action, message, timeout_enabled, timeout_seconds, timeout_action)
             VALUES ($1, $2, $3, 'tool_input', 'regex', $4, $5, $6, false, 60, 'deny')
             ON CONFLICT DO NOTHING",
            &[&r.name, &r.enabled, &r.priority, &r.pattern, &r.action, &r.message],
        )
        .await
        .map_err(|e| Error::msg(format!("rule {:?}: {e}", r.name)))?;
    }
    db.set_setting("intercept_default_rules_v2", "done").await
}

/// `seedDefaultInterceptRulesV3`: the delete-endpoint path rule (catches a delete
/// endpoint reached with GET/POST, which v1's method-only rules miss), once.
async fn seed_default_intercept_rules_v3(db: &DB) -> Result<()> {
    if db.get_setting("intercept_default_rules_v3").await?.0 == "done" {
        return Ok(());
    }
    let name = "[built-in] Delete-style endpoint paths";
    let message = "Calling delete-style endpoints is forbidden (/delete /remove /unlink /erase etc.), regardless of the HTTP method — most apps' delete endpoints can be triggered with GET/POST and will still really delete target data";
    db.execute(
        "INSERT INTO intercept_rules(name, enabled, priority, match_target, match_type, pattern, action, message, timeout_enabled, timeout_seconds, timeout_action)
         SELECT $1, true, 80, 'tool_input', 'regex', $2, 'deny', $3, false, 60, 'deny'
         WHERE NOT EXISTS (SELECT 1 FROM intercept_rules WHERE name = $1)",
        &[&name, &DELETE_ENDPOINT_PATH_PATTERN, &message],
    )
    .await
    .map_err(|e| Error::msg(format!("rule {name:?}: {e}")))?;
    db.set_setting("intercept_default_rules_v3", "done").await
}

/// `seedDefaultAssetInterceptRules`: the built-in asset blocklist (fuzzy domain
/// matches for government / education sites), once.
async fn seed_default_asset_intercept_rules(db: &DB) -> Result<()> {
    if db.get_setting("asset_intercept_default_rules_v1").await?.0 == "done" {
        return Ok(());
    }
    let rules: &[(&str, &str, &str)] = &[
        (
            "fuzzy_domain",
            ".gov",
            "[built-in] Government websites (.gov)",
        ),
        (
            "fuzzy_domain",
            ".gov.cn",
            "[built-in] Government websites (.gov.cn)",
        ),
        (
            "fuzzy_domain",
            ".edu",
            "[built-in] Education websites (.edu)",
        ),
        (
            "fuzzy_domain",
            ".edu.cn",
            "[built-in] Education websites (.edu.cn)",
        ),
    ];
    for (kind, pattern, note) in rules {
        db.execute(
            "INSERT INTO asset_intercept_rules(enabled, kind, pattern, note, builtin)
             VALUES (true, $1, $2, $3, true)
             ON CONFLICT DO NOTHING",
            &[kind, pattern, note],
        )
        .await
        .map_err(|e| Error::msg(format!("asset rule {pattern:?}: {e}")))?;
    }
    db.set_setting("asset_intercept_default_rules_v1", "done")
        .await
}

/// `seedTargetGateExemptions`: store the initial exemption list (the package
/// sources of the runtime image) once, insert-if-absent so the list is owned by
/// the operator afterwards.
async fn seed_target_gate_exemptions(db: &DB) -> Result<()> {
    if db.get_setting("target_gate_exempt_seed_v1").await?.0 == "done" {
        return Ok(());
    }
    // Marshal `[{host,port,note}]` (note omitted when empty) exactly as Go's
    // encoding/json with `json:"host"`, `json:"port"`, `json:"note,omitempty"`.
    let items: Vec<serde_json::Value> = TARGET_GATE_EXEMPTIONS
        .iter()
        .map(|e| {
            let mut m = serde_json::Map::new();
            m.insert(
                "host".to_string(),
                serde_json::Value::String(e.host.to_string()),
            );
            m.insert("port".to_string(), serde_json::Value::from(e.port));
            if !e.note.is_empty() {
                m.insert(
                    "note".to_string(),
                    serde_json::Value::String(e.note.to_string()),
                );
            }
            serde_json::Value::Object(m)
        })
        .collect();
    let raw = serde_json::Value::Array(items).to_string();
    db.insert_setting_if_absent(SETTING_TARGET_EXEMPT_HOSTS, &raw)
        .await?;
    db.set_setting("target_gate_exempt_seed_v1", "done").await
}
