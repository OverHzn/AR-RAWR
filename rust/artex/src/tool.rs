//! Rust port of the dynamic surface of the external `norma/tool` package.
//!
//! The *pure* value helpers (`Result`, `Text`, `Errorf`, the deferred/todo
//! formatters) are already parity-tested in `tool-rs` and re-exported here. What
//! `mcphttp` (and the later agent/server layer) additionally needs is the
//! executable half of the package: the [`CoreTool`] contract, the [`Spec`] →
//! [`build`] constructor with its fail-closed defaults, the per-call
//! [`ToolContext`], and [`capture`] — the output-bounder every large-output tool
//! returns through. Those are modeled here because they have no `*-rs` home (they
//! are trait-object / I/O machinery, not pure data), and they are proven against
//! the ported Go specs in `tests/tool_capture_test.rs`.
//!
//! Go's `CoreTool` is an interface; the Rust analogue is a trait, and [`build`]
//! returns `Arc<dyn CoreTool>`. `ToolContext.Emit` (a progress callback) and
//! `.Tasks` (the background-task `Manager`) are agent-runtime concerns that land
//! with that layer; [`ToolContext`] carries the value fields [`capture`] reads
//! (`max_output_chars`, `output_dir`) plus the routing identity fields.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::llm::{text_block, ContentBlock, Context};
use crate::permission::{Context as PermissionContext, Decision};

// Re-export the pure `norma/tool` value helpers from the parity crate so the rest
// of `artex` builds against `crate::tool` rather than reaching into `tool-rs`.
pub use tool_rs::port::{
    errorf, render_deferred_tools_block, render_todo_reminder, text, Result, Todo,
    EXECUTE_EXTRA_TOOLS_NAME, SEARCH_EXTRA_TOOLS_NAME, TODO_COMPLETED, TODO_IN_PROGRESS,
    TODO_PENDING,
};

/// `json.RawMessage`: the raw bytes of a tool's JSON input.
pub type RawInput = Vec<u8>;

/// `tool.ToolContext`: per-call execution state threaded into a tool.
///
/// `Emit` (progress) and `Tasks` (background manager) are agent-runtime hooks
/// added with that layer; only the value fields [`capture`] and [`ToolContext::resolve_path`]
/// read, plus the routing identity, are modeled now.
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// `ToolContext.WorkingDir`.
    pub working_dir: String,
    /// `ToolContext.AgentID`.
    pub agent_id: String,
    /// `ToolContext.ToolUseID` — the id of the `tool_use` block this call answers.
    pub tool_use_id: String,
    /// `ToolContext.MaxOutputChars` — the per-session output cap (0 → 30000 default).
    pub max_output_chars: i64,
    /// `ToolContext.OutputDir` — when set, oversized output spills to a file there.
    pub output_dir: String,
    /// `ToolContext.Env` — extra `KEY=VALUE` entries for Bash subprocesses.
    pub env: Vec<String>,
}

impl ToolContext {
    /// `tool.resolvePath`: join a relative path against the working directory.
    pub fn resolve_path(&self, p: &str) -> String {
        if gocompat::filepath::path_is_abs(p) || self.working_dir.is_empty() {
            return p.to_string();
        }
        gocompat::filepath::path_join(&[&self.working_dir, p])
    }
}

/// `tool.CoreTool`: a capability the agent can invoke. Go declares this as an
/// interface; the Rust port is a trait, and [`build`] returns `Arc<dyn CoreTool>`.
///
/// `call` mirrors Go's `(Result, error)`: a non-`None` second element is the
/// error string (`Err` in Go), a `None` carries the tool [`Result`].
#[async_trait::async_trait]
pub trait CoreTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn prompt(&self) -> &str;
    fn input_schema(&self) -> &Value;
    /// `CoreTool.IsReadOnly(input)`.
    fn is_read_only(&self, input: RawInput) -> bool;
    /// `CoreTool.IsConcurrencySafe(input)`.
    fn is_concurrency_safe(&self, input: RawInput) -> bool;
    /// `CoreTool.CheckPermissions(ctx, input, pc)`.
    fn check_permissions(&self, ctx: Context, input: RawInput, pc: &PermissionContext) -> Decision;
    /// `CoreTool.Call(ctx, input, tc)` — `(Result, error)`, error modelled as the
    /// `Some(message)` second element.
    async fn call(
        &self,
        ctx: Context,
        input: RawInput,
        tc: ToolContext,
    ) -> (Result, Option<String>);
}

type ReadOnlyFn = Arc<dyn Fn(RawInput) -> bool + Send + Sync>;
type ConcurrentFn = Arc<dyn Fn(RawInput) -> bool + Send + Sync>;
type PermissionsFn = Arc<dyn Fn(Context, RawInput, &PermissionContext) -> Decision + Send + Sync>;
type RunFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = (Result, Option<String>)> + Send + 'static>>;
type RunFn = Arc<dyn Fn(Context, RawInput, ToolContext) -> RunFuture + Send + Sync>;

/// `tool.Spec`: a tool declaration. [`build`] fills the unset behavioral fields
/// with fail-closed defaults (not read-only, not concurrency-safe, ask).
#[derive(Clone)]
pub struct Spec {
    pub name: String,
    pub description: String,
    #[allow(dead_code)]
    pub prompt: String,
    pub schema: Value,
    /// `Spec.ReadOnly`.
    pub read_only: Option<ReadOnlyFn>,
    /// `Spec.Concurrent`.
    pub concurrent: Option<ConcurrentFn>,
    /// `Spec.Permissions` — `None` → [`build`] defaults to `AskUser("approve <name>?")`.
    pub permissions: Option<PermissionsFn>,
    /// `Spec.Run` — required; the tool body.
    pub run: RunFn,
    /// `Spec.RawInput`.
    pub raw_input: bool,
}

impl std::fmt::Debug for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spec")
            .field("name", &self.name)
            .field("schema", &self.schema)
            .field("raw_input", &self.raw_input)
            .finish()
    }
}

struct BuiltTool {
    spec: Spec,
}

/// `tool.Build`: construct a [`CoreTool`] from a [`Spec`], applying fail-closed
/// defaults (not read-only, not concurrency-safe, requires approval).
pub fn build(spec: Spec) -> Arc<dyn CoreTool> {
    Arc::new(BuiltTool { spec })
}

#[async_trait::async_trait]
impl CoreTool for BuiltTool {
    fn name(&self) -> &str {
        &self.spec.name
    }
    fn description(&self) -> &str {
        &self.spec.description
    }
    fn prompt(&self) -> &str {
        &self.spec.prompt
    }
    fn input_schema(&self) -> &Value {
        &self.spec.schema
    }

    fn is_read_only(&self, input: RawInput) -> bool {
        match &self.spec.read_only {
            Some(f) => f(input),
            None => false,
        }
    }

    fn is_concurrency_safe(&self, input: RawInput) -> bool {
        match &self.spec.concurrent {
            Some(f) => f(input),
            None => false,
        }
    }

    fn check_permissions(&self, ctx: Context, input: RawInput, pc: &PermissionContext) -> Decision {
        match &self.spec.permissions {
            Some(f) => f(ctx, input, pc),
            None => crate::permission::ask_user(&format!("approve {}?", self.spec.name)),
        }
    }

    async fn call(
        &self,
        ctx: Context,
        input: RawInput,
        tc: ToolContext,
    ) -> (Result, Option<String>) {
        (self.spec.run)(ctx, input, tc).await
    }
}

// === Capture ===

const DEFAULT_MAX_OUTPUT: i64 = 30000;
/// `tool.captureNoticeSlack`.
const CAPTURE_NOTICE_SLACK: i64 = 4096;

static SPILL_SEQ: AtomicI64 = AtomicI64::new(0);

/// `tool.maxOut`: the output cap in effect for `tc` (its `MaxOutputChars`, or 30000).
pub fn max_output(tc: &ToolContext) -> i64 {
    if tc.max_output_chars > 0 {
        tc.max_output_chars
    } else {
        DEFAULT_MAX_OUTPUT
    }
}

/// `tool.Capture`: bound a tool's textual output for the model. When the output
/// exceeds [`max_output`] and an `OutputDir` is set, the full output is written to
/// a file there and a tight head + a pointer to that file is returned; otherwise a
/// head+tail truncation (overflow discarded) is returned. Both cuts are UTF-8
/// rune-boundary safe.
pub fn capture(tc: &ToolContext, s: &str) -> String {
    let max = max_output(tc);
    if (s.len() as i64) <= max {
        return s.to_string();
    }
    if !tc.output_dir.is_empty() {
        if let Some(path) = spill_output(&tc.output_dir, s) {
            let lines = s.bytes().filter(|b| *b == b'\n').count() as i64 + 1;
            return format!(
                "{}\n\n... <persisted-output>[Output too large: full {} bytes / {} lines.Full output saved to  {} </persisted-output>",
                cut_head(s, max as usize),
                s.len(),
                lines,
                path
            );
        }
    }
    truncate(s, max as usize)
}

/// `tool.CaptureOnce`: `Capture` with an idempotency guard — output that already
/// carries a `Capture` marker and is within `max + slack` is returned unchanged.
pub fn capture_once(tc: &ToolContext, s: &str) -> String {
    if already_captured(s) && (s.len() as i64) <= max_output(tc) + CAPTURE_NOTICE_SLACK {
        return s.to_string();
    }
    capture(tc, s)
}

/// `tool.alreadyCaptured`: whether `s` already carries a `Capture` marker.
fn already_captured(s: &str) -> bool {
    s.contains("<persisted-output>") || s.contains("characters truncated]")
}

/// `tool.spillOutput`: write the full output to an owner-only file under `dir`
/// (resolved to an absolute path) and return that absolute path, or `None` on
/// failure (the caller then falls back to `truncate`).
fn spill_output(dir: &str, s: &str) -> Option<String> {
    use std::io::Write;
    let abs = gocompat::filepath::path_abs(dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&abs);
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(&abs).ok()?;
    }
    let seq = SPILL_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!("output-{unix}-{seq}.txt");
    let path = gocompat::filepath::path_join(&[&abs, &name]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .ok()?;
        f.write_all(s.as_bytes()).ok()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, s.as_bytes()).ok()?;
    }
    Some(path)
}

/// `tool.truncate`: trim `s` to `max` bytes keeping head and tail, both cuts on
/// rune boundaries.
pub fn truncate(s: &str, max: usize) -> String {
    let bytes = s.as_bytes();
    if max == 0 || bytes.len() <= max {
        return s.to_string();
    }
    let half = max / 2;
    format!(
        "{}\n\n... [{} characters truncated] ...\n\n{}",
        cut_head(s, half),
        bytes.len() - max,
        cut_tail(s, half)
    )
}

/// `tool.cutHead`: the longest prefix of `s` at most `max` bytes AND ending on a
/// rune boundary.
pub fn cut_head(s: &str, max: usize) -> &str {
    &s[..rune_safe_len(s.as_bytes(), max)]
}

/// `tool.cutTail`: the longest suffix of `s` at most `max` bytes AND starting on a
/// rune boundary. Go slices `s` as a byte string and then drops leading
/// continuation bytes; the Rust port computes the aligned byte offset first (never
/// slicing the `str` at an unaligned offset, which would panic).
pub fn cut_tail(s: &str, max: usize) -> &str {
    if max == 0 {
        return "";
    }
    let bytes = s.as_bytes();
    if bytes.len() <= max {
        return s;
    }
    let start = bytes.len() - max;
    let aligned = start + rune_safe_start(&bytes[start..]);
    // `aligned` lands on a rune-start byte (not a 10xxxxxx continuation), i.e. a
    // valid `str` char boundary.
    &s[aligned..]
}

/// `tool.runeSafeLen`: the largest `n <= max` with `b[:n]` ending on a rune
/// boundary (a UTF-8 rune starts at a byte that is not a `10xxxxxx` continuation).
pub fn rune_safe_len(b: &[u8], max: usize) -> usize {
    if max == 0 {
        return 0;
    }
    if b.len() <= max {
        return b.len();
    }
    let mut max = max;
    while max > 0 && (b[max] & 0xC0) == 0x80 {
        max -= 1;
    }
    max
}

/// `tool.runeSafeStart`: the number of leading UTF-8 continuation bytes to drop so
/// `b[n:]` starts on a rune boundary.
pub fn rune_safe_start(b: &[u8]) -> usize {
    let mut i = 0;
    while i < b.len() && (b[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

/// A single text content block, for callers building a [`Result`] by hand (mirrors
/// `llm.TextBlock`).
pub fn single_text_block(s: &str) -> ContentBlock {
    text_block(s)
}
