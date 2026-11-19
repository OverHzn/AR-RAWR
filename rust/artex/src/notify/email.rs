//! Rust port of `notify/email.go` — SMTP email delivery.
//!
//! Email has the largest protocol surface of any notification target: a TCP dial,
//! an EHLO/STARTTLS/AUTH handshake, the MAIL/RCPT/DATA envelope, and each stage
//! fails differently. This port speaks that protocol directly rather than pulling
//! in a full ESMTP client crate, whose dependency would re-litigate exactly the
//! parts that matter here: the SSRF dial guard, credential handling, and turning
//! a peer's reply code into a retry classification.
//!
//! Only the password is masked; the host/account/recipients stay visible so
//! operators can see who a destination would reach. The protocol-level behaviour
//! is driven by `email_test.go`'s fake SMTP server (plaintext, no STARTTLS), so
//! the TLS paths are correct-by-construction (mirroring net/smtp) but only the
//! plaintext + dial-guard + plaintext-credential paths are exercised by tests.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::{lookup_host, TcpStream};

use super::html::{html_body, html_title};
use super::http::{allow_local_targets, is_blocked_dial_ip, ALLOW_LOCAL_TARGETS_ENV};
use super::{
    cfg_bool, cfg_int, cfg_string, cfg_strings, smtp_reply_code, Channel, Context, Error, Message,
};

// ===== Reply parsing + SMTP classification =====

/// A parsed SMTP reply: the three-digit code plus the text after it (the code
/// stripped, matching net/smtp's `textproto.Error` text).
struct Reply {
    code: i64,
    text: String,
}

/// Split a reply line into its leading three-digit code and the remaining text.
/// A line that does not start with three ASCII digits has code 0.
fn parse_reply_line(line: &str) -> Reply {
    let b = line.as_bytes();
    let mut code = 0i64;
    if b.len() >= 3 && b[..3].iter().all(|c| c.is_ascii_digit()) {
        code = line[..3].parse().unwrap_or(0);
    }
    let rest = &line[3..];
    let text = rest.strip_prefix(' ').unwrap_or(rest);
    Reply {
        code,
        text: text.to_string(),
    }
}

/// `smtpStageError`: classify a failed SMTP stage by the reply code. A 5xx is a
/// permanent refusal (retrying is pointless); everything else — including 4xx
/// greylisting and an unparseable/empty reply — is transient (retry). The original
/// text is always preserved so the operator can tell a bad address from a server
/// outage.
fn smtp_stage_error(what: &str, err_text: &str) -> Error {
    let code = smtp_reply_code(err_text);
    let msg = format!("{what}: {err_text}");
    if (500..600).contains(&code) {
        Error::permanent(msg)
    } else {
        Error::msg(msg)
    }
}

/// net/smtp's `isLocalhost`: loopback names that smtp.PlainAuth treats as a safe
/// place to send credentials in the clear.
fn is_localhost(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

// ===== RFC 2047 Q-encoding (mime.QEncoding.Encode parity) =====

/// The length in bytes of the UTF-8 sequence whose leading byte is `b`.
fn utf8_rune_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// `mime.QEncoding.Encode("utf-8", s)` — a faithful port of Go's RFC 2047
/// Q-encoding. ASCII-printable input (no control bytes, nothing above `~`) is
/// returned unchanged; otherwise the string is split into `=?utf-8?q?...?=`
/// encoded-words of at most 75 characters each (the split never breaks a UTF-8
/// sequence), with space encoded as `_` and everything outside the
/// printable-minus-special set written as `=XX` per byte.
fn q_encode(s: &str) -> String {
    let needs = s.chars().any(|r| !(' '..='~').contains(&r) && r != '\t');
    if !needs {
        return s.to_string();
    }
    const OPEN: &str = "=?utf-8?q?";
    // maxContentLen = 75 - len("=?UTF-8?q?") - len("?=") = 75 - 10 - 2 = 63.
    const MAX_CONTENT: usize = 63;

    let bytes = s.as_bytes();
    let mut out = String::new();
    out.push_str(OPEN);
    let mut current = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // A printable char (including space) that needs no per-byte escaping costs
        // one output char; anything else costs three per UTF-8 byte (=XX).
        let (rune_len, enc_len) =
            if (b' '..=b'~').contains(&b) && b != b'=' && b != b'?' && b != b'_' {
                (1usize, 1usize)
            } else {
                let rl = utf8_rune_len(b);
                (rl, 3 * rl)
            };
        if current + enc_len > MAX_CONTENT {
            out.push_str("?= ");
            out.push_str(OPEN);
            current = 0;
        }
        for &bb in &bytes[i..i + rune_len] {
            match bb {
                b' ' => out.push('_'),
                b'!'..=b'~' if bb != b'=' && bb != b'?' && bb != b'_' => out.push(bb as char),
                _ => out.push_str(&format!("={bb:02X}")),
            }
        }
        current += enc_len;
        i += rune_len;
    }
    out.push_str("?=");
    out
}

// ===== Message assembly (buildEmailMessage) =====

/// `buildEmailMessage`: build a complete RFC 5322 HTML email. The subject is the
/// title Q-encoded (it carries Chinese text and a bracketed severity tag); the
/// HTML body is base64-wrapped at 76 columns so long lines survive SMTP's
/// 1000-byte line limit and never begin with a dot (so no dot-stuffing is needed).
fn build_email_message(from: &str, to: &[String], m: &Message) -> String {
    let mut b = String::new();
    b.push_str(&format!("From: {from}\r\n"));
    b.push_str(&format!("To: {}\r\n", to.join(", ")));
    b.push_str(&format!("Subject: {}\r\n", q_encode(&html_title(m))));
    b.push_str("MIME-Version: 1.0\r\n");
    b.push_str("Content-Type: text/html; charset=\"UTF-8\"\r\n");
    b.push_str("Content-Transfer-Encoding: base64\r\n\r\n");

    let encoded = base64::engine::general_purpose::STANDARD.encode(html_body(m, 0).as_bytes());
    let mut rest: &str = &encoded;
    while rest.len() > 76 {
        b.push_str(&rest[..76]);
        b.push_str("\r\n");
        rest = &rest[76..];
    }
    b.push_str(rest);
    b.push_str("\r\n");
    b
}

// ===== SMTP transport =====

/// The dial + session timeouts (bound connection setup and the whole session, so
/// a stuck peer cannot hang the single delivery dispatcher forever).
const EMAIL_DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const EMAIL_SESSION_TIMEOUT: Duration = Duration::from_secs(45);

/// An owned stream that is either a plaintext TCP connection or a TLS one, so a
/// session can upgrade to TLS mid-conversation and keep one set of read/write
/// helpers.
enum SmtpStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for SmtpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            SmtpStream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            SmtpStream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for SmtpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            SmtpStream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            SmtpStream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmtpStream::Plain(s) => Pin::new(s).poll_flush(cx),
            SmtpStream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            SmtpStream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            SmtpStream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Read one CRLF-terminated line from the session (without the trailing CRLF).
async fn read_line(r: &mut BufReader<SmtpStream>) -> Result<String, Error> {
    let mut s = String::new();
    let n = r
        .read_line(&mut s)
        .await
        .map_err(|e| Error::msg(format!("smtp server read error: {e}")))?;
    if n == 0 {
        return Err(Error::msg("smtp server closed connection"));
    }
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    Ok(s)
}

/// Read one full SMTP reply (handling the `250-...` / `250 ...` continuation
/// form), returning the final code plus all lines' text joined by newlines.
async fn read_reply(r: &mut BufReader<SmtpStream>) -> Result<Reply, Error> {
    let mut lines: Vec<String> = Vec::new();
    loop {
        let line = read_line(r).await?;
        let cont = line.as_bytes().get(3) == Some(&b'-');
        lines.push(line);
        if !cont {
            break;
        }
    }
    let code = lines.last().map(|l| parse_reply_line(l).code).unwrap_or(0);
    let mut text = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&parse_reply_line(l).text);
    }
    Ok(Reply { code, text })
}

/// Write a single CRLF-terminated command line.
async fn write_line(r: &mut BufReader<SmtpStream>, line: &str) -> Result<(), Error> {
    r.get_mut()
        .write_all(line.as_bytes())
        .await
        .map_err(|e| Error::msg(format!("smtp write error: {e}")))?;
    r.get_mut()
        .write_all(b"\r\n")
        .await
        .map_err(|e| Error::msg(format!("smtp write error: {e}")))?;
    r.get_mut()
        .flush()
        .await
        .map_err(|e| Error::msg(format!("smtp write error: {e}")))?;
    Ok(())
}

/// Write a command and read its reply.
async fn cmd(r: &mut BufReader<SmtpStream>, line: &str) -> Result<Reply, Error> {
    write_line(r, line).await?;
    read_reply(r).await
}

/// Write the DATA payload followed by the terminating `.` line. The body is
/// base64, so it never begins a line with a dot; a trailing CRLF is ensured.
async fn write_data(r: &mut BufReader<SmtpStream>, body: &str) -> Result<(), Error> {
    r.get_mut()
        .write_all(body.as_bytes())
        .await
        .map_err(|e| Error::msg(format!("failed to write the email body: {e}")))?;
    if body.ends_with("\r\n") {
        r.get_mut()
            .write_all(b".\r\n")
            .await
            .map_err(|e| Error::msg(format!("failed to submit the email: {e}")))?;
    } else {
        r.get_mut()
            .write_all(b"\r\n.\r\n")
            .await
            .map_err(|e| Error::msg(format!("failed to submit the email: {e}")))?;
    }
    r.get_mut()
        .flush()
        .await
        .map_err(|e| Error::msg(format!("failed to submit the email: {e}")))?;
    Ok(())
}

/// The EHLO client name: the OS hostname, like net/smtp's `localName`.
fn ehlo_hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string())
}

fn strip_angle(s: &str) -> &str {
    s.trim().trim_matches('<').trim_matches('>')
}

/// Whether an EHLO reply advertises the given extension keyword.
fn ehlo_advertises(reply: &Reply, keyword: &str) -> bool {
    reply
        .text
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case(keyword))
}

/// Build the TLS connector once per process from the ring provider + webpki roots.
fn tls_connector() -> Result<tokio_rustls::TlsConnector, Error> {
    use tokio_rustls::rustls;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::permanent("failed to build SMTP TLS config"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// Upgrade a plaintext TCP stream to TLS for `host`.
async fn tls_connect(
    host: &str,
    tcp: TcpStream,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, Error> {
    use tokio_rustls::rustls;
    let connector = tls_connector()?;
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| Error::permanent("invalid SMTP TLS server name"))?;
    connector
        .connect(name, tcp)
        .await
        .map_err(|e| Error::msg(format!("SMTP TLS handshake failed: {e}")))
}

/// `sendEmailMessage`: run the whole SMTP conversation for one message.
#[allow(clippy::too_many_arguments)]
async fn send_email_message(
    host: &str,
    port: u16,
    implicit_tls: bool,
    username: &str,
    password: &str,
    from: &str,
    to: &[String],
    msg: &str,
) -> Result<(), Error> {
    let session = async {
        // --- Dial with the SSRF guard (the same address-class rule as HTTP) ---
        let addrs = lookup_host((host, port)).await.map_err(|e| {
            Error::permanent(format!(
                "failed to connect to the SMTP server: lookup {host}: {e}"
            ))
        })?;
        let addr = addrs.into_iter().next().ok_or_else(|| {
            Error::permanent(format!(
                "failed to connect to the SMTP server: lookup {host}: no addresses"
            ))
        })?;
        if is_blocked_dial_ip(addr.ip()) && !allow_local_targets() {
            return Err(Error::permanent(format!(
                "failed to connect to the SMTP server: refusing to deliver to \
loopback/link-local address {} (if you really need to deliver to a local service, \
set {ALLOW_LOCAL_TARGETS_ENV}=1)",
                addr.ip()
            )));
        }

        let tcp = tokio::time::timeout(EMAIL_DIAL_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| Error::msg("failed to connect to the SMTP server: dial timeout"))?
            .map_err(|e| Error::msg(format!("failed to connect to the SMTP server: {e}")))?;

        let mut tls_active = false;
        let stream = if implicit_tls {
            tls_active = true;
            SmtpStream::Tls(Box::new(tls_connect(host, tcp).await?))
        } else {
            SmtpStream::Plain(tcp)
        };
        let mut r = BufReader::new(stream);

        // Server greeting.
        let greet = read_reply(&mut r).await?;
        if greet.code >= 400 {
            return Err(smtp_stage_error(
                "connect",
                &format!("{} {}", greet.code, greet.text),
            ));
        }

        // EHLO.
        let ehlo = cmd(&mut r, &format!("EHLO {}", ehlo_hostname())).await?;
        if ehlo.code >= 400 {
            return Err(smtp_stage_error(
                "EHLO",
                &format!("{} {}", ehlo.code, ehlo.text),
            ));
        }

        // STARTTLS upgrade when offered (and we are not already on implicit TLS).
        if !implicit_tls && ehlo_advertises(&ehlo, "STARTTLS") {
            let r2 = cmd(&mut r, "STARTTLS").await?;
            if r2.code >= 400 {
                return Err(Error::msg(format!(
                    "STARTTLS failed: {} {}",
                    r2.code, r2.text
                )));
            }
            let tcp = match r.into_inner() {
                SmtpStream::Plain(t) => t,
                SmtpStream::Tls(_) => unreachable!("STARTTLS on an already-TLS stream"),
            };
            let tls = tls_connect(host, tcp).await?;
            tls_active = true;
            r = BufReader::new(SmtpStream::Tls(Box::new(tls)));
            // Re-issue EHLO inside the TLS tunnel, as RFC 3207 requires.
            let _ = cmd(&mut r, &format!("EHLO {}", ehlo_hostname())).await?;
        }

        // AUTH PLAIN, when an account is configured. smtp.PlainAuth refuses to
        // send credentials over a plaintext session to a non-local host; that
        // refusal is correct and must carry an actionable message.
        if !username.is_empty() {
            if !tls_active && !is_localhost(host) {
                return Err(Error::permanent(
                    "refusing to send credentials: the connection is not encrypted. \
Enable TLS, or use port 465 (implicit TLS), or tick the \"Enable TLS\" checkbox \
(unencrypted connection)",
                ));
            }
            let mut plain = vec![0u8];
            plain.extend_from_slice(username.as_bytes());
            plain.push(0);
            plain.extend_from_slice(password.as_bytes());
            let token = base64::engine::general_purpose::STANDARD.encode(&plain);
            let a = cmd(&mut r, &format!("AUTH PLAIN {token}")).await?;
            if a.code >= 400 {
                return Err(Error::permanent(format!(
                    "SMTP authentication failed: {} {}",
                    a.code, a.text
                )));
            }
        }

        // Envelope: MAIL FROM, then RCPT TO for each recipient.
        let mail = cmd(&mut r, &format!("MAIL FROM:<{}>", strip_angle(from))).await?;
        if mail.code >= 400 {
            return Err(smtp_stage_error(
                &format!("sender {from} rejected"),
                &format!("{} {}", mail.code, mail.text),
            ));
        }
        for rcpt in to {
            let rc = cmd(&mut r, &format!("RCPT TO:<{}>", strip_angle(rcpt))).await?;
            if rc.code >= 400 {
                return Err(smtp_stage_error(
                    &format!("recipient {rcpt} rejected"),
                    &format!("{} {}", rc.code, rc.text),
                ));
            }
        }

        // DATA: 354 go-ahead, then the body + '.', then the final 250.
        let data = cmd(&mut r, "DATA").await?;
        if data.code >= 400 {
            return Err(Error::msg(format!(
                "SMTP DATA failed: {} {}",
                data.code, data.text
            )));
        }
        write_data(&mut r, msg).await?;
        let end = read_reply(&mut r).await?;
        if end.code >= 400 {
            return Err(Error::msg(format!(
                "failed to submit the email: {} {}",
                end.code, end.text
            )));
        }

        // QUIT is best-effort; a failure here does not unsend the message.
        let _ = cmd(&mut r, "QUIT").await;
        Ok(())
    };

    tokio::time::timeout(EMAIL_SESSION_TIMEOUT, session)
        .await
        .map_err(|_| Error::msg("smtp session timeout"))?
}

// ===== Channel =====

/// The email channel: SMTP delivery of a full HTML summary.
pub struct EmailChannel;

#[async_trait]
impl Channel for EmailChannel {
    fn kind(&self) -> &'static str {
        "email"
    }

    fn validate(&self, cfg: &Value) -> Result<(), Error> {
        if cfg_string(cfg, "host").is_empty() {
            return Err(Error::msg("missing SMTP server address"));
        }
        let port = cfg_int(cfg, "port");
        if port <= 0 || port > 65535 {
            return Err(Error::msg("invalid SMTP port (should be 1-65535)"));
        }
        if cfg_string(cfg, "from").is_empty() {
            return Err(Error::msg("missing sender address"));
        }
        if cfg_strings(cfg, "to").map(|v| v.is_empty()).unwrap_or(true) {
            return Err(Error::msg("at least one recipient address is required"));
        }
        Ok(())
    }

    async fn send(&self, ctx: &Context, cfg: &Value, m: &Message) -> Result<usize, Error> {
        // Go: `if err := c.Validate(cfg); err != nil { return 0, Permanent(err) }`.
        self.validate(cfg)
            .map_err(|e| Error::permanent(e.message()))?;
        if ctx.is_cancelled() {
            return Err(Error::msg("context canceled"));
        }

        let host = cfg_string(cfg, "host");
        let port = cfg_int(cfg, "port") as u16;
        let from = cfg_string(cfg, "from");
        let to = cfg_strings(cfg, "to").unwrap_or_default();
        let username = cfg_string(cfg, "username");
        let password = cfg_string(cfg, "password");
        let implicit_tls = cfg_bool(cfg, "tls");

        let msg = build_email_message(&from, &to, m);

        send_email_message(
            &host,
            port,
            implicit_tls,
            &username,
            &password,
            &from,
            &to,
            &msg,
        )
        .await?;

        // Email does not truncate; the whole batch counts as delivered.
        Ok(m.items.len())
    }

    fn default_rate_per_min(&self) -> i64 {
        60
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{Item, Message};

    /// The standard single-item message used by the parity tests (`singleMsg()`).
    fn single_msg() -> Message {
        Message {
            items: vec![Item {
                finding_id: 42,
                name: "login page \"SQL injection\" risk".to_string(),
                vulnclass: "SQL injection".to_string(),
                severity: "high".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn q_encode_matches_go_for_the_title() {
        // Captured from Go's mime.QEncoding.Encode("utf-8", title).
        let title = "[🟠 High] login page \"SQL injection\" risk";
        assert_eq!(
            q_encode(title),
            "=?utf-8?q?[=F0=9F=9F=A0_High]_login_page_\"SQL_injection\"_risk?="
        );
    }

    #[test]
    fn q_encode_leaves_ascii_untouched() {
        assert_eq!(q_encode("plain ascii subject"), "plain ascii subject");
    }

    #[test]
    fn q_encode_splits_long_input() {
        let s = "🟠 ".repeat(40);
        let enc = q_encode(&s);
        // Every encoded-word is at most 75 characters.
        for word in enc.split(' ') {
            assert!(word.len() <= 75, "word too long: {word}");
            assert!(word.starts_with("=?utf-8?q?"));
            assert!(word.ends_with("?="));
        }
    }

    #[test]
    fn smtp_stage_error_classifies_by_code() {
        assert!(smtp_stage_error("x", "550 5.1.1 no user").is_permanent());
        assert!(smtp_stage_error("x", "553 5.1.3 bad").is_permanent());
        assert!(!smtp_stage_error("x", "450 4.7.1 greylist").is_permanent());
        assert!(!smtp_stage_error("x", "452 4.2.2 full").is_permanent());
        // Unparseable / empty -> transient.
        assert!(!smtp_stage_error("x", "unexpected EOF").is_permanent());
        assert!(!smtp_stage_error("x", "").is_permanent());
        // The original text is preserved.
        assert!(
            smtp_stage_error("recipient rejected", "550 5.1.1 User unknown")
                .message()
                .contains("550 5.1.1 User unknown")
        );
    }

    #[test]
    fn build_email_message_is_well_formed() {
        let m = single_msg();
        let msg = build_email_message(
            "artex@example.com",
            &["a@example.com".into(), "b@example.com".into()],
            &m,
        );
        assert!(msg.starts_with("From: artex@example.com\r\n"));
        assert!(msg.contains("To: a@example.com, b@example.com\r\n"));
        assert!(msg.contains("Subject: =?utf-8?"));
        // Split headers/body and base64-decode the body.
        let parts: Vec<&str> = msg.splitn(2, "\r\n\r\n").collect();
        assert_eq!(parts.len(), 2, "missing header/body separator");
        let body_b64 = parts[1].replace("\r\n", "");
        let html = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(body_b64.trim())
                .expect("body is base64"),
        )
        .unwrap();
        assert!(html.starts_with("<div"), "body is not HTML: {html:.80}");
        // Double quotes in the title's text position stay verbatim.
        assert!(html.contains("\"SQL injection\""));
    }

    #[test]
    fn is_localhost_matches_go() {
        assert!(is_localhost("localhost"));
        assert!(is_localhost("127.0.0.1"));
        assert!(is_localhost("::1"));
        assert!(!is_localhost("smtp.example.com"));
    }
}
