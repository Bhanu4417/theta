//! Provider-neutral LLM layer.
//!
//! This is Theta's own `pi-ai` equivalent: one [`Provider`] trait that turns a
//! [`ChatRequest`] into a stream of [`ProviderEvent`]s, independent of any
//! vendor wire format. Concrete providers (OpenAI-compatible, Anthropic,
//! Google) live next to this module and translate to/from it.

pub mod anthropic;
pub mod catalog;
pub mod google;
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

/// An inline image attached to a message (base64 data, no `data:` prefix).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImagePart {
    pub mime: String,
    pub data: String,
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
    /// Provider-reported token usage, captured on assistant messages so the
    /// context estimate can use the real prompt size (Pi's
    /// `getLastAssistantUsage` + tail estimate).
    #[serde(default)]
    pub tokens: Option<crate::harness::transcript::TokenUsage>,
    /// Inline images (vision models).
    #[serde(default)]
    pub images: Vec<ImagePart>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, text: text.into(), tool_calls: Vec::new(), tool_call_id: None, tokens: None, images: Vec::new() }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, text: text.into(), tool_calls: Vec::new(), tool_call_id: None, tokens: None, images: Vec::new() }
    }
    /// A user message carrying inline images.
    pub fn user_with_images(text: impl Into<String>, images: Vec<ImagePart>) -> Self {
        let mut m = Self::user(text);
        m.images = images;
        m
    }
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, text: text.into(), tool_calls, tool_call_id: None, tokens: None, images: Vec::new() }
    }
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            tokens: None,
            images: Vec::new(),
        }
    }
}

/// Parse a `data:` or `file://` image URL into an inline image part.
pub fn image_from_url(mime: &str, url: &str) -> Option<ImagePart> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        if !meta.contains("base64") {
            return None;
        }
        let m = meta.split(';').next().unwrap_or("").trim();
        let mime = if m.is_empty() { mime.to_string() } else { m.to_string() };
        if !mime.starts_with("image/") {
            return None;
        }
        return Some(ImagePart { mime, data: data.replace(['\n', '\r'], "") });
    }
    if let Some(path) = url.strip_prefix("file://") {
        if !mime.starts_with("image/") {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        return Some(ImagePart { mime: mime.to_string(), data: base64_encode(&bytes) });
    }
    None
}

/// Minimal standard base64 encoder (RFC 4648) — no extra dependency.
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
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

/// Transient failures worth retrying (network, gateway, upstream overload).
pub fn is_retryable_provider_error(e: &ProviderError) -> bool {
    match e {
        ProviderError::Transport(_) | ProviderError::Unavailable(_) => true,
        ProviderError::Protocol(m) => {
            let m = m.to_ascii_lowercase();
            m.contains("429") || m.contains("500") || m.contains("502")
                || m.contains("503") || m.contains("504") || m.contains("overloaded")
                || m.contains("timeout")
        }
        _ => false,
    }
}

/// Stream a request, retrying transient failures with exponential backoff.
pub async fn stream_with_retry(
    provider: &dyn Provider,
    request: ChatRequest,
    on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    max_retries: u32,
    base_ms: u64,
) -> Result<AssistantTurn, ProviderError> {
    let mut attempt = 0u32;
    loop {
        match provider.stream(request.clone(), &mut *on_event).await {
            Ok(turn) => return Ok(turn),
            Err(e) => {
                if attempt < max_retries && is_retryable_provider_error(&e) {
                    attempt += 1;
                    let delay = base_ms.saturating_mul(1u64 << (attempt - 1).min(6));
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
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
    fn retryable_classification() {
        use crate::providers::ProviderError;
        assert!(is_retryable_provider_error(&ProviderError::Transport("x".into())));
        assert!(is_retryable_provider_error(&ProviderError::Unavailable("x".into())));
        assert!(is_retryable_provider_error(&ProviderError::Protocol("HTTP 503".into())));
        assert!(!is_retryable_provider_error(&ProviderError::Auth("x".into())));
        assert!(!is_retryable_provider_error(&ProviderError::Unsupported("x".into())));
    }

    #[test]
    fn base64_and_image_urls() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b""), "");
        let img = image_from_url(
            "image/png",
            "data:image/png;base64,iVBORw0KGgo=",
        )
        .unwrap();
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.data, "iVBORw0KGgo=");
        // Non-images are ignored.
        assert!(image_from_url("text/plain", "data:text/plain;base64,aGk=").is_none());
    }

    #[test]
    fn image_from_file_url_is_base64_encoded() {
        let dir = std::env::temp_dir().join(format!("theta-img-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.png");
        std::fs::write(&p, b"hi").unwrap();
        let img = image_from_url("image/png", &format!("file://{}", p.display())).unwrap();
        assert_eq!(img.data, base64_encode(b"hi"));
        assert!(image_from_url("text/plain", &format!("file://{}", p.display())).is_none());
        let _ = std::fs::remove_dir_all(&dir);
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
