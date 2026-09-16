use futures::StreamExt;
use serde_json::{json, Value};

use crate::ai::{
    sse_data_lines, AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent, Role,
    ToolCall,
};
use crate::providers::ProviderError;

pub struct Responses {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
    timeout: Option<std::time::Duration>,
}

impl Responses {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            // Nagle's algorithm would coalesce small SSE frames, adding latency
            // to every streamed token.
            .tcp_nodelay(true)
            // Reuse connections aggressively: a multi-turn session otherwise
            // pays a fresh TLS handshake per request.
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let mut headers = Vec::new();
        if base_url.contains("opencode.ai/zen") {
            headers.push(("x-opencode-session".to_string(), crate::ai::openai::session_id()));
        }
        Self { base_url, api_key, client, headers, timeout: None }
    }
}

pub(crate) fn build_body(req: &ChatRequest, stream: bool) -> Value {
    let mut instructions: Option<String> = None;
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {
                instructions = Some(match instructions {
                    Some(prev) => format!("{prev}\n{}", m.text),
                    None => m.text.clone(),
                });
            }
            Role::User => {
                if m.images.is_empty() {
                    input.push(json!({ "role": "user", "content": m.text }));
                } else {
                    let mut parts = vec![json!({ "type": "input_text", "text": m.text })];
                    for img in &m.images {
                        parts.push(json!({
                            "type": "input_image",
                            "image_url": format!("data:{};base64,{}", img.mime, img.data)
                        }));
                    }
                    input.push(json!({ "role": "user", "content": parts }));
                }
            }
            Role::Assistant => {
                if !m.text.is_empty() {
                    input.push(json!({ "role": "assistant", "content": m.text }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.name,
                        "arguments": c.arguments,
                    }));
                }
            }
            Role::Tool => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "output": m.text,
                }));
            }
        }
    }
    if input.is_empty() {
        let text = instructions
            .take()
            .unwrap_or_else(|| "Continue.".to_string());
        input.push(json!({ "role": "user", "content": text }));
    }
    let mut body = json!({ "model": req.model, "input": input, "stream": stream });
    if let Some(i) = instructions {
        body["instructions"] = json!(i);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect(),
        );
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        body["max_output_tokens"] = json!(m);
    }
    if let Some(e) = &req.reasoning_effort {
        body["reasoning"] = json!({ "effort": e });
    }
    body
}

#[derive(Default)]
pub(crate) struct StreamAccum {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<(u64, u64)>,
    error: Option<String>,
    incomplete: Option<String>,
}

impl StreamAccum {
    pub(crate) fn ingest(&mut self, data: &str, out: &mut Vec<ProviderEvent>) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.output_text.delta" => {
                if let Some(d) = v.get("delta").and_then(Value::as_str) {
                    if !d.is_empty() {
                        self.text.push_str(d);
                        out.push(ProviderEvent::TextDelta(d.to_string()));
                    }
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(d) = v.get("delta").and_then(Value::as_str) {
                    if !d.is_empty() {
                        out.push(ProviderEvent::ReasoningDelta(d.to_string()));
                    }
                }
            }
            "response.output_text.done" => {
                if self.text.is_empty() {
                    if let Some(t) = v.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            self.text.push_str(t);
                            out.push(ProviderEvent::TextDelta(t.to_string()));
                        }
                    }
                }
            }
            "response.content_part.done" => {
                if self.text.is_empty() {
                    if let Some(t) = part_text(v.get("part")) {
                        if !t.is_empty() {
                            self.text.push_str(&t);
                            out.push(ProviderEvent::TextDelta(t));
                        }
                    }
                }
            }
            "response.output_item.done" => {
                let Some(item) = v.get("item") else { return };
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "message" => {
                        if self.text.is_empty() {
                            if let Some(t) = message_text(item) {
                                if !t.is_empty() {
                                    self.text.push_str(&t);
                                    out.push(ProviderEvent::TextDelta(t));
                                }
                            }
                        }
                        return;
                    }
                    "function_call" => {}
                    _ => return,
                }
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let arguments = match item.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(o) => o.to_string(),
                    None => "{}".to_string(),
                };
                let call = ToolCall { id, name, arguments };
                self.tool_calls.push(call.clone());
                out.push(ProviderEvent::ToolCall(call));
            }
            "response.completed" => {
                if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                    let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let output = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                    if input > 0 || output > 0 {
                        self.usage = Some((input, output));
                        out.push(ProviderEvent::Usage { input, output });
                    }
                }
            }
            "response.incomplete" => {
                let reason = v
                    .get("response")
                    .and_then(|r| r.get("incomplete_details"))
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                self.incomplete = Some(reason);
            }
            "response.failed" | "error" => {
                let msg = v
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .or_else(|| v.get("message").and_then(Value::as_str))
                    .unwrap_or("responses API error")
                    .to_string();
                self.error = Some(msg);
            }
            _ => {}
        }
    }

    pub(crate) fn into_turn(self) -> AssistantTurn {
        let finish = if !self.tool_calls.is_empty() {
            FinishReason::ToolCalls
        } else if self.incomplete.is_some() {
            FinishReason::Length
        } else {
            FinishReason::Stop
        };
        AssistantTurn { text: self.text, tool_calls: self.tool_calls, finish: Some(finish) }
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn incomplete(&self) -> Option<&str> {
        self.incomplete.as_deref()
    }
}

fn part_text(part: Option<&Value>) -> Option<String> {
    let p = part?;
    p.get("text").and_then(Value::as_str).map(str::to_string)
}

fn message_text(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let text: String = content
        .iter()
        .filter(|c| c.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|c| c.get("text").and_then(Value::as_str))
        .collect();
    Some(text)
}

impl Provider for Responses {
    fn id(&self) -> &'static str {
        "openai-responses"
    }

    fn set_timeout(&mut self, secs: u64) {
        self.timeout = (secs > 0).then(|| std::time::Duration::from_secs(secs));
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
                .post(format!("{}/responses", self.base_url))
                .header("Accept", "text/event-stream")
                .json(&body);
            if let Some(t) = self.timeout {
                rb = rb.timeout(t);
            }
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
            if let Some(err) = acc.error() {
                return Err(ProviderError::Protocol(err.to_string()));
            }
            let incomplete = acc.incomplete().map(str::to_string);
            let turn = acc.into_turn();
            if turn.text.trim().is_empty() && turn.tool_calls.is_empty() {
                if let Some(reason) = incomplete {
                    return Err(ProviderError::Protocol(format!(
                        "model produced no output ({reason}); lower reasoning effort or raise the token budget"
                    )));
                }
            }
            if let Some(f) = &turn.finish {
                on_event(ProviderEvent::Done(f.clone()));
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
    use crate::ai::ChatMessage;

    #[test]
    fn body_maps_messages_tools_and_instructions() {
        let req = ChatRequest {
            model: "muse-spark-1.3-contributor".into(),
            messages: vec![
                ChatMessage::system("be brief"),
                ChatMessage::user("list files"),
                ChatMessage::assistant(
                    "",
                    vec![ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        arguments: "{\"command\":\"ls\"}".into(),
                    }],
                ),
                ChatMessage::tool_result("c1", "a.rs"),
            ],
            tools: vec![crate::ai::ToolSpec {
                name: "bash".into(),
                description: "run".into(),
                parameters: json!({"type": "object"}),
            }],
            ..Default::default()
        };
        let b = build_body(&req, true);
        assert_eq!(b["instructions"], "be brief");
        assert_eq!(b["input"][0]["role"], "user");
        assert_eq!(b["input"][1]["type"], "function_call");
        assert_eq!(b["input"][1]["call_id"], "c1");
        assert_eq!(b["input"][2]["type"], "function_call_output");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["name"], "bash");
    }

    #[test]
    fn system_only_input_is_never_empty() {
        let req = ChatRequest {
            model: "muse-spark-1.3-contributor".into(),
            messages: vec![ChatMessage::system("only system notes")],
            ..Default::default()
        };
        let b = build_body(&req, true);
        let input = b["input"].as_array().expect("input array");
        assert!(!input.is_empty(), "Responses rejects empty input");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "only system notes");
        assert!(b.get("instructions").is_none());
    }

    #[test]
    fn streamed_text_and_tool_calls_are_accumulated() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(
            r#"{"type":"response.output_text.delta","delta":"Hel"}"#,
            &mut ev,
        );
        acc.ingest(
            r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"c1","name":"bash","arguments":"{\"command\":\"ls\"}"}}"#,
            &mut ev,
        );
        acc.ingest(
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            &mut ev,
        );
        assert!(ev.contains(&ProviderEvent::TextDelta("Hel".into())));
        assert!(ev.contains(&ProviderEvent::Usage { input: 10, output: 5 }));
        let turn = acc.into_turn();
        assert_eq!(turn.text, "Hel");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, "c1");
        assert_eq!(turn.finish, Some(FinishReason::ToolCalls));
    }

    #[test]
    fn incomplete_response_is_reported_as_length() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(
            r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}"#,
            &mut ev,
        );
        assert_eq!(acc.incomplete(), Some("max_output_tokens"));
        assert_eq!(acc.into_turn().finish, Some(FinishReason::Length));
    }

    #[test]
    fn message_item_text_is_captured_without_deltas() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(
            r#"{"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}}"#,
            &mut ev,
        );
        let turn = acc.into_turn();
        assert_eq!(turn.text, "hi");
        assert!(ev.contains(&ProviderEvent::TextDelta("hi".into())));
    }

    #[test]
    fn reasoning_effort_is_mapped_to_the_body() {
        let req = ChatRequest {
            model: "muse-spark-1.3-contributor".into(),
            messages: vec![ChatMessage::user("hi")],
            reasoning_effort: Some("minimal".into()),
            ..Default::default()
        };
        let b = build_body(&req, true);
        assert_eq!(b["reasoning"]["effort"], "minimal");
        let plain = ChatRequest {
            model: "x".into(),
            messages: vec![ChatMessage::user("hi")],
            ..Default::default()
        };
        assert!(build_body(&plain, true).get("reasoning").is_none());
    }

    #[test]
    fn failed_response_surfaces_an_error() {
        let mut acc = StreamAccum::default();
        let mut ev = Vec::new();
        acc.ingest(
            r#"{"type":"response.failed","response":{"error":{"message":"boom"}}}"#,
            &mut ev,
        );
        assert_eq!(acc.error(), Some("boom"));
    }
}
