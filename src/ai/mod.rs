pub mod anthropic;
pub mod catalog;
pub mod discovery;
pub mod google;
pub mod openai;
pub mod responses;

use crate::providers::ProviderError;
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImagePart {
    pub mime: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tokens: Option<crate::harness::transcript::TokenUsage>,
    #[serde(default)]
    pub images: Vec<ImagePart>,
    #[serde(default)]
    pub cost: Option<f64>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, text: text.into(), tool_calls: Vec::new(), tool_call_id: None, tokens: None, images: Vec::new(), cost: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, text: text.into(), tool_calls: Vec::new(), tool_call_id: None, tokens: None, images: Vec::new(), cost: None }
    }
    pub fn user_with_images(text: impl Into<String>, images: Vec<ImagePart>) -> Self {
        let mut m = Self::user(text);
        m.images = images;
        m
    }
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, text: text.into(), tool_calls, tool_call_id: None, tokens: None, images: Vec::new(), cost: None }
    }
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            tokens: None,
            images: Vec::new(),
            cost: None,
        }
    }
}

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

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
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
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCall),
    Usage { input: u64, output: u64 },
    Done(FinishReason),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantTurn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: Option<FinishReason>,
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Pin<Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>>;

    fn list_models<'a>(
        &'a self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, ProviderError>> + Send + 'a>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn set_timeout(&mut self, _secs: u64) {}
}

pub fn provider_for(
    provider_id: &str,
    base_url: &str,
    key: Option<String>,
) -> Result<Box<dyn Provider>, ProviderError> {
    let id = provider_id.to_ascii_lowercase();
    if !base_url.trim().is_empty() {
        return Ok(Box::new(openai::OpenAiCompat::new(base_url, key)));
    }
    match id.as_str() {
        "anthropic" | "claude" => Ok(Box::new(anthropic::Anthropic::new(key))),
        "google" | "gemini" => Ok(Box::new(google::Google::new(key))),
        _ => openai::OpenAiCompat::preset(&id, key)
            .map(|p| Box::new(p) as Box<dyn Provider>)
            .ok_or_else(|| {
                ProviderError::Unsupported(format!(
                    "unknown ai.provider '{provider_id}' (use anthropic/google/openai/xai/… or set ai.base_url)"
                ))
            }),
    }
}

pub fn needs_responses_api(provider_id: &str, model: &str) -> bool {
    let id = provider_id.to_ascii_lowercase();
    let zen = matches!(
        id.as_str(),
        "opencode" | "opencode-zen" | "zen" | "opencode-go" | "go"
    );
    if !zen {
        return false;
    }
    let m = model.to_ascii_lowercase();
    m.starts_with("muse-spark") || m.starts_with("gpt-5") || m.starts_with("grok-4")
}

pub fn provider_for_model(
    provider_id: &str,
    base_url: &str,
    key: Option<String>,
    model: &str,
) -> Result<Box<dyn Provider>, ProviderError> {
    if needs_responses_api(provider_id, model) {
        let base = if base_url.trim().is_empty() {
            openai::OpenAiCompat::preset_base(&provider_id.to_ascii_lowercase()).unwrap_or("")
        } else {
            base_url
        };
        if !base.is_empty() {
            return Ok(Box::new(responses::Responses::new(base, key)));
        }
    }
    provider_for(provider_id, base_url, key)
}

/// True when a failure is the prompt exceeding the model's context window.
///
/// Providers report this as a 400 with assorted wording, so match on the
/// message rather than a status code. Recovering beats surfacing it: the
/// caller can compact and try again.
pub fn is_context_overflow_error(e: &ProviderError) -> bool {
    let text = match e {
        ProviderError::Protocol(m) | ProviderError::Transport(m) => m.clone(),
        ProviderError::Auth(m) => m.clone(),
        ProviderError::Unsupported(m) => m.clone(),
        _ => return false,
    };
    let m = text.to_ascii_lowercase();
    m.contains("context length")
        || m.contains("context_length")
        || m.contains("maximum context")
        || m.contains("context window")
        || m.contains("too many tokens")
        || m.contains("prompt is too long")
        || m.contains("reduce the length")
}

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

/// Build the HTTP client every provider shares.
///
/// `read_timeout` is an **idle** timeout: it fires only when no bytes arrive for
/// that long. A total request timeout must never be used here — a
/// reasoning-heavy turn can stream for minutes, and a total timeout aborts it
/// mid-generation, which the user sees as a turn that hangs and then dies.
pub(crate) fn http_client(read_timeout: Option<std::time::Duration>) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        // Nagle would coalesce small SSE frames, adding latency per token.
        .tcp_nodelay(true)
        // Reuse connections: otherwise every turn pays a fresh TLS handshake.
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .connect_timeout(std::time::Duration::from_secs(10));
    if let Some(t) = read_timeout {
        b = b.read_timeout(t);
    }
    b.build().expect("reqwest client")
}

pub(crate) fn sse_data_lines(buf: &mut Vec<u8>, chunk: &[u8]) -> Vec<String> {
    buf.extend_from_slice(chunk);
    let mut out = Vec::new();
    // Scan with a cursor and drain once at the end. Draining per line shifted
    // the whole remaining buffer every time, which is quadratic — noticeable
    // when a large tool argument arrives as one long line.
    let mut start = 0usize;
    while let Some(rel) = buf[start..].iter().position(|&b| b == b'\n') {
        let end = start + rel;
        let line = String::from_utf8_lossy(&buf[start..end]);
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if !data.is_empty() {
                out.push(data.to_string());
            }
        }
        start = end + 1;
    }
    if start > 0 {
        buf.drain(..start);
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
    fn responses_api_routing_by_model_family() {
        assert!(needs_responses_api("opencode-go", "muse-spark-1.3-contributor"));
        assert!(needs_responses_api("opencode", "gpt-5.6-luna"));
        assert!(needs_responses_api("opencode-go", "grok-4.6"));
        assert!(!needs_responses_api("opencode-go", "deepseek-v4.1-flash"));
        assert!(!needs_responses_api("opencode-go", "glm-5.2"));
        assert!(!needs_responses_api("openai", "muse-spark-1.3-contributor"));
    }

    #[test]
    fn provider_for_model_picks_the_right_adapter() {
        let p = provider_for_model("opencode-go", "", None, "muse-spark-1.3-contributor").unwrap();
        assert_eq!(p.id(), "openai-responses");
        let p = provider_for_model("opencode-go", "", None, "deepseek-v4.1-flash").unwrap();
        assert_eq!(p.id(), "openai-compat");
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
        assert!(sse_data_lines(&mut buf, b"\n: ping\n\n").is_empty());
    }
}
