//! Raw LLM wire capture — pure port of `llmrec/capture.go`.
//!
//! The Recorder sees normalized requests/decoded events; the transport that
//! dials the provider taps the untouched bytes here. Every HTTP attempt is
//! kept, including discarded retries (rate-limit bodies are otherwise lost).

use std::io::Read;
use std::sync::{Arc, Mutex};

/// One HTTP round trip's status and mirrored response bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    pub status: i32,
    pub body: Vec<u8>,
}

#[derive(Debug, Default)]
struct State {
    request: Vec<u8>,
    attempts: Vec<Attempt>,
}

/// `Capture`: thread-safe collector for one logical LLM call.
#[derive(Debug, Default)]
pub struct Capture {
    mu: Mutex<State>,
}

/// A minimal context carrying the optional capture (Go's context.Value slot).
#[derive(Debug, Clone, Default)]
pub struct LlmContext {
    capture: Option<Arc<Capture>>,
}

/// `NewCapture`: a context carrying a fresh capture plus the capture itself.
pub fn new_capture() -> (LlmContext, Arc<Capture>) {
    let c = Arc::new(Capture::default());
    (
        LlmContext {
            capture: Some(c.clone()),
        },
        c,
    )
}

/// `CaptureFrom`: the attached capture, or None when recording is off.
pub fn capture_from(ctx: &LlmContext) -> Option<Arc<Capture>> {
    ctx.capture.clone()
}

impl Capture {
    /// `SetRequest`: keep only the first body (retries resend identical bytes).
    pub fn set_request(&self, body: &[u8]) {
        if body.is_empty() {
            return;
        }
        let mut s = self.mu.lock().unwrap();
        if s.request.is_empty() {
            s.request = body.to_vec();
        }
    }

    pub fn raw_request(&self) -> Vec<u8> {
        self.mu.lock().unwrap().request.clone()
    }

    /// `RawResponse`: a lone attempt is returned verbatim (replayable); several
    /// get per-attempt framing in order.
    pub fn raw_response(&self) -> Vec<u8> {
        let s = self.mu.lock().unwrap();
        match s.attempts.len() {
            0 => Vec::new(),
            1 => s.attempts[0].body.clone(),
            _ => {
                let mut out = Vec::new();
                for (i, a) in s.attempts.iter().enumerate() {
                    out.extend_from_slice(
                        format!(
                            "===== attempt {}/{} — HTTP {} =====\n",
                            i + 1,
                            s.attempts.len(),
                            a.status
                        )
                        .as_bytes(),
                    );
                    out.extend_from_slice(&a.body);
                    if !a.body.ends_with(b"\n") {
                        out.push(b'\n');
                    }
                }
                out
            }
        }
    }

    pub fn attempts(&self) -> Vec<Attempt> {
        self.mu.lock().unwrap().attempts.clone()
    }

    /// Register an attempt and wrap `rc` so reads mirror into it.
    pub fn tee_response<R: Read>(&self, status: i32, rc: R) -> CaptureReader<'_, R> {
        let mut s = self.mu.lock().unwrap();
        s.attempts.push(Attempt {
            status,
            body: Vec::new(),
        });
        let idx = s.attempts.len() - 1;
        CaptureReader {
            inner: rc,
            capture: self,
            idx,
        }
    }
}

/// A reader that mirrors everything it reads into its capture attempt.
pub struct CaptureReader<'a, R> {
    inner: R,
    capture: &'a Capture,
    idx: usize,
}

impl<R: Read> Read for CaptureReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.capture.mu.lock().unwrap().attempts[self.idx]
                .body
                .extend_from_slice(&buf[..n]);
        }
        Ok(n)
    }
}

/// `tee_or_passthrough`: the "recording off" path returns the body untouched.
pub enum MaybeTee<'a, R> {
    Plain(R),
    Tee(CaptureReader<'a, R>),
}

impl<R: Read> Read for MaybeTee<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            MaybeTee::Plain(r) => r.read(buf),
            MaybeTee::Tee(r) => r.read(buf),
        }
    }
}

/// Nil-safe helpers (Go calls these on a nil `*Capture` every request).
pub fn set_request(cap: Option<&Arc<Capture>>, body: &[u8]) {
    if let Some(c) = cap {
        c.set_request(body);
    }
}

pub fn raw_request(cap: Option<&Arc<Capture>>) -> Vec<u8> {
    cap.map(|c| c.raw_request()).unwrap_or_default()
}

pub fn raw_response(cap: Option<&Arc<Capture>>) -> Vec<u8> {
    cap.map(|c| c.raw_response()).unwrap_or_default()
}

pub fn attempts(cap: Option<&Arc<Capture>>) -> Vec<Attempt> {
    cap.map(|c| c.attempts()).unwrap_or_default()
}

pub fn tee_or_passthrough<'a, R: Read>(
    cap: Option<&'a Arc<Capture>>,
    status: i32,
    rc: R,
) -> MaybeTee<'a, R> {
    match cap {
        Some(c) => MaybeTee::Tee(c.tee_response(status, rc)),
        None => MaybeTee::Plain(rc),
    }
}
