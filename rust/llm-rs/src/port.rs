//! Rust port of the pure parts of the external `norma/llm` message model.
//!
//! The `Role` and `BlockType` enums are plain strings in Go; here they are `String`
//! fields with the canonical tags exposed as constants. `Usage` is `Copy` so the
//! `Add` / `MergeEcho` receivers take their argument by value, exactly like Go's
//! `func (u *Usage) Add(o Usage)`.

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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Deserialize a field that Go may encode as `null` (nil slice) or omit entirely into
/// the type's default; otherwise deserialize normally.
fn de_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}
