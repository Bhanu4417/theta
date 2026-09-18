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
    /// Extra data for the UI attached to a tool result, chiefly the diff an edit
    /// produced. Persisted so a restored session can still show what changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_metadata: Option<serde_json::Value>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, text: text.into(), tool_calls: Vec::new(), tool_call_id: None, tokens: None, images: Vec::new(), cost: None, tool_metadata: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, text: text.into(), tool_calls: Vec::new(), tool_call_id: None, tokens: None, images: Vec::new(), cost: None, tool_metadata: None }
    }
    pub fn user_with_images(text: impl Into<String>, images: Vec<ImagePart>) -> Self {
        let mut m = Self::user(text);
        m.images = images;
        m
    }
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, text: text.into(), tool_calls, tool_call_id: None, tokens: None, images: Vec::new(), cost: None, tool_metadata: None }
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
            tool_metadata: None,
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

/// The conventional environment variable for a provider's key. Used to make
/// "no credentials" errors concrete.
pub fn env_hint(provider_id: &str) -> String {
    format!("{}_API_KEY", provider_id.to_ascii_uppercase().replace('-', "_"))
}

/// True when this provider id maps to a hosted endpoint we ship for, and so
/// needs credentials unless a custom `base_url` is set.
///
/// Must cover the native adapters as well as the OpenAI-compatible presets:
/// checking only the preset table would let an Anthropic or Gemini user with no
/// key reach the network and be shown raw JSON again.
pub fn is_hosted_preset(provider_id: &str) -> bool {
    let id = provider_id.to_ascii_lowercase();
    matches!(id.as_str(), "anthropic" | "claude" | "google" | "gemini")
        || openai::OpenAiCompat::preset_base(&id).is_some()
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

/// Whether an HTTP status is worth retrying.
///
/// `408 Request Timeout`, `425 Too Early`, `429 Too Many Requests` and every
/// `5xx`, which is the rule the reference implementation uses. A busy model
/// behind a shared gateway mostly returns 500s and 429s, and those are the
/// failures a retry actually recovers from.
/// Parse a `Retry-After` header into milliseconds.
///
/// The header is either a number of seconds or an HTTP date. Both forms are
/// handled, because a gateway that sends one form and not the other would
/// otherwise lose the server's own estimate of when it will be ready.
pub fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = raw.parse::<f64>() {
        if secs.is_finite() && secs >= 0.0 {
            return Some((secs * 1000.0) as u64);
        }
        return None;
    }
    // HTTP date form: seconds from now, floored at zero for a past date.
    let when = httpdate_secs(raw)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(((when - now).max(0) as u64).saturating_mul(1000))
}

/// Seconds since the epoch for an RFC 7231 date such as
/// `Wed, 21 Oct 2015 07:28:00 GMT`.
///
/// A small parser rather than a date dependency, for a header that is almost
/// always sent as plain seconds anyway.
fn httpdate_secs(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches(" GMT").trim();
    // "Wed, 21 Oct 2015 07:28:00" -> drop the weekday.
    let s = s.split_once(", ").map(|(_, rest)| rest).unwrap_or(s);
    let mut parts = s.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let (hh, mm, ss) = {
        let mut t = parts.next()?.split(':');
        (
            t.next()?.parse::<i64>().ok()?,
            t.next()?.parse::<i64>().ok()?,
            t.next().unwrap_or("0").parse::<i64>().ok()?,
        )
    };

    // Days since the epoch (civil-from-days).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

fn month_number(name: &str) -> Option<i64> {
    Some(match name {
        "Jan" => 1, "Feb" => 2, "Mar" => 3, "Apr" => 4, "May" => 5, "Jun" => 6,
        "Jul" => 7, "Aug" => 8, "Sep" => 9, "Oct" => 10, "Nov" => 11, "Dec" => 12,
        _ => return None,
    })
}

pub fn is_retryable_status(code: u16) -> bool {
    matches!(code, 408 | 425 | 429) || (500..=599).contains(&code)
}

pub fn is_retryable_provider_error(e: &ProviderError) -> bool {
    match e {
        // A dropped connection or a resolution failure: the same request will
        // usually succeed on a second attempt.
        ProviderError::Transport(_) | ProviderError::Unavailable(_) => true,
        ProviderError::Status { code, .. } => is_retryable_status(*code),
        ProviderError::Protocol(m) => {
            let m = m.to_ascii_lowercase();
            m.contains("overloaded") || m.contains("timeout") || m.contains("timed out")
        }
        _ => false,
    }
}

/// How long to wait before the next attempt.
///
/// A server-supplied `Retry-After` wins: it knows when it will be ready, and
/// guessing shorter just burns an attempt. Otherwise exponential backoff,
/// capped so a long outage does not park the session for minutes.
pub fn retry_delay(
    e: &ProviderError,
    attempt: u32,
    base_ms: u64,
    max_ms: u64,
) -> std::time::Duration {
    if let ProviderError::Status { retry_after_ms: Some(ms), .. } = e {
        return std::time::Duration::from_millis((*ms).min(max_ms));
    }
    let exp = base_ms.saturating_mul(1u64 << (attempt.saturating_sub(1)).min(10));
    std::time::Duration::from_millis(exp.min(max_ms))
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

/// Identify this client to a provider, the way the OpenCode Go docs ask:
/// clients should name themselves rather than presenting as a generic HTTP
/// library, which is what `reqwest`'s default user agent does.
///
/// `x-opencode-session` is only routed efficiently when the value is stable, so
/// this is one identity for the process rather than a fresh value per request.
pub(crate) fn user_agent() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let release = kernel_release().unwrap_or_else(|| os.to_string());
    format!("theta/{} ({release}; {arch})", env!("CARGO_PKG_VERSION"))
}

/// The kernel release, so the identity carries a version like a real client
/// would. Linux exposes it as text; other platforms fall back to the OS name.
fn kernel_release() -> Option<String> {
    let text = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// A stable identifier for this client's traffic.
///
/// The Go docs require the session header value to be stable; generating a new
/// one per provider build (and so per model switch) undermines the routing it
/// exists for. Generated once for the process.
pub(crate) fn session_identity() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // A short, opaque token: the value is only an identity, not a secret.
        format!("theta-{nanos:x}-{:x}", std::process::id())
    })
}

/// Build the HTTP client every provider shares.
///
/// `read_timeout` is an **idle** timeout: it fires only when no bytes arrive for
/// that long. A total request timeout must never be used here — a
/// reasoning-heavy turn can stream for minutes, and a total timeout aborts it
/// mid-generation, which the user sees as a turn that hangs and then dies.
pub(crate) fn http_client(read_timeout: Option<std::time::Duration>) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        // Present as Theta, not as the HTTP library it happens to use.
        .user_agent(user_agent())
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
        // Status codes now arrive as a Status, not buried in a Protocol string,
        // so the classification sees the code rather than guessing at text.
        assert!(is_retryable_provider_error(&ProviderError::Status {
            code: 503,
            message: "Service Unavailable".into(),
            retry_after_ms: None,
        }));
        assert!(!is_retryable_provider_error(&ProviderError::Protocol("bad json".into())));
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

    #[test]
    fn env_hint_follows_the_provider_id() {
        assert_eq!(env_hint("openai"), "OPENAI_API_KEY");
        assert_eq!(env_hint("opencode-go"), "OPENCODE_GO_API_KEY");
    }

    #[test]
    fn hosted_presets_are_recognized_but_unknown_ids_are_not() {
        assert!(is_hosted_preset("openai"));
        assert!(is_hosted_preset("opencode-go"));
        assert!(is_hosted_preset("OPENCODE-GO"), "case-insensitive");
        // Native adapters have no OpenAI preset, so they need explicit coverage.
        for native in ["anthropic", "claude", "google", "gemini"] {
            assert!(is_hosted_preset(native), "{native} needs credentials too");
        }
        // An arbitrary id is treated as a custom endpoint, not a hosted preset.
        assert!(!is_hosted_preset("my-self-hosted-thing"));
    }

    #[test]
    fn a_custom_base_url_may_omit_the_key() {
        // A local or self-hosted endpoint often needs no credentials, so the
        // check must not fire when base_url is set.
        assert!(provider_for_model("openai", "http://localhost:11434/v1", None, "llama3").is_ok());
    }

    #[test]
    fn a_supplied_key_is_accepted() {
        assert!(provider_for_model("openai", "", Some("sk-test".into()), "gpt-4o").is_ok());
    }

    #[test]
    fn every_5xx_and_the_usual_4xx_are_retryable() {
        // The rule the reference implementation uses. Substring-matching the
        // message before this recognized only 500/502/503/504 and missed the
        // rest, so a 501 or 507 gave up immediately.
        for code in [408, 425, 429] {
            assert!(is_retryable_status(code), "{code} should retry");
        }
        for code in 500..=599 {
            assert!(is_retryable_status(code), "{code} should retry");
        }
        for code in [400, 401, 403, 404, 422, 451] {
            assert!(!is_retryable_status(code), "{code} must not retry");
        }
    }

    use crate::agent::short_reason;

    #[test]
    fn a_status_error_is_classified_by_its_code() {
        let e = ProviderError::Status {
            code: 500,
            message: "Internal server error".into(),
            retry_after_ms: None,
        };
        assert!(is_retryable_provider_error(&e));
        assert_eq!(short_reason(&e), "provider error (500)");

        let e = ProviderError::Status { code: 429, message: String::new(), retry_after_ms: None };
        assert_eq!(short_reason(&e), "rate limited (429)");

        let e = ProviderError::Status { code: 401, message: String::new(), retry_after_ms: None };
        assert!(!is_retryable_provider_error(&e), "auth is not a retryable failure");
    }

    #[test]
    fn a_failed_connection_is_retryable_and_named_clearly() {
        // "fail to connect" is what the user actually sees, so the label says so.
        for (msg, want) in [
            ("error sending request: connection refused", "could not connect"),
            ("operation timed out", "network timeout"),
            ("something else entirely", "network error"),
        ] {
            let e = ProviderError::Transport(msg.into());
            assert!(is_retryable_provider_error(&e), "{msg} must retry");
            assert_eq!(short_reason(&e), want, "for {msg}");
        }
    }

    #[test]
    fn retry_delay_prefers_retry_after_then_backs_off_with_a_cap() {
        // A server that says when to come back is believed.
        let e = ProviderError::Status {
            code: 429,
            message: String::new(),
            retry_after_ms: Some(7_000),
        };
        assert_eq!(retry_delay(&e, 1, 500, 30_000).as_millis(), 7_000);
        // But never beyond the cap.
        let e = ProviderError::Status {
            code: 429,
            message: String::new(),
            retry_after_ms: Some(999_000),
        };
        assert_eq!(retry_delay(&e, 1, 500, 30_000).as_millis(), 30_000);

        // Without one, exponential: 500, 1000, 2000, ...
        let e = ProviderError::Transport("boom".into());
        assert_eq!(retry_delay(&e, 1, 500, 30_000).as_millis(), 500);
        assert_eq!(retry_delay(&e, 2, 500, 30_000).as_millis(), 1_000);
        assert_eq!(retry_delay(&e, 3, 500, 30_000).as_millis(), 2_000);
        // And capped rather than growing without bound.
        assert_eq!(retry_delay(&e, 20, 500, 30_000).as_millis(), 30_000);
    }

    #[test]
    fn retry_after_parses_seconds_and_http_dates() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(retry_after_ms(&h), Some(12_000));

        // A past date is treated as "now", never a negative wait.
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after_ms(&h), Some(0));

        h.remove(RETRY_AFTER);
        assert_eq!(retry_after_ms(&h), None);
    }

    #[test]
    fn http_date_parsing_matches_known_instants() {
        // 1970-01-01 00:00:00 and a known modern date.
        assert_eq!(httpdate_secs("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(httpdate_secs("Thu, 01 Jan 1970 00:00:00"), Some(0));
        // 2015-10-21T07:28:00Z
        assert_eq!(httpdate_secs("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
    }


    #[test]
    fn the_client_identifies_itself_as_theta() {
        // The OpenCode Go docs ask clients to name themselves rather than
        // present as a generic HTTP library, which is what reqwest's default
        // user agent does.
        let ua = user_agent();
        assert!(ua.starts_with("theta/"), "{ua}");
        assert!(ua.contains(env!("CARGO_PKG_VERSION")), "carries the version: {ua}");
        assert!(ua.contains(std::env::consts::ARCH), "carries the arch: {ua}");
        assert!(
            !ua.to_ascii_lowercase().contains("reqwest"),
            "must not present as the HTTP library: {ua}"
        );
    }

    #[test]
    fn the_session_identity_is_stable_for_the_process() {
        // It exists so traffic can be routed consistently, so a fresh value per
        // request (or per model switch) defeats the purpose.
        let a = session_identity();
        let b = session_identity();
        assert_eq!(a, b, "the identity must not change between calls");
        assert!(a.starts_with("theta-"), "{a}");
        assert!(a.len() > 12, "distinctive enough to route on: {a}");
    }

}
