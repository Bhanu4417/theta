use std::collections::BTreeMap;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::ai::{
    sse_data_lines, AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent, Role,
    ToolCall,
};
use crate::providers::ProviderError;

pub struct OpenAiCompat {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
    timeout: Option<std::time::Duration>,
}

impl OpenAiCompat {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let client = crate::ai::http_client(None);
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let mut headers = Vec::new();
        if base_url.contains("opencode.ai/zen") {
            headers.push(("x-opencode-session".to_string(), session_id()));
        }
        Self { base_url, api_key, client, headers, timeout: None }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn preset(provider: &str, api_key: Option<String>) -> Option<Self> {
        Self::preset_base(provider).map(|base| Self::new(base, api_key))
    }

    pub fn preset_base(provider: &str) -> Option<&'static str> {
        Some(match provider {
            "opencode" | "opencode-zen" | "zen" => "https://opencode.ai/zen/v1",
            "opencode-go" | "go" => "https://opencode.ai/zen/go/v1",
            "openai" => "https://api.openai.com/v1",
            "xai" | "grok" => "https://api.x.ai/v1",
            "groq" => "https://api.groq.com/openai/v1",
            "deepseek" => "https://api.deepseek.com/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "together" => "https://api.together.xyz/v1",
            "fireworks" => "https://api.fireworks.ai/inference/v1",
            "mistral" => "https://api.mistral.ai/v1",
            "cerebras" => "https://api.cerebras.ai/v1",
            "perplexity" => "https://api.perplexity.ai",
            "ollama" => "http://localhost:11434/v1",
            _ => return None,
        })
    }

    pub async fn fetch_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let mut req = self.client.get(&url).timeout(std::time::Duration::from_secs(20));
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(net)?;
        let status = resp.status();
        let text = resp.text().await.map_err(net)?;
        if !status.is_success() {
            return Err(ProviderError::Protocol(format!("HTTP {status}: {}", text.trim())));
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| ProviderError::Protocol(e.to_string()))?;
        let arr = v
            .get("data")
            .or_else(|| v.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
            .collect())
    }
}

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
    // Reasoning effort was previously sent only on the `/responses` path, so a
    // configured effort was silently ignored for every model reaching
    // `/chat/completions` — which run at their own (usually higher) default and
    // take far longer to answer.
    if let Some(e) = &req.reasoning_effort {
        if !e.trim().is_empty() {
            body["reasoning_effort"] = json!(e);
        }
    }
    body
}

#[derive(Default)]
pub(crate) struct StreamAccum {
    text: String,
    calls: BTreeMap<u64, ToolCall>,
    order: Vec<u64>,
}

impl StreamAccum {
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

    fn set_timeout(&mut self, secs: u64) {
        self.timeout = (secs > 0).then(|| std::time::Duration::from_secs(secs));
        // Rebuild so the timeout applies as an idle read timeout.
        self.client = crate::ai::http_client(self.timeout);
    }

    fn list_models<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>, ProviderError>> + Send + 'a>,
    > {
        Box::pin(self.fetch_models())
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
            for call in &turn.tool_calls {
                on_event(ProviderEvent::ToolCall(call.clone()));
            }
            Ok(turn)
        })
    }
}

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

    #[test]
    fn reasoning_effort_reaches_the_chat_completions_body() {
        // This was the cause of a real latency bug: the effort was honoured on
        // the /responses path only, so every /chat/completions model ran at its
        // own higher default.
        let mut req = ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("hi")],
            tools: vec![],
            temperature: None,
            max_tokens: None,
            reasoning_effort: Some("low".into()),
        };
        let body = build_body(&req, true);
        assert_eq!(body["reasoning_effort"], json!("low"));

        // Unset or blank effort must not be sent, so the provider default
        // applies rather than an empty string being rejected.
        req.reasoning_effort = None;
        assert!(build_body(&req, true).get("reasoning_effort").is_none());
        req.reasoning_effort = Some("  ".into());
        assert!(build_body(&req, true).get("reasoning_effort").is_none());
    }
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
            ..Default::default()
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
    fn set_timeout_is_applied_and_zero_disables() {
        let mut p = OpenAiCompat::new("https://example.test/v1", None);
        p.set_timeout(42);
        assert_eq!(p.timeout, Some(std::time::Duration::from_secs(42)));
        p.set_timeout(0);
        assert!(p.timeout.is_none());
    }

    #[test]
    fn presets_cover_common_gateways() {
        for p in [
            "opencode",
            "opencode-go",
            "openai",
            "xai",
            "grok",
            "groq",
            "deepseek",
            "openrouter",
            "ollama",
        ] {
            assert!(OpenAiCompat::preset(p, None).is_some(), "{p} preset");
        }
        assert!(OpenAiCompat::preset("nope", None).is_none());
    }

    #[test]
    fn opencode_presets_use_zen_endpoints_and_session_header() {
        let zen = OpenAiCompat::preset("opencode", None).unwrap();
        assert_eq!(zen.base_url, "https://opencode.ai/zen/v1");
        let go = OpenAiCompat::preset("opencode-go", None).unwrap();
        assert_eq!(go.base_url, "https://opencode.ai/zen/go/v1");
        assert!(go.headers.iter().any(|(k, _)| k == "x-opencode-session"));
    }
}
