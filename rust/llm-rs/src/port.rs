//! Rust port of the pure parts of the external `norma/llm` message model.
//!
//! The `Role` and `BlockType` enums are plain strings in Go; here they are `String`
//! fields with the canonical tags exposed as constants. `Usage` is `Copy` so the
//! `Add` / `MergeEcho` receivers take their argument by value, exactly like Go's
//! `func (u *Usage) Add(o Usage)`.

use std::collections::HashSet;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Block-type tags (Go `llm.BlockType` constants).
pub const BLOCK_TEXT: &str = "text";
pub const BLOCK_THINKING: &str = "thinking";
pub const BLOCK_TOOL_USE: &str = "tool_use";
pub const BLOCK_TOOL_RESULT: &str = "tool_result";
pub const BLOCK_REDACTED_THINKING: &str = "redacted_thinking";

/// Role tags (Go `llm.Role` constants).
pub const ROLE_USER: &str = "user";
pub const ROLE_ASSISTANT: &str = "assistant";

/// `llm.ContentBlock`: one piece of message content (the Anthropic content-block union).
/// Which fields are populated depends on `block_type`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    /// Text block payload.
    #[serde(default)]
    pub text: String,
    /// Thinking block payload.
    #[serde(default)]
    pub thinking: String,
    /// Thinking block signature (replayed verbatim on multi-turn history).
    #[serde(default)]
    pub signature: String,
    /// Redacted-thinking encrypted payload.
    #[serde(default)]
    pub data: String,
    /// ToolUse id.
    #[serde(default)]
    pub id: String,
    /// ToolUse tool name.
    #[serde(default)]
    pub name: String,
    /// ToolUse arguments (Go `json.RawMessage`).
    #[serde(default)]
    pub input: Option<Value>,
    /// Gemini per-tool-call token.
    #[serde(default, rename = "thought_signature")]
    pub thought_signature: String,
    /// ToolResult: the tool_use id this answers.
    #[serde(default, rename = "tool_use_id")]
    pub tool_use_id: String,
    /// ToolResult: nested content blocks.
    #[serde(default, deserialize_with = "de_null_default")]
    pub content: Vec<ContentBlock>,
    /// ToolResult: whether the tool errored.
    #[serde(default, rename = "is_error")]
    pub is_error: bool,
}

impl ContentBlock {
    fn new(block_type: &str) -> Self {
        ContentBlock {
            block_type: block_type.to_string(),
            text: String::new(),
            thinking: String::new(),
            signature: String::new(),
            data: String::new(),
            id: String::new(),
            name: String::new(),
            input: None,
            thought_signature: String::new(),
            tool_use_id: String::new(),
            content: Vec::new(),
            is_error: false,
        }
    }
}

/// `llm.TextBlock`: a text content block.
pub fn text_block(s: &str) -> ContentBlock {
    let mut b = ContentBlock::new(BLOCK_TEXT);
    b.text = s.to_string();
    b
}

/// `llm.ToolResultText`: a tool_result block carrying a single text block.
pub fn tool_result_text(tool_use_id: &str, text: &str, is_error: bool) -> ContentBlock {
    let mut b = ContentBlock::new(BLOCK_TOOL_RESULT);
    b.tool_use_id = tool_use_id.to_string();
    b.content = vec![text_block(text)];
    b.is_error = is_error;
    b
}

/// Mirrors Go `ContentBlock.MarshalJSON`: a thinking block always emits `"thinking"`
/// (even when empty) and never the other fields; every other block marshals its
/// populated fields with `omitempty` semantics.
impl Serialize for ContentBlock {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("type", &self.block_type)?;
        if self.block_type == BLOCK_THINKING {
            m.serialize_entry("thinking", &self.thinking)?;
            if !self.signature.is_empty() {
                m.serialize_entry("signature", &self.signature)?;
            }
        } else {
            if !self.text.is_empty() {
                m.serialize_entry("text", &self.text)?;
            }
            if !self.thinking.is_empty() {
                m.serialize_entry("thinking", &self.thinking)?;
            }
            if !self.signature.is_empty() {
                m.serialize_entry("signature", &self.signature)?;
            }
            if !self.data.is_empty() {
                m.serialize_entry("data", &self.data)?;
            }
            if !self.id.is_empty() {
                m.serialize_entry("id", &self.id)?;
            }
            if !self.name.is_empty() {
                m.serialize_entry("name", &self.name)?;
            }
            if let Some(input) = &self.input {
                // Go's `json.RawMessage` `omitempty` drops a nil/empty payload.
                if !matches!(input, Value::Null) {
                    m.serialize_entry("input", input)?;
                }
            }
            if !self.thought_signature.is_empty() {
                m.serialize_entry("thought_signature", &self.thought_signature)?;
            }
            if !self.tool_use_id.is_empty() {
                m.serialize_entry("tool_use_id", &self.tool_use_id)?;
            }
            if !self.content.is_empty() {
                m.serialize_entry("content", &self.content)?;
            }
            if self.is_error {
                m.serialize_entry("is_error", &self.is_error)?;
            }
        }
        m.end()
    }
}

/// `llm.Message`: a single conversation turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    /// Go marshals a nil `Content` as `null` (no `omitempty`); accept both `null`
    /// and a missing field as an empty list.
    #[serde(default, deserialize_with = "de_null_default")]
    pub content: Vec<ContentBlock>,
}

/// `llm.UserText`: a user message with one text block.
pub fn user_text(s: &str) -> Message {
    Message {
        role: ROLE_USER.to_string(),
        content: vec![text_block(s)],
    }
}

impl Message {
    /// `Message.ToolUses`: the `tool_use` blocks of the message.
    pub fn tool_uses(&self) -> Vec<ContentBlock> {
        self.content
            .iter()
            .filter(|b| b.block_type == BLOCK_TOOL_USE)
            .cloned()
            .collect()
    }

    /// `Message.Text`: concatenation of all text blocks.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for b in &self.content {
            if b.block_type == BLOCK_TEXT {
                s.push_str(&b.text);
            }
        }
        s
    }
}

/// `llm.Usage`: token accounting, including prompt-cache tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(rename = "input_tokens")]
    pub input_tokens: i64,
    #[serde(rename = "output_tokens")]
    pub output_tokens: i64,
    #[serde(rename = "cache_read_tokens")]
    pub cache_read_tokens: i64,
    #[serde(rename = "cache_write_tokens")]
    pub cache_write_tokens: i64,
}

impl Usage {
    /// `Usage.Add`: sum token counts from another `Usage`.
    pub fn add(&mut self, o: Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
    }

    /// `Usage.MergeEcho`: output keeps accumulating; input/cache take the max so a
    /// re-echoed field (Anthropic repeats the input side across `message_start` /
    /// `message_delta`) is recovered rather than double-counted.
    pub fn merge_echo(&mut self, o: Usage) {
        self.output_tokens += o.output_tokens;
        self.input_tokens = self.input_tokens.max(o.input_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(o.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(o.cache_write_tokens);
    }
}

/// `llm.ToolSchema`: a tool definition exposed to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// Go `map[string]any`; modeled as a JSON value. Only its top-level key count
    /// (`len` in Go) is read by the downstream token estimate.
    #[serde(default, rename = "input_schema")]
    pub input_schema: Value,
}

impl ToolSchema {
    /// Go `len(t.InputSchema)`: number of top-level keys (0 for a nil / absent map).
    pub fn input_schema_len(&self) -> usize {
        self.input_schema.as_object().map_or(0, |m| m.len())
    }
}

/// `llm.CompletionRequest`: a single, provider-neutral model invocation. The Go struct
/// carries no JSON tags, so it marshals with its PascalCase field names
/// (`rename_all = "PascalCase"`), and its nil slices marshal as `null` (handled by the
/// `null`-tolerant deserializer). Only `System`, `Messages`, and `Tools` are read by the
/// ported helpers; the rest are carried for wire-shape fidelity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CompletionRequest {
    #[serde(default, deserialize_with = "de_null_default")]
    pub system: Vec<String>,
    #[serde(default)]
    pub dynamic_boundary: i64,
    #[serde(default, deserialize_with = "de_null_default")]
    pub messages: Vec<Message>,
    #[serde(default, deserialize_with = "de_null_default")]
    pub tools: Vec<ToolSchema>,
    #[serde(default)]
    pub max_tokens: i64,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default, deserialize_with = "de_null_default")]
    pub stop: Vec<String>,
    #[serde(default)]
    pub thinking: String,
}

// === Boundary (norma/llm boundary.go) ===

/// `llm.BlockCompactBoundary` (`boundary.go`): marker block type for a compaction point.
pub const BLOCK_COMPACT_BOUNDARY: &str = "compact_boundary";

/// `llm.BoundaryMeta` (`boundary.go`). `Trigger` is `omitempty`, reproduced on the wire
/// by the manual `Serialize` below.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct BoundaryMeta {
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub pre_tokens: i64,
    #[serde(default)]
    pub messages_summarized: i64,
    #[serde(default)]
    pub post_tokens: i64,
}

impl Serialize for BoundaryMeta {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if !self.trigger.is_empty() {
            m.serialize_entry("trigger", &self.trigger)?;
        }
        m.serialize_entry("pre_tokens", &self.pre_tokens)?;
        m.serialize_entry("messages_summarized", &self.messages_summarized)?;
        m.serialize_entry("post_tokens", &self.post_tokens)?;
        m.end()
    }
}

/// `llm.BoundaryStatus` (`boundary.go`) — why a boundary was classified as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryStatus {
    /// `BoundaryNone`
    None,
    /// `BoundaryComplete`
    Complete,
    /// `BoundaryNoMessage`
    NoMessage,
    /// `BoundaryNoTokens`
    NoTokens,
    /// `BoundaryIncomplete`
    Incomplete,
}

impl BoundaryStatus {
    /// Go `String()` value for each constant.
    pub fn as_str(self) -> &'static str {
        match self {
            BoundaryStatus::None => "none",
            BoundaryStatus::Complete => "complete",
            BoundaryStatus::NoMessage => "no_message",
            BoundaryStatus::NoTokens => "no_tokens",
            BoundaryStatus::Incomplete => "incomplete",
        }
    }
}

/// Scalar inputs for [`parse_boundary_meta`] (a pointer-free stand-in for `*Message`).
#[derive(Debug, Clone, Default)]
pub struct ParseBoundaryMetaInput {
    pub has_text: bool,
    pub text: String,
    pub has_content: bool,
    pub has_thinking: bool,
    pub has_signature: bool,
    pub has_tool_use_id: bool,
    pub has_tool_calls: bool,
}

/// `llm.IsBoundaryMarker` (`boundary.go`): a message is a boundary marker iff it is an
/// assistant message whose only content block is a `compact_boundary` block.
pub fn is_boundary_marker(m: &Message) -> bool {
    m.role == ROLE_ASSISTANT
        && m.content.len() == 1
        && m.content[0].block_type == BLOCK_COMPACT_BOUNDARY
}

/// `llm.LastBoundaryIndex` (`boundary.go`): index of the last boundary marker, or `-1`.
pub fn last_boundary_index(msgs: &[Message]) -> i64 {
    let mut last: i64 = -1;
    for (i, m) in msgs.iter().enumerate() {
        if is_boundary_marker(m) {
            last = i as i64;
        }
    }
    last
}

/// `llm.MessagesAfterBoundary` (`boundary.go`): messages strictly after the last boundary
/// marker; the whole slice when there is no boundary.
pub fn messages_after_boundary(msgs: &[Message]) -> Vec<Message> {
    let last = last_boundary_index(msgs);
    if last < 0 {
        return msgs.to_vec();
    }
    msgs[(last as usize + 1)..].to_vec()
}

/// `llm.MessagesForAPI` (`boundary.go`): drop everything up to and including the last
/// compaction boundary (never send a boundary marker to the model), then pair tool
/// blocks. Pure; the read side of the compaction contract.
pub fn messages_for_api(msgs: &[Message]) -> Vec<Message> {
    pair_tool_blocks(messages_after_boundary(msgs).as_slice())
}

/// `llm.PairToolBlocks` (`boundary.go`): drop `tool_result` blocks with no preceding
/// `tool_use` of the same id, and any `tool_use` block left unpaired.
pub fn pair_tool_blocks(msgs: &[Message]) -> Vec<Message> {
    let mut pending: HashSet<String> = HashSet::new();
    let mut out: Vec<Message> = Vec::new();
    for m in msgs {
        let mut kept: Vec<ContentBlock> = Vec::new();
        for b in &m.content {
            match b.block_type.as_str() {
                "tool_result" => {
                    if !pending.contains(&b.tool_use_id) {
                        continue; // orphan result — drop
                    }
                    pending.remove(&b.tool_use_id);
                }
                "tool_use" => {
                    pending.insert(b.id.clone());
                }
                _ => {}
            }
            kept.push(b.clone());
        }
        if kept.is_empty() {
            continue;
        }
        let mut nm = m.clone();
        nm.content = kept;
        out.push(nm);
    }
    out
}

/// `llm.BoundaryMessage` (`boundary.go`): a boundary marker carrying `meta`.
pub fn boundary_message(meta: &BoundaryMeta) -> Message {
    Message {
        role: ROLE_ASSISTANT.to_string(),
        content: vec![ContentBlock {
            block_type: BLOCK_COMPACT_BOUNDARY.to_string(),
            input: Some(serde_json::to_value(meta).unwrap_or(Value::Null)),
            ..ContentBlock::default()
        }],
    }
}

/// `llm.ParseBoundaryMeta` (`boundary.go`): decode a message's `BoundaryMeta` and report
/// whether it is complete (`pre_tokens > 0`, some message summarized, `post_tokens >= 0`).
pub fn parse_boundary_meta(inp: &ParseBoundaryMetaInput) -> (BoundaryMeta, BoundaryStatus) {
    let mut meta = BoundaryMeta::default();
    if inp.has_text && !inp.text.is_empty() {
        if let Ok(v) = serde_json::from_str::<BoundaryMeta>(&inp.text) {
            meta = v;
        }
    } else if inp.has_content
        || inp.has_thinking
        || inp.has_signature
        || inp.has_tool_use_id
        || inp.has_tool_calls
    {
        return (meta, BoundaryStatus::None);
    }
    if meta.pre_tokens > 0 && meta.messages_summarized > 0 && meta.post_tokens >= 0 {
        return (meta, BoundaryStatus::Complete);
    }
    if meta.pre_tokens <= 0 {
        return (meta, BoundaryStatus::NoTokens);
    }
    if meta.messages_summarized == 0 {
        return (meta, BoundaryStatus::NoMessage);
    }
    (meta, BoundaryStatus::Incomplete)
}

/// Deserialize a field that Go may encode as `null` (nil slice) or omit entirely into
/// the type's default; otherwise deserialize normally.
fn de_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
