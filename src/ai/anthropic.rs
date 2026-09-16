use futures::StreamExt;
use serde_json::{json, Value};

use crate::ai::{
    sse_data_lines, AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent,
    Role, ToolCall,
};
use crate::providers::ProviderError;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;

pub struct Anthropic {
    api_key: Option<String>,
    base_url: String,
    client: reqwest::Client,
    timeout: Option<std::time::Duration>,
}

impl Anthropic {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_base("https://api.anthropic.com", api_key)
    }

    pub fn with_base(base: impl Into<String>, api_key: Option<String>) -> Self {
        let client = crate::ai::http_client(None);
        Self {
            api_key,
            base_url: base.into().trim_end_matches('/').to_string(),
            client,
            timeout: None,
        }
    }

    pub async fn fetch_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = format!("{}/v1/models", self.base_url);
        let mut req = self
            .client
            .get(&url)
            .header("anthropic-version", API_VERSION)
            .timeout(std::time::Duration::from_secs(20));
        if let Some(k) = &self.api_key {
            req = req.header("x-api-key", k);
        }
        let resp = req.send().await.map_err(net)?;
        let status = resp.status();
        let text = resp.text().await.map_err(net)?;
        if !status.is_success() {
            return Err(ProviderError::Protocol(format!("HTTP {status}: {}", text.trim())));
        }
        let v: Value =
            serde_json::from_str(&text).map_err(|e| ProviderError::Protocol(e.to_string()))?;
        Ok(v.get("data")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }
}

pub fn build_body(req: &ChatRequest, stream: bool) -> Value {
    let mut system: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => system.push(m.text.clone()),
            Role::User => {
                let mut content = vec![json!({ "type": "text", "text": m.text })];
                for img in &m.images {
                    content.push(json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": img.mime, "data": img.data }
                    }));
                }
                messages.push(json!({ "role": "user", "content": content }));
            }
            Role::Assistant => {
                let mut content: Vec<Value> = Vec::new();
                if !m.text.is_empty() {
                    content.push(json!({ "type": "text", "text": m.text }));
                }
                for c in &m.tool_calls {
                    let input: Value = serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                    content.push(json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": input
                    }));
                }
                if content.is_empty() {
                    content.push(json!({ "type": "text", "text": "" }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            Role::Tool => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.text
                }]
            })),
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": stream,
    });
    if !system.is_empty() {
        // Sent as content blocks with a cache breakpoint: the system prompt and
        // tools are identical on every turn of a session, so caching them turns
        // a full prefill into a cache read.
        body["system"] = json!([{
            "type": "text",
            "text": system.join("\n\n"),
            "cache_control": { "type": "ephemeral" }
        }]);
    }
    if !req.tools.is_empty() {
        let last = req.tools.len() - 1;
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mut v = json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    });
                    // A breakpoint on the final tool caches the whole block.
                    if i == last {
                        v["cache_control"] = json!({ "type": "ephemeral" });
                    }
                    v
                })
                .collect(),
        );
    }
    if let Some(temp) = req.temperature {
        body["temperature"] = json!(temp);
    }
    // Extended thinking, sized from the configured effort. A disabled budget is
    // the fast path, so an unset effort sends nothing and leaves the model's
    // default alone.
    if let Some(budget) = thinking_budget(req.reasoning_effort.as_deref()) {
        // The API requires the reply budget to exceed the thinking budget.
        if req.max_tokens.map(|m| m > budget).unwrap_or(true) {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    body
}

/// Map a reasoning effort onto Anthropic's thinking budget, in tokens.
fn thinking_budget(effort: Option<&str>) -> Option<u32> {
    match effort.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
        Some("minimal") => Some(1024),
        Some("low") => Some(2048),
        Some("medium") => Some(8192),
        Some("high") => Some(16384),
        _ => None,
    }
}

#[derive(Default)]
pub struct StreamState {
    text: String,
    calls: Vec<ToolCall>,
    active: Option<(u64, usize)>,
    input_tokens: u64,
    output_tokens: u64,
    finish: Option<FinishReason>,
}

impl StreamState {
    pub fn ingest(&mut self, data: &str, out: &mut Vec<ProviderEvent>) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match typ {
            "message_start" => {
                if let Some(u) = v
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|x| x.as_u64())
                {
                    self.input_tokens = u;
                }
            }
            "content_block_start" => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                if let Some(block) = v.get("content_block") {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let call = ToolCall {
                            id: block.get("id").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
                            name: block.get("name").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
                            arguments: String::new(),
                        };
                        self.calls.push(call);
                        self.active = Some((index, self.calls.len() - 1));
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = v.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                                if !t.is_empty() {
                                    self.text.push_str(t);
                                    out.push(ProviderEvent::TextDelta(t.to_string()));
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
                                if !t.is_empty() {
                                    out.push(ProviderEvent::ReasoningDelta(t.to_string()));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let (Some((_, pos)), Some(partial)) = (
                                self.active,
                                delta.get("partial_json").and_then(|p| p.as_str()),
                            ) {
                                if let Some(call) = self.calls.get_mut(pos) {
                                    call.arguments.push_str(partial);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(u) = v.get("usage").and_then(|u| u.get("output_tokens")).and_then(|x| x.as_u64()) {
                    self.output_tokens = u;
                }
                if let Some(reason) = v.get("delta").and_then(|d| d.get("stop_reason")).and_then(|r| r.as_str()) {
                    let f = match reason {
                        "end_turn" | "stop_sequence" => FinishReason::Stop,
                        "tool_use" => FinishReason::ToolCalls,
                        "max_tokens" => FinishReason::Length,
                        other => FinishReason::Other(other.to_string()),
                    };
                    self.finish = Some(f.clone());
                    out.push(ProviderEvent::Done(f));
                }
            }
            _ => {}
        }
    }

    fn into_turn(self) -> AssistantTurn {
        let calls = self.calls;
        AssistantTurn { text: self.text, tool_calls: calls, finish: self.finish }
    }

    pub fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }
}

impl Provider for Anthropic {
    fn id(&self) -> &'static str {
        "anthropic"
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
                .post(format!("{}/v1/messages", self.base_url))
                .header("anthropic-version", API_VERSION)
                .header("Accept", "text/event-stream")
                .json(&body);
            if let Some(key) = &self.api_key {
                rb = rb.header("x-api-key", key);
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
            let mut state = StreamState::default();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(net)?;
                for data in sse_data_lines(&mut buf, &chunk) {
                    let mut events = Vec::new();
                    state.ingest(&data, &mut events);
                    for ev in events {
                        on_event(ev);
                    }
                }
            }
            let (input, output) = state.usage();
            if input > 0 || output > 0 {
                on_event(ProviderEvent::Usage { input, output });
            }
            let turn = state.into_turn();
            for call in &turn.tool_calls {
                on_event(ProviderEvent::ToolCall(call.clone()));
            }
            Ok(turn)
        })
    }
}

fn net(e: reqwest::Error) -> ProviderError {
    ProviderError::Transport(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{ChatMessage, ToolSpec};

    #[test]
    fn body_has_system_tools_and_tool_results() {
        let req = ChatRequest {
            model: "claude-3-7-sonnet-20250219".into(),
            messages: vec![
                ChatMessage::system("be brief"),
                ChatMessage::user("hi"),
                ChatMessage::assistant("", vec![ToolCall { id: "t1".into(), name: "read".into(), arguments: "{\"path\":\"/a\"}".into() }]),
                ChatMessage::tool_result("t1", "data"),
            ],
            tools: vec![ToolSpec { name: "read".into(), description: "r".into(), parameters: json!({"type":"object"}) }],
            temperature: Some(0.3),
            max_tokens: Some(1000),
            ..Default::default()
        };
        let b = build_body(&req, true);
        // The system prompt is a content block carrying a cache breakpoint, so
        // the unchanged prefix is read from cache instead of re-prefilled.
        assert_eq!(b["system"][0]["type"], "text");
        assert_eq!(b["system"][0]["text"], "be brief");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(b["messages"][1]["content"][0]["input"]["path"], "/a");
        assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        // The breakpoint goes on the last tool so the whole block is cached.
        assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["max_tokens"], 1000);
    }

    #[test]
    fn caching_is_added_only_where_anthropic_allows_it() {
        let mut req = ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::system("s"), ChatMessage::user("hi")],
            tools: vec![
                ToolSpec { name: "a".into(), description: "a".into(), parameters: json!({}) },
                ToolSpec { name: "b".into(), description: "b".into(), parameters: json!({}) },
            ],
            ..Default::default()
        };
        let b = build_body(&req, true);
        // Exactly one tool breakpoint, on the last definition.
        assert!(b["tools"][0].get("cache_control").is_none());
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");

        // No system prompt means no system block at all, not an empty one.
        req.messages = vec![ChatMessage::user("hi")];
        let b = build_body(&req, true);
        assert!(b.get("system").is_none());
    }

    #[test]
    fn stream_accumulates_text_and_tool_use() {
        let mut s = StreamState::default();
        let mut out = Vec::new();
        s.ingest(r#"{"type":"message_start","message":{"usage":{"input_tokens":12}}}"#, &mut out);
        s.ingest(r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}"#, &mut out);
        s.ingest(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"bash"}}"#, &mut out);
        s.ingest(r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}"#, &mut out);
        s.ingest(r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#, &mut out);
        s.ingest(r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#, &mut out);
        assert!(out.iter().any(|e| matches!(e, ProviderEvent::TextDelta(t) if t == "Hel")));
        assert!(out.iter().any(|e| matches!(e, ProviderEvent::Done(FinishReason::ToolCalls))));
        let (i, o) = s.usage();
        assert_eq!((i, o), (12, 4));
        let turn = s.into_turn();
        assert_eq!(turn.text, "Hel");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "bash");
        assert_eq!(turn.tool_calls[0].arguments, "{\"cmd\":\"ls\"}");
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::thinking_budget;

    #[test]
    fn effort_maps_onto_a_sized_thinking_budget() {
        assert_eq!(thinking_budget(Some("minimal")), Some(1024));
        assert_eq!(thinking_budget(Some("low")), Some(2048));
        assert_eq!(thinking_budget(Some("HIGH")), Some(16384));
        // Unset or unknown leaves the model's own default alone.
        assert_eq!(thinking_budget(None), None);
        assert_eq!(thinking_budget(Some("")), None);
        assert_eq!(thinking_budget(Some("bogus")), None);
        // Ordering holds, so "higher effort" never means a smaller budget.
        let l = thinking_budget(Some("low")).unwrap();
        let m = thinking_budget(Some("medium")).unwrap();
        let h = thinking_budget(Some("high")).unwrap();
        assert!(l < m && m < h);
    }
}
