//! Provider-neutral LLM layer.
//!
//! This is Theta's own `pi-ai` equivalent: one [`Provider`] trait that turns a
//! [`ChatRequest`] into a stream of [`ProviderEvent`]s, independent of any
//! vendor wire format. Concrete providers (OpenAI-compatible, Anthropic,
//! Google) live next to this module and translate to/from it.

pub mod catalog;
pub mod openai;

use crate::providers::ProviderError;
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON string of arguments (as streamed by the provider).
    pub arguments: String,
}

/// One message in the model conversation. Kept deliberately flat and
/// serializable so every provider can map it to its own shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
    /// Set on assistant messages that requested tools.
    pub tool_calls: Vec<ToolCall>,
    /// Set on tool-result messages: which call this answers.
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, text: text.into(), tool_calls: Vec::new(), tool_call_id: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, text: text.into(), tool_calls: Vec::new(), tool_call_id: None }
    }
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, text: text.into(), tool_calls, tool_call_id: None }
    }
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// Declaration of a tool the model may call, in JSON Schema.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

/// A normalized streaming event. Providers translate their wire format into
/// these; the agent loop never sees vendor JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCall),
    Usage { input: u64, output: u64 },
    Done(FinishReason),
}

/// The fully assembled result of one model turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantTurn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: Option<FinishReason>,
}

/// The core LLM contract. `stream` invokes `on_event` as deltas arrive and
/// returns the assembled turn (text + tool calls). Boxed future keeps the
/// trait dyn-compatible, so the harness can hold `Box<dyn Provider>`.
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Pin<Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>>;
}

/// Split buffered SSE bytes into complete `data:` payloads. Chunks may split a
/// line anywhere; only complete lines are yielded.
pub(crate) fn sse_data_lines(buf: &mut Vec<u8>, chunk: &[u8]) -> Vec<String> {
    buf.extend_from_slice(chunk);
    let mut out = Vec::new();
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=pos).collect();
        let line = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)]);
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if !data.is_empty() {
                out.push(data.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_constructors() {
        assert_eq!(ChatMessage::system("s").role, Role::System);
        let a = ChatMessage::assistant("hi", vec![ToolCall {
            id: "1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        }]);
        assert_eq!(a.tool_calls.len(), 1);
        let t = ChatMessage::tool_result("1", "ok");
        assert_eq!(t.tool_call_id.as_deref(), Some("1"));
    }

    #[test]
    fn sse_lines_reassemble_across_chunks() {
        let mut buf = Vec::new();
        assert!(sse_data_lines(&mut buf, b"data: {\"a\"").is_empty());
        let got = sse_data_lines(&mut buf, b":1}\ndata: [DONE]\n");
        assert_eq!(got, vec!["{\"a\":1}".to_string(), "[DONE]".to_string()]);
        // Non-data lines (comments/heartbeats) are ignored.
        assert!(sse_data_lines(&mut buf, b"\n: ping\n\n").is_empty());
    }
}
