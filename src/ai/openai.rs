//! OpenAI-compatible provider (`/chat/completions`).
//!
//! This one implementation covers OpenAI, xAI/Grok, Groq, OpenRouter,
//! DeepSeek, Together, Fireworks, Cerebras, Ollama, LM Studio, vLLM and any
//! other gateway that speaks the OpenAI wire format — only `base_url` (and
//! optionally an API key) differ.

use std::collections::BTreeMap;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::ai::{
    sse_data_lines, AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent, Role,
    ToolCall,
};
use crate::providers::ProviderError;

pub struct OpenAiCompat {
    /// e.g. `https://api.openai.com/v1`, `https://api.x.ai/v1`.
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    /// Extra request headers (e.g. a gateway session id).
    headers: Vec<(String, String)>,
}

impl OpenAiCompat {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let mut headers = Vec::new();
        // The OpenCode "zen" gateway routes per session and requires this.
        if base_url.contains("opencode.ai/zen") {
            headers.push(("x-opencode-session".to_string(), session_id()));
        }
        Self { base_url, api_key, client, headers }
    }

    /// Add/replace an extra request header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Standard preset for a well-known provider id, or `None` if unknown.
    pub fn preset(provider: &str, api_key: Option<String>) -> Option<Self> {
        let base = match provider {
            "openai" => "https://api.openai.com/v1",
            "xai" | "grok" => "https://api.x.ai/v1",
            "groq" => "https://api.groq.com/openai/v1",
            "deepseek" => "https://api.deepseek.com/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "together" => "https://api.together.xyz/v1",
            "fireworks" => "https://api.fireworks.ai/inference/v1",
            "ollama" => "http://localhost:11434/v1",
            _ => return None,
        };
        Some(Self::new(base, api_key))
    }
}

/// Build the JSON request body. Pure so it can be unit-tested.
pub(crate) fn build_body(req: &ChatRequest, stream: bool) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut obj = json!({ "role": role, "content": m.text });
            if !m.images.is_empty() {
                let mut parts = vec![json!({ "type": "text", "text": m.text })];
                for img in &m.images {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", img.mime, img.data) }
                    }));
                }
                obj["content"] = Value::Array(parts);
            }
            if !m.tool_calls.is_empty() {
                obj["tool_calls"] = Value::Array(
                    m.tool_calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments }
                            })
                        })
                        .collect(),
                );
            }
            if let Some(id) = &m.tool_call_id {
                obj["tool_call_id"] = json!(id);
            }
            obj
        })
        .collect();

    let mut body = json!({ "model": req.model, "messages": messages, "stream": stream });
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect(),
        );
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        body["max_tokens"] = json!(m);
    }
    body
}

/// Accumulates streamed deltas into a final assistant turn. Tool-call
/// arguments arrive in fragments keyed by `index`.
#[derive(Default)]
pub(crate) struct StreamAccum {
    text: String,
    calls: BTreeMap<u64, ToolCall>,
    order: Vec<u64>,
}

impl StreamAccum {
    /// Translate one SSE `data:` payload into neutral events and fold it into
    /// the accumulator. Returns the events produced (may be empty).
    pub(crate) fn ingest(&mut self, data: &str, out: &mut Vec<ProviderEvent>) {
        if data == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            let g = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            let input = g("prompt_tokens");
            let output = g("completion_tokens");
            if input > 0 || output > 0 {
                out.push(ProviderEvent::Usage { input, output });
            }
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            let f = match reason {
                "stop" => FinishReason::Stop,
                "tool_calls" => FinishReason::ToolCalls,
                "length" => FinishReason::Length,
                other => FinishReason::Other(other.to_string()),
            };
            out.push(ProviderEvent::Done(f));
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };
        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                self.text.push_str(text);
                out.push(ProviderEvent::TextDelta(text.to_string()));
            }
        }
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
            if !reasoning.is_empty() {
                out.push(ProviderEvent::ReasoningDelta(reasoning.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for c in calls {
                let index = c.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                let entry = self.calls.entry(index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if !self.order.contains(&index) {
                    self.order.push(index);
                }
                if let Some(id) = c.get("id").and_then(|i| i.as_str()) {
                    if !id.is_empty() {
                        entry.id = id.to_string();
                    }
                }
                if let Some(f) = c.get("function") {
                    if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                        entry.name.push_str(name);
                    }
                    if let Some(args) = f.get("arguments").and_then(|a| a.as_str()) {
                        entry.arguments.push_str(args);
                    }
                }
            }
        }
    }

    fn into_turn(self) -> AssistantTurn {
        let tool_calls: Vec<ToolCall> = self
            .order
            .iter()
            .filter_map(|i| self.calls.get(i).cloned())
            .collect();
        AssistantTurn { text: self.text, tool_calls, finish: None }
    }
}

impl Provider for OpenAiCompat {
    fn id(&self) -> &'static str {
        "openai-compat"
    }

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let body = build_body(&request, true);
            let mut rb = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .header("Accept", "text/event-stream")
                .json(&body);
            if let Some(key) = &self.api_key {
                rb = rb.bearer_auth(key);
            }
            for (k, v) in &self.headers {
                rb = rb.header(k.as_str(), v.as_str());
            }
            let resp = rb.send().await.map_err(net)?;
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(match status.as_u16() {
                    401 | 403 => ProviderError::Auth(text),
                    404 => ProviderError::Unsupported(text),
                    _ => ProviderError::Transport(format!("{status}: {text}")),
                });
            }
            let mut stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut acc = StreamAccum::default();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(net)?;
                for data in sse_data_lines(&mut buf, &chunk) {
                    let mut events = Vec::new();
                    acc.ingest(&data, &mut events);
                    for ev in events {
                        on_event(ev);
                    }
                }
            }
            let turn = acc.into_turn();
            // Announce assembled tool calls once, after the stream ends.
            for call in &turn.tool_calls {
                on_event(ProviderEvent::ToolCall(call.clone()));
            }
            Ok(turn)
        })
    }
}

/// A process-unique session id for gateways that require one.
pub(crate) fn session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("theta-{t}-{n}")
}

fn net(e: reqwest::Error) -> ProviderError {
    ProviderError::Transport(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ChatMessage, ToolSpec, ProviderEvent};

    #[test]
    fn body_includes_image_parts_for_vision() {
        let req = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::user_with_images(
                "what is this?",
                vec![crate::ai::ImagePart { mime: "image/png".into(), data: "AAAA".into() }],
            )],
            ..Default::default()
        };
        let b = build_body(&req, true);
        let content = &b["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    fn body_serializes_messages_tools_and_tool_results() {
        let req = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![
                ChatMessage::system("be brief"),
                ChatMessage::user("list files"),
                ChatMessage::assistant(
                    "",
                    vec![ToolCall { id: "c1".into(), name: "bash".into(), arguments: "{\"command\":\"ls\"}".into() }],
                ),
                ChatMessage::tool_result("c1", "a.rs\nb.rs"),
            ],
            tools: vec![ToolSpec {
                name: "bash".into(),
                description: "run".into(),
                parameters: json!({"type": "object"}),
            }],
            temperature: Some(0.2),
            max_tokens: Some(1024),
        };
        let b = build_body(&req, true);
        assert_eq!(b["stream"], true);
        assert_eq!(b["model"], "gpt-4o");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][2]["tool_calls"][0]["id"], "c1");
        assert_eq!(b["messages"][3]["tool_call_id"], "c1");
        assert_eq!(b["tools"][0]["function"]["name"], "bash");
        assert!((b["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-5);
    }

    #[test]
    fn streamed_text_is_accumulated_and_emitted() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#, &mut ev);
        acc.ingest(r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#, &mut ev);
        assert_eq!(acc.text, "Hello");
        assert!(ev.contains(&ProviderEvent::TextDelta("Hel".into())));
        assert!(ev.contains(&ProviderEvent::Done(FinishReason::Stop)));
    }

    #[test]
    fn streamed_tool_calls_are_fragmented_then_assembled() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":"{\"comm"}}]}}]}"#,
            &mut ev,
        );
        acc.ingest(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            &mut ev,
        );
        let turn = acc.into_turn();
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, "call_1");
        assert_eq!(turn.tool_calls[0].name, "bash");
        assert_eq!(turn.tool_calls[0].arguments, r#"{"command":"ls"}"#);
        assert!(ev.iter().any(|e| matches!(e, ProviderEvent::Done(FinishReason::ToolCalls))));
    }

    #[test]
    fn usage_is_reported() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(
            r#"{"usage":{"prompt_tokens":10,"completion_tokens":5},"choices":[]}"#,
            &mut ev,
        );
        assert!(ev.contains(&ProviderEvent::Usage { input: 10, output: 5 }));
    }

    #[test]
    fn presets_cover_common_gateways() {
        for p in ["openai", "xai", "grok", "groq", "deepseek", "openrouter", "ollama"] {
            assert!(OpenAiCompat::preset(p, None).is_some(), "{p} preset");
        }
        assert!(OpenAiCompat::preset("nope", None).is_none());
    }
}
