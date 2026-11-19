//! Port of `notify/email_test.go`, the email dial-guard cases of
//! `notify/ssrf_test.go`, and the email cases of `notify/channels_test.go`.
//!
//! SMTP behaviour is driven by a minimal hand-rolled server rather than mocking
//! net/smtp: almost all of the email channel's risk lives in "talking to a real
//! SMTP server", and mocking that away is equivalent to not testing it. The fake
//! server speaks plaintext (it never advertises STARTTLS), returns the reply code
//! each case asks for at a chosen stage, and records the commands it saw.

use std::sync::{Arc, Mutex};

mod common;

use artex::notify::channel;
use artex::notify::http::ALLOW_LOCAL_TARGETS_ENV;
use artex::notify::{Context, Error, Item, Message};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use common::lock_env;

// ===== Message constructor (mirror Go's singleMsg()) =====

fn single_msg() -> Message {
    Message {
        items: vec![Item {
            finding_id: 42,
            name: "login page \"SQL injection\" risk".to_string(),
            vulnclass: "SQL injection".to_string(),
            severity: "high".to_string(),
            summary: "param id\nunfiltered leads to injection".to_string(),
            assets: vec!["a.example.com".to_string(), "b.example.com".to_string()],
            detail_url: "https://artex.local/function/findings/detail?id=42".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

// ===== Fake SMTP server =====

#[derive(Default)]
struct FakeState {
    commands: Vec<String>,
    body: String,
}

struct FakeSmtp {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<FakeState>>,
}

impl FakeSmtp {
    /// Start a fake server on an ephemeral loopback port.
    async fn start(mail_reply: &str, rcpt_reply: &str, advertise_auth: bool) -> Self {
        let ln = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake smtp");
        let addr = ln.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(FakeState::default()));
        let st = state.clone();
        let mail_reply = mail_reply.to_string();
        let rcpt_reply = rcpt_reply.to_string();
        tokio::spawn(async move {
            serve(ln, st, mail_reply, rcpt_reply, advertise_auth).await;
        });
        FakeSmtp { addr, state }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Base channel config pointing at this server.
    fn cfg(&self) -> serde_json::Value {
        json!({
            "host": "127.0.0.1",
            "port": self.port(),
            "from": "artex@example.com",
            "to": ["a@example.com", "b@example.com"],
        })
    }

    fn saw_command(&self, prefix: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|c| c.starts_with(prefix))
    }

    fn body(&self) -> String {
        self.state.lock().unwrap().body.clone()
    }
}

/// Write one CRLF-terminated line.
async fn send(w: &mut tokio::net::tcp::OwnedWriteHalf, s: &str) {
    let _ = w.write_all(s.as_bytes()).await;
    let _ = w.write_all(b"\r\n").await;
    let _ = w.flush().await;
}

async fn serve(
    ln: TcpListener,
    state: Arc<Mutex<FakeState>>,
    mail_reply: String,
    rcpt_reply: String,
    advertise_auth: bool,
) {
    let (sock, _) = match ln.accept().await {
        Ok(v) => v,
        Err(_) => return,
    };
    let (rd, mut wr) = sock.into_split();
    let mut reader = BufReader::new(rd);

    send(&mut wr, "220 fake.local ESMTP ready").await;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        state.lock().unwrap().commands.push(line.clone());

        if line.starts_with("EHLO") || line.starts_with("HELO") {
            // Never advertise STARTTLS: force the client down the plaintext branch.
            send(&mut wr, "250-fake.local").await;
            if advertise_auth {
                send(&mut wr, "250-AUTH PLAIN").await;
            }
            send(&mut wr, "250 8BITMIME").await;
        } else if line.starts_with("AUTH") {
            send(&mut wr, "235 2.7.0 Authentication successful").await;
        } else if line.starts_with("MAIL FROM") {
            let r = mail_reply.clone();
            send(&mut wr, &r).await;
        } else if line.starts_with("RCPT TO") {
            let r = rcpt_reply.clone();
            send(&mut wr, &r).await;
        } else if line.starts_with("DATA") {
            send(&mut wr, "354 End data with <CR><LF>.<CR><LF>").await;
            let mut body = String::new();
            loop {
                let mut dl = String::new();
                match reader.read_line(&mut dl).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let mut t = dl.clone();
                while t.ends_with('\n') || t.ends_with('\r') {
                    t.pop();
                }
                if t == "." {
                    break;
                }
                body.push_str(&dl);
            }
            state.lock().unwrap().body = body;
            send(&mut wr, "250 2.0.0 Ok: queued as FAKE1").await;
        } else if line.starts_with("QUIT") {
            send(&mut wr, "221 2.0.0 Bye").await;
            return;
        } else {
            send(&mut wr, "250 OK").await;
        }
    }
}

// ===== Tests =====

#[tokio::test]
async fn email_send_delivers_full_message() {
    let _g = lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let fake = FakeSmtp::start("250 OK", "250 OK", true).await;
    let mut cfg = fake.cfg();
    cfg["username"] = json!("artex");
    cfg["password"] = json!("pw");

    let kept = channel("email")
        .unwrap()
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("delivery failed");
    assert_eq!(kept, 1, "a single message counts as one delivery");

    for want in [
        "MAIL FROM:<artex@example.com>",
        "RCPT TO:<a@example.com>",
        "RCPT TO:<b@example.com>",
        "DATA",
        "AUTH",
        "QUIT",
    ] {
        assert!(fake.saw_command(want), "missing SMTP command {want:?}");
    }

    let body = fake.body();
    assert!(!body.is_empty(), "no body received in the DATA phase");
    assert!(
        body.contains("Content-Type: text/html"),
        "missing Content-Type:\n{body}"
    );
    assert!(body.contains("base64"), "body not base64-encoded:\n{body}");
    assert!(
        body.contains("a@example.com, b@example.com"),
        "To header missing recipients:\n{body}"
    );
}

#[tokio::test]
async fn email_send_without_auth() {
    let _g = lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    // No account configured: AUTH must never be sent (some relays reject it).
    let fake = FakeSmtp::start("250 OK", "250 OK", true).await;
    let cfg = fake.cfg();

    channel("email")
        .unwrap()
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("delivery failed");
    assert!(
        !fake.saw_command("AUTH"),
        "AUTH sent despite no account configured"
    );
}

#[tokio::test]
async fn email_send_classifies_smtp_replies() {
    let _g = lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");

    // (rcpt_reply, mail_reply, permanent)
    let cases: &[(&str, &str, bool)] = &[
        ("550 5.1.1 User unknown", "250 OK", true),
        ("450 4.7.1 Greylisting in action", "250 OK", false),
        ("452 4.2.2 Mailbox full", "250 OK", false),
        ("250 OK", "553 5.1.3 Bad address", true),
        ("250 OK", "451 4.3.0 Temporary failure", false),
    ];

    for (rcpt, mail, want_permanent) in cases {
        let fake = FakeSmtp::start(mail, rcpt, false).await;
        let cfg = fake.cfg();
        let err = channel("email")
            .unwrap()
            .send(&Context::background(), &cfg, &single_msg())
            .await
            .expect_err("should error");
        let permanent = matches!(err, Error::Permanent(_));
        assert_eq!(
            permanent, *want_permanent,
            "wrong permanent classification for rcpt={rcpt:?} mail={mail:?}: {err:?}"
        );
        let msg = err.message();
        let rcpt_code = rcpt.split_whitespace().next().unwrap_or("");
        let mail_code = mail.split_whitespace().next().unwrap_or("");
        assert!(
            msg.contains(rcpt_code) || msg.contains(mail_code),
            "error should keep the server's reply code: {msg}"
        );
    }
}

#[tokio::test]
async fn email_validate_reports_missing_fields() {
    let ch = channel("email").unwrap();
    let cases = [
        json!({"port": 25, "from": "a@b.c", "to": ["d@e.f"]}), // missing host
        json!({"host": "smtp.example.com"}),                   // missing port
        json!({"host": "h", "port": 70000, "from": "a@b.c", "to": ["d@e.f"]}), // port out of range
        json!({"host": "h", "port": 25, "to": ["d@e.f"]}),     // missing from
        json!({"host": "h", "port": 25, "from": "a@b.c"}),     // missing to
    ];
    for cfg in &cases {
        assert!(ch.validate(cfg).is_err(), "should fail validation: {cfg}");
    }
}

#[test]
fn email_config_tolerance() {
    // The form sends everything as strings: a numeric port, a single recipient,
    // a boolean-as-string tls. Validation must tolerate all of them.
    let cfg = json!({
        "host": "smtp.example.com",
        "port": "587",
        "from": "a@b.c",
        "to": "d@e.f",
        "tls": "true",
    });
    let ch = channel("email").unwrap();
    ch.validate(&cfg)
        .expect("should tolerate numbers/booleans in string form");

    assert_eq!(artex::notify::cfg_int(&cfg, "port"), 587);
    assert!(artex::notify::cfg_bool(&cfg, "tls"));
    let to = artex::notify::cfg_strings(&cfg, "to").unwrap();
    assert_eq!(to, vec!["d@e.f".to_string()]);
}

#[tokio::test]
async fn email_dial_guard_rejects_loopback_by_default() {
    let _g = lock_env().await;
    std::env::remove_var(ALLOW_LOCAL_TARGETS_ENV); // default: no local delivery
    let fake = FakeSmtp::start("250 OK", "250 OK", false).await;
    let cfg = fake.cfg();

    let err = channel("email")
        .unwrap()
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect_err("loopback delivery should be refused by default");
    assert!(
        !fake.saw_command("EHLO") && !fake.saw_command("HELO"),
        "SMTP session was established — the guard did not work"
    );
    assert!(
        err.message().contains(ALLOW_LOCAL_TARGETS_ENV),
        "refusal should explain how to opt in: {err:?}"
    );
}

#[tokio::test]
async fn email_dial_guard_allows_loopback_when_opted_in() {
    let _g = lock_env().await;
    std::env::set_var(ALLOW_LOCAL_TARGETS_ENV, "1");
    let fake = FakeSmtp::start("250 OK", "250 OK", false).await;
    let cfg = fake.cfg();

    channel("email")
        .unwrap()
        .send(&Context::background(), &cfg, &single_msg())
        .await
        .expect("local SMTP should be deliverable once explicitly allowed");
    assert!(
        fake.saw_command("EHLO"),
        "no EHLO seen — the session was never established"
    );
}
