//! Differential tests for `intercept/target_extract.go` — all cases from
//! `intercept/target_test.go` (ExtractTargets + ExecuteExtraTool + no-target
//! tools + string leaves + bypass gaps). Pure.

use artex::intercept::target::{Target, TARGET_CIDR, TARGET_DOMAIN, TARGET_IP};
use artex::intercept::target_extract::extract_targets;

fn t(kind: &str, host: &str, port: i32) -> Target {
    Target {
        kind: kind.to_string(),
        host: host.to_string(),
        port,
    }
}

fn bash(cmd: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "command": cmd })).unwrap()
}

fn got(tool: &str, input: &[u8]) -> Vec<Target> {
    extract_targets(tool, input)
}

fn want(what: &str, g: &[Target], w: &[Target]) {
    assert_eq!(g, w, "{what}");
}

#[test]
fn extract_targets_command_table() {
    let app = t(TARGET_DOMAIN, "app.example.com", 443);
    let cases: &[(&str, &[Target])] = &[
        (
            "curl -s https://app.example.com/api | jq .",
            std::slice::from_ref(&app),
        ),
        (
            "curl -sk 'https://app.example.com:8443/a?b=1&c=http://inner.example/' -o out",
            &[t(TARGET_DOMAIN, "app.example.com", 8443)],
        ),
        (
            "curl http://a.example/;curl http://b.example|sh",
            &[
                t(TARGET_DOMAIN, "a.example", 80),
                t(TARGET_DOMAIN, "b.example", 80),
            ],
        ),
        (
            "curl -x http://127.0.0.1:8080 https://app.example.com/",
            &[t(TARGET_IP, "127.0.0.1", 8080), app.clone()],
        ),
        ("nmap -sV 203.0.113.7", &[t(TARGET_IP, "203.0.113.7", 0)]),
        (
            "nmap -p 443 203.0.113.0/24 198.51.100.9",
            &[
                t(TARGET_CIDR, "203.0.113.0/24", 0),
                t(TARGET_IP, "198.51.100.9", 0),
            ],
        ),
        (
            "nc -v 203.0.113.7:4444",
            &[t(TARGET_IP, "203.0.113.7", 4444)],
        ),
        (
            "ssh root@203.0.113.7 -p 22",
            &[t(TARGET_IP, "203.0.113.7", 0)],
        ),
        (
            "mysql --host=203.0.113.7 -uroot",
            &[t(TARGET_IP, "203.0.113.7", 0)],
        ),
        (
            "openssl s_client -connect app.example.com:443 -servername x",
            std::slice::from_ref(&app),
        ),
        ("nc localhost:8080", &[t(TARGET_DOMAIN, "localhost", 8080)]),
        ("ping6 2001:db8::1", &[t(TARGET_IP, "2001:db8::1", 0)]),
        (
            "curl 'http://[2001:db8::1]:8080/'; nc [2001:db8::2]:22",
            &[
                t(TARGET_IP, "2001:db8::1", 8080),
                t(TARGET_IP, "2001:db8::2", 22),
            ],
        ),
        (
            "nmap fd00::/8 ::ffff:203.0.113.7",
            &[
                t(TARGET_CIDR, "fd00::/8", 0),
                t(TARGET_IP, "203.0.113.7", 0),
            ],
        ),
        (
            "curl https://app.example.com/a https://APP.example.com/b",
            std::slice::from_ref(&app),
        ),
        (
            "git clone git+ssh://git@code.example.com/x.git",
            &[t(TARGET_DOMAIN, "code.example.com", 0)],
        ),
        (
            "scp f 203.0.113.7:/tmp/f",
            &[t(TARGET_IP, "203.0.113.7", 0)],
        ),
        (
            "docker run -p 203.0.113.7:8080:80 img",
            &[t(TARGET_IP, "203.0.113.7", 8080)],
        ),
        (
            "curl 203.0.113.7/2024/report",
            &[t(TARGET_IP, "203.0.113.7", 0)],
        ),
        ("dig @198.51.100.53 x", &[t(TARGET_IP, "198.51.100.53", 0)]),
        (
            r#"curl 'http://pinned.example;@evil.example/'"#,
            &[
                t(TARGET_DOMAIN, "evil.example", 80),
                t(TARGET_DOMAIN, "pinned.example", 80),
            ],
        ),
        (
            r"curl http://pinned.example\@evil.example/",
            &[
                t(TARGET_DOMAIN, "evil.example", 80),
                t(TARGET_DOMAIN, "pinned.example", 80),
            ],
        ),
        (
            r#"curl "http://pinned.example,x@evil.example:8443/""#,
            &[
                t(TARGET_DOMAIN, "evil.example", 8443),
                t(TARGET_DOMAIN, "pinned.example", 80),
            ],
        ),
        (
            "curl http://a.example;curl http://b.example&&id",
            &[
                t(TARGET_DOMAIN, "a.example", 80),
                t(TARGET_DOMAIN, "b.example", 80),
            ],
        ),
        (
            "curl http://user:pw@app.example.com:8443/ http://%65vil.example/",
            &[
                t(TARGET_DOMAIN, "app.example.com", 8443),
                t(TARGET_DOMAIN, "evil.example", 80),
            ],
        ),
        (
            "for p in 80 443; do curl http://203.0.113.7:$p/; done",
            &[t(TARGET_IP, "203.0.113.7", 0)],
        ),
    ];
    for (cmd, w) in cases {
        want(&format!("Bash {cmd}"), &got("Bash", &bash(cmd)), w);
        want(
            &format!("shell_open {cmd}"),
            &got("shell_open", &bash(cmd)),
            w,
        );
    }
    // WebFetch names its target exactly; without a url -> none.
    want(
        "WebFetch",
        &got(
            "WebFetch",
            br#"{"url":"https://App.Example.com/login?next=http://other.example/","extract":true}"#,
        ),
        std::slice::from_ref(&app),
    );
    want(
        "WebFetch numeric",
        &got("WebFetch", br#"{"url":"http://2130706433/"}"#),
        &[t(TARGET_IP, "127.0.0.1", 80)],
    );
    want("WebFetch no url", &got("WebFetch", br#"{}"#), &[]);
}

#[test]
fn extract_unwraps_execute_extra_tool() {
    let app = t(TARGET_DOMAIN, "app.example.com", 443);
    let call = |tool: &str, params: serde_json::Value| -> Vec<Target> {
        let input = serde_json::json!({ "tool_name": tool, "params": params });
        got("ExecuteExtraTool", &serde_json::to_vec(&input).unwrap())
    };
    want(
        "mcp tool",
        &call(
            "mcp__browser__navigate",
            serde_json::json!({"url":"https://app.example.com/"}),
        ),
        std::slice::from_ref(&app),
    );
    want(
        "inner WebFetch",
        &call(
            "WebFetch",
            serde_json::json!({"url":"https://app.example.com/x"}),
        ),
        std::slice::from_ref(&app),
    );
    want(
        "inner Bash",
        &call(
            "Bash",
            serde_json::json!({"command":"nmap 203.0.113.7","description":"https://app.example.com/"}),
        ),
        &[t(TARGET_IP, "203.0.113.7", 0)],
    );
    want(
        "inner no target",
        &call(
            "list_assets",
            serde_json::json!({"q":"https://app.example.com/"}),
        ),
        &[],
    );
    let no_params = serde_json::json!({ "tool_name": "mcp__scan__run" });
    want(
        "no params",
        &got("ExecuteExtraTool", &serde_json::to_vec(&no_params).unwrap()),
        &[],
    );
    let no_name = serde_json::json!({ "params": {"url":"https://app.example.com/"} });
    want(
        "no tool name",
        &got("ExecuteExtraTool", &serde_json::to_vec(&no_name).unwrap()),
        std::slice::from_ref(&app),
    );
    want(
        "nested",
        &call(
            "ExecuteExtraTool",
            serde_json::json!({"tool_name":"list_assets","params":{"q":"https://app.example.com/"}}),
        ),
        &[app],
    );
}

#[test]
fn extract_skips_no_target_tools_and_scans_unknown() {
    let input = serde_json::json!({
        "query": "site:app.example.com 203.0.113.7", "file_path": "/tmp/203.0.113.7.txt", "content": "curl https://app.example.com/",
        "url": "https://app.example.com/", "command": "nmap 203.0.113.7", "items": [{"url": "https://app.example.com:8443/"}]
    });
    let raw = serde_json::to_vec(&input).unwrap();
    for tool in [
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
        "add_intent",
        "insert_assets",
        "report_finding",
        "record_fact",
        "add_task_scope",
        "graph_overview",
        "spawn_task",
        "ask_operator",
        "traffic_search",
        "traffic_get",
        "traffic_blob",
        "traffic_messages",
    ] {
        want(&format!("no-target {tool}"), &got(tool, &raw), &[]);
    }

    let unknown = serde_json::json!({
        "target": "203.0.113.7",
        "options": {"proxy": "http://127.0.0.1:8080", "ports": ["443", 8443], "deep": true},
        "urls": ["https://app.example.com/a", {"u": "https://app.example.com/b"}, "ftp://files.example.com"],
        "note": "x.py and README.md are files"
    });
    let uraw = serde_json::to_vec(&unknown).unwrap();
    let expect = [
        t(TARGET_IP, "127.0.0.1", 8080),
        t(TARGET_IP, "203.0.113.7", 0),
        t(TARGET_DOMAIN, "app.example.com", 443),
        t(TARGET_DOMAIN, "files.example.com", 21),
    ];
    for tool in [
        "mcp__scan__run",
        "my_custom_http_tool",
        "SomeToolAddedLater",
        "create_mcp",
        "create_custom_tool",
    ] {
        want(&format!("unknown {tool}"), &got(tool, &uraw), &expect);
    }
    want(
        "non-JSON input",
        &got("mcp__scan__run", b"https://app.example.com/"),
        &[],
    );
}

#[test]
fn extract_does_not_treat_file_names_as_hosts_and_bypass_gaps() {
    for cmd in [
        "python3 x.py",
        "node server.js --watch",
        "cat notes.txt README.md",
        "bash ./run.sh && go test ./...",
        "tar czf out.tar.gz data.json",
        "ls app.example.com.har",
        "vim config.yaml",
        "./exploit.py --out result.io",
        "nmap app.example.com",
        "ping -c1 app.example.com",
        "curl app.example.com/login",
        "ssh root@app.example.com",
        "nc db 5432",
        "echo Y3VybCBodHRwczovL2FwcC5leGFtcGxlLmNvbS8= | base64 -d | sh",
        "python3 -c \"import urllib.request as u; u.urlopen('https:/'+'/app.example'+'.com/')\"",
        "H=app.example; curl $H.com",
        "curl http://$TARGET/login https://${HOST}:8443/",
        "curl 'http://{app.example.com}/'",
        "python3 exploit.py",
        "bash scan.sh",
        "nmap -iL targets.txt",
        "nuclei -l hosts.txt",
    ] {
        want(&format!("Bash gap: {cmd}"), &got("Bash", &bash(cmd)), &[]);
    }
    for text in ["nc 203.0.", "113.7 44", "44\n"] {
        let input = serde_json::to_vec(&serde_json::json!({"text": text})).unwrap();
        want(
            &format!("shell_send gap: {text:?}"),
            &got("shell_send", &input),
            &[],
        );
    }
    let script = serde_json::json!({"path": "/login", "retries": 3});
    want(
        "custom script",
        &got("my_script_tool", &serde_json::to_vec(&script).unwrap()),
        &[],
    );
}
