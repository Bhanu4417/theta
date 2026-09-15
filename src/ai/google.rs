//! Google Gemini provider (`:streamGenerateContent`, SSE).

use std::collections::HashMap;

use futures::StreamExt;
use serde_json::{json, Value};

use crate::ai::{
    sse_data_lines, AssistantTurn, ChatMessage, ChatRequest, FinishReason, Provider, ProviderEvent,
    Role, ToolCall,
};
use crate::providers::ProviderError;

pub struct Google {
    api_key: Option<String>,
    base_url: String,
    client: reqwest::Client,
}

impl Google {
    pub fn new(api_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            api_key,
            base_url: "https://generativelanguage.googleapis.com".into(),
            client,
        }
    }
}

/// Map tool-call ids to names by scanning assistant messages (Gemini's
/// `functionResponse` needs the function name, not an id).
fn tool_names(messages: &[ChatMessage]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for m in messages {
        for c in &m.tool_calls {
            map.insert(c.id.clone(), c.name.clone());
        }
    }
    map
}

/// Build the `:streamGenerateContent` body (pure, testable).
pub fn build_body(req: &ChatRequest) -> Value {
    let names = tool_names(&req.messages);
    let mut system: Option<String> = None;
    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => system = Some(m.text.clone()),
            Role::User => contents.push(json!({ "role": "user", "parts": [{ "text": m.text }] })),
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                if !m.text.is_empty() {
                    parts.push(json!({ "text": m.text }));
                }
                for c in &m.tool_calls {
                    let args: Value = serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                    parts.push(json!({ "functionCall": { "name": c.name, "args": args } }));
                }
                if !parts.is_empty() {
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
            }
            Role::Tool => {
                let name = m
                    .tool_call_id
                    .as_ref()
                    .and_then(|id| names.get(id))
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
                contents.push(json!({
                    "role": "user",
                    "parts": [{ "functionResponse": { "name": name, "response": { "result": m.text } } }]
                }));
            }
        }
    }
    let mut body = json!({ "contents": contents });
    if let Some(sys) = system {
        body["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
    }
    if !req.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": req.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })).collect::<Vec<_>>()
        }]);
    }
    let mut gen = json!({});
    if let Some(t) = req.temperature {
        gen["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        gen["maxOutputTokens"] = json!(m);
    }
    if gen.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        body["generationConfig"] = gen;
    }
    body
}

#[derive(Default)]
pub struct StreamState {
    text: String,
    calls: Vec<ToolCall>,
    input_tokens: u64,
    output_tokens: u64,
    finish: Option<FinishReason>,
}

impl StreamState {
    pub fn ingest(&mut self, data: &str, out: &mut Vec<ProviderEvent>) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(u) = v.get("usageMetadata") {
            self.input_tokens = u.get("promptTokenCount").and_then(|x| x.as_u64()).unwrap_or(self.input_tokens);
            self.output_tokens = u
                .get("candidatesTokenCount")
                .and_then(|x| x.as_u64())
                .unwrap_or(self.output_tokens);
        }
        let Some(cand) = v.get("candidates").and_then(|c| c.get(0)) else {
            return;
        };
        if let Some(parts) = cand
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        self.text.push_str(t);
                        out.push(ProviderEvent::TextDelta(t.to_string()));
                    }
                }
                if let Some(fc) = p.get("functionCall") {
                    let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("tool").to_string();
                    let args = fc.get("args").cloned().unwrap_or(json!({}));
                    let id = format!("call-{}", self.calls.len());
                    let call = ToolCall {
                        id: id.clone(),
                        name,
                        arguments: serde_json::to_string(&args).unwrap_or_default(),
                    };
                    out.push(ProviderEvent::ToolCall(call.clone()));
                    self.calls.push(call);
                }
            }
        }
        if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
            let f = match reason {
                "STOP" => FinishReason::Stop,
                "MAX_TOKENS" => FinishReason::Length,
                other => FinishReason::Other(other.to_string()),
            };
            self.finish = Some(f.clone());
            out.push(ProviderEvent::Done(f));
        }
    }

    fn into_turn(self) -> AssistantTurn {
        AssistantTurn { text: self.text, tool_calls: self.calls, finish: self.finish }
    }

    pub fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }
}

impl Provider for Google {
    fn id(&self) -> &'static str {
        "google"
    }

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let body = build_body(&request);
            let url = format!(
                "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
                self.base_url.trim_end_matches('/'),
                request.model
            );
            let mut rb = self
                .client
                .post(url)
                .header("Accept", "text/event-stream")
                .json(&body);
            if let Some(key) = &self.api_key {
                rb = rb.query(&[("key", key)]);
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
            let (i, o) = state.usage();
            if i > 0 || o > 0 {
                on_event(ProviderEvent::Usage { input: i, output: o });
            }
            Ok(state.into_turn())
        })
    }
}

fn net(e: reqwest::Error) -> ProviderError {
    ProviderError::Transport(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ToolSpec;

    #[test]
    fn body_maps_roles_tools_and_function_responses() {
        let req = ChatRequest {
            model: "gemini-2.0-flash".into(),
            messages: vec![
                ChatMessage::system("be brief"),
                ChatMessage::user("read it"),
                ChatMessage::assistant("", vec![ToolCall { id: "c1".into(), name: "read".into(), arguments: "{\"path\":\"/a\"}".into() }]),
                ChatMessage::tool_result("c1", "contents"),
            ],
            tools: vec![ToolSpec { name: "read".into(), description: "r".into(), parameters: json!({"type":"object"}) }],
            temperature: None,
            max_tokens: None,
        };
        let b = build_body(&req);
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(b["contents"][0]["parts"][0]["text"], "read it");
        assert_eq!(b["contents"][1]["role"], "model");
        assert_eq!(b["contents"][1]["parts"][0]["functionCall"]["name"], "read");
        assert_eq!(
            b["contents"][2]["parts"][0]["functionResponse"]["name"],
            "read"
        );
        assert_eq!(b["tools"][0]["functionDeclarations"][0]["name"], "read");
    }

    #[test]
    fn stream_accumulates_text_function_calls_and_usage() {
        let mut s = StreamState::default();
        let mut out = Vec::new();
        s.ingest(
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi "}]}}]}"#,
            &mut out,
        );
        s.ingest(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"bash","args":{"command":"ls"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":2}}"#,
            &mut out,
        );
        assert!(out.iter().any(|e| matches!(e, ProviderEvent::TextDelta(t) if t == "Hi ")));
        assert!(out.iter().any(|e| matches!(e, ProviderEvent::Done(FinishReason::Stop))));
        let (i, o) = s.usage();
        assert_eq!((i, o), (7, 2));
        let turn = s.into_turn();
        assert_eq!(turn.text, "Hi ");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "bash");
        assert_eq!(turn.tool_calls[0].arguments, "{\"command\":\"ls\"}");
    }
}
