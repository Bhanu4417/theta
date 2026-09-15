//! Typed client for the OpenCode HTTP server (verified against opencode 1.18.25).
//!
//! Parsing is deliberately lenient: metadata objects and event payloads are read
//! through `serde_json::Value` helpers so minor schema drift cannot crash the UI.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::harness::transcript::{
    Message, Part, PartKind, Role, TokenUsage, ToolInfo, ToolStatus,
};

// ---------------------------------------------------------------------------
// Data model (subset of the OpenAPI schema that the UI renders)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider_id: String,
    pub model_id: String,
    pub label: String,
    pub context_limit: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct OcSession {
    pub id: String,
    pub title: String,
    pub directory: String,
    pub updated_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// One selectable option in an agent question.
#[derive(Debug, Clone)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A single question the agent asks (the `ask`/question tool).
#[derive(Debug, Clone)]
pub struct QuestionInfo {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multiple: bool,
    pub custom: bool,
}

/// A pending question request from the AI assistant.
#[derive(Debug, Clone)]
pub struct QuestionRequest {
    pub id: String,
    pub session_id: String,
    pub questions: Vec<QuestionInfo>,
}

/// A pending permission request (the `permission.list` shape).
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: String,
    pub permission: String,
    pub patterns: Vec<String>,
    pub metadata: Value,
}

impl PermissionRequest {
    pub fn detail(&self) -> String {
        for key in ["command", "filePath", "file", "path", "url", "description"] {
            if let Some(v) = self.metadata.get(key).and_then(|v| v.as_str()) {
                return v.to_string();
            }
        }
        self.patterns.first().cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Lenient JSON parsing helpers
// ---------------------------------------------------------------------------

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn i64v(v: &Value, key: &str) -> Option<i64> {
    v.get(key)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
}

pub fn parse_tool_status(s: &str) -> ToolStatus {
    match s {
        "running" => ToolStatus::Running,
        "completed" => ToolStatus::Completed,
        "error" => ToolStatus::Error,
        _ => ToolStatus::Pending,
    }
}

pub fn parse_part(v: &Value) -> Option<Part> {
    let id = s(v, "id")?;
    let message_id = s(v, "messageID").unwrap_or_default();
    let typ = s(v, "type").unwrap_or_default();
    let kind = match typ.as_str() {
        "text" => PartKind::Text {
            text: s(v, "text").unwrap_or_default(),
            synthetic: v.get("synthetic").and_then(|x| x.as_bool()).unwrap_or(false),
        },
        "reasoning" => {
            let time = v.get("time");
            let end = time
                .and_then(|t| t.get("end"))
                .and_then(|e| e.as_i64().or_else(|| e.as_f64().map(|f| f as i64)));
            let start = time
                .and_then(|t| t.get("start"))
                .and_then(|e| e.as_i64().or_else(|| e.as_f64().map(|f| f as i64)));
            PartKind::Reasoning {
                text: s(v, "text").unwrap_or_default(),
                running: end.is_none(),
                start,
                end,
            }
        }
        "tool" => {
            let st = v.get("state").cloned().unwrap_or(Value::Null);
            let status = st
                .get("status")
                .and_then(|x| x.as_str())
                .map(parse_tool_status)
                .unwrap_or(ToolStatus::Pending);
            // Top-level metadata (older shapes) merged under state.metadata.
            let mut metadata = st.get("metadata").cloned().unwrap_or(json!({}));
            if let (Some(top), Some(obj)) = (v.get("metadata"), metadata.as_object_mut()) {
                if let Some(top_obj) = top.as_object() {
                    for (k, val) in top_obj {
                        obj.entry(k.clone()).or_insert(val.clone());
                    }
                }
            }
            let merged_state = json!({
                "metadata": metadata,
                "input": st.get("input").cloned().unwrap_or(json!({})),
            });
            let metadata_value = merged_state.get("metadata").cloned().unwrap_or(json!({}));
            // `state.output` only exists once the tool completes; while a
            // command runs, OpenCode streams its output into `metadata.output`.
            let output = s(&st, "output").or_else(|| {
                metadata_value
                    .get("output")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
            let info = ToolInfo {
                tool: s(v, "tool").unwrap_or_else(|| "tool".into()),
                call_id: s(v, "callID").unwrap_or_default(),
                status,
                title: s(&st, "title").filter(|t| !t.trim().is_empty()),
                input: st.get("input").cloned().unwrap_or(json!({})),
                output,
                error: s(&st, "error"),
                metadata: metadata_value,
                start_ms: st
                    .get("time")
                    .and_then(|t| t.get("start"))
                    .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))),
            };
            let _ = merged_state; // input already parsed above
            PartKind::Tool(info)
        }
        "step-start" => PartKind::StepStart,
        "step-finish" => PartKind::StepFinish,
        _ => PartKind::Other,
    };
    Some(Part {
        id,
        message_id,
        kind,
    })
}

pub fn parse_message(v: &Value) -> Option<Message> {
    let id = s(v, "id")?;
    let role = match s(v, "role").as_deref() {
        Some("user") => Role::User,
        _ => Role::Assistant,
    };
    let error = v.get("error").and_then(|e| {
        let name = s(e, "name").unwrap_or_else(|| "error".into());
        let msg = s(e, "message").or_else(|| s(e, "data").map(|d| d.to_string()));
        Some(match msg {
            Some(m) if !m.is_empty() => format!("{name}: {m}"),
            _ => name,
        })
    });
    let completed = v
        .get("time")
        .and_then(|t| t.get("completed"))
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)));
    let created = v
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)));
    let cost = v.get("cost").and_then(|c| c.as_f64());
    let tokens = v.get("tokens").and_then(|t| {
        let g = |k: &str| {
            t.get(k)
                .and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)))
        };
        let cache = t.get("cache");
        let cr = cache.and_then(|c| c.get("read")).and_then(|x| x.as_u64()).unwrap_or(0);
        let cw = cache.and_then(|c| c.get("write")).and_then(|x| x.as_u64()).unwrap_or(0);
        Some(TokenUsage {
            input: g("input").unwrap_or(0),
            output: g("output").unwrap_or(0),
            reasoning: g("reasoning").unwrap_or(0),
            cache_read: cr,
            cache_write: cw,
        })
    });
    Some(Message {
        id,
        role,
        error,
        completed,
        created,
        cost,
        tokens,
        parts: Vec::new(),
    })
}

pub fn parse_session(v: &Value) -> Option<OcSession> {
    Some(OcSession {
        id: s(v, "id")?,
        title: s(v, "title").unwrap_or_default(),
        directory: s(v, "directory").unwrap_or_default(),
        updated_ms: v
            .get("time")
            .and_then(|t| t.get("updated"))
            .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))),
    })
}

pub fn parse_message_with_parts(v: &Value) -> Option<Message> {
    let mut msg = parse_message(v.get("info")?)?;
    if let Some(parts) = v.get("parts").and_then(|p| p.as_array()) {
        for p in parts {
            if let Some(part) = parse_part(p) {
                msg.parts.push(part);
            }
        }
    }
    Some(msg)
}

pub fn parse_providers(body: &Value) -> Vec<ModelEntry> {
    let mut out: Vec<ModelEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(providers) = body.get("providers").and_then(|p| p.as_array()) {
        for p in providers {
            let pid = match s(p, "id") {
                Some(id) => id,
                None => continue,
            };
            if let Some(models) = p.get("models").and_then(|m| m.as_object()) {
                let mut keys: Vec<&String> = models.keys().collect();
                keys.sort();
                for mid in keys {
                    let key = (pid.clone(), mid.clone());
                    if seen.insert(key) {
                        let limit = models
                            .get(mid.as_str())
                            .and_then(|m| m.get("limit"))
                            .and_then(|l| l.get("context"))
                            .and_then(|c| c.as_u64());
                        out.push(ModelEntry {
                            label: format!("{pid}/{mid}"),
                            provider_id: pid.clone(),
                            model_id: mid.clone(),
                            context_limit: limit,
                        });
                    }
                }
            }
        }
    }
    out
}

pub fn parse_default_model(body: &Value) -> Option<ModelRef> {
    // `default` maps providerID -> modelID
    let d = body.get("default")?.as_object()?;
    for (pid, mid) in d {
        if let Some(mid) = mid.as_str() {
            return Some(ModelRef {
                provider_id: pid.clone(),
                model_id: mid.to_string(),
            });
        }
    }
    None
}

pub fn parse_agents(body: &Value) -> Vec<AgentInfo> {
    body.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let name = s(v, "name")?;
                    let mode = s(v, "mode").unwrap_or_default();
                    let hidden = v.get("hidden").and_then(|h| h.as_bool()).unwrap_or(false);
                    if hidden || mode == "subagent" {
                        return None;
                    }
                    Some(AgentInfo {
                        name,
                        description: s(v, "description").unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_commands(body: &Value) -> Vec<CustomCommand> {
    body.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let name = s(v, "name")?;
                    Some(CustomCommand {
                        name,
                        description: s(v, "description").unwrap_or_default(),
                        source: s(v, "source").unwrap_or_else(|| "command".into()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_find_file(body: &Value) -> Vec<String> {
    body.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_grep(body: &Value) -> Vec<GrepMatch> {
    let mut out = Vec::new();
    if let Some(arr) = body.as_array() {
        for m in arr {
            let path = m
                .get("path")
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or_default();
            let text = m
                .get("lines")
                .and_then(|l| l.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .trim_end();
            let line = m.get("line_number").and_then(|l| l.as_u64()).unwrap_or(0);
            if !path.is_empty() {
                out.push(GrepMatch {
                    path: path.to_string(),
                    line,
                    text: text.to_string(),
                });
            }
        }
    }
    out
}

fn parse_question_info(v: &Value) -> Option<QuestionInfo> {
    let question = s(v, "question")?;
    let options = v
        .get("options")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(QuestionOption {
                        label: s(o, "label")?,
                        description: s(o, "description").unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(QuestionInfo {
        question,
        header: s(v, "header").unwrap_or_default(),
        options,
        multiple: v.get("multiple").and_then(|b| b.as_bool()).unwrap_or(false),
        custom: v.get("custom").and_then(|b| b.as_bool()).unwrap_or(false),
    })
}

pub fn parse_question_request(v: &Value) -> Option<QuestionRequest> {
    let id = s(v, "id")?;
    let questions = v
        .get("questions")
        .and_then(|q| q.as_array())
        .map(|arr| arr.iter().filter_map(parse_question_info).collect())
        .unwrap_or_default();
    Some(QuestionRequest {
        id,
        session_id: s(v, "sessionID").unwrap_or_default(),
        questions,
    })
}

pub fn parse_questions(body: &Value) -> Vec<QuestionRequest> {
    body.as_array()
        .map(|a| a.iter().filter_map(parse_question_request).collect())
        .unwrap_or_default()
}

pub fn parse_permission_requests(body: &Value) -> Vec<PermissionRequest> {
    body.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    let id = s(v, "id")?;
                    let patterns = v
                        .get("patterns")
                        .and_then(|p| p.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(PermissionRequest {
                        id,
                        session_id: s(v, "sessionID").unwrap_or_default(),
                        permission: s(v, "permission").unwrap_or_else(|| "permission".into()),
                        patterns,
                        metadata: v.get("metadata").cloned().unwrap_or(json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Client {
    pub base: String,
    http: reqwest::Client,
    sse: reqwest::Client,
}

impl Client {
    pub fn new(base: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        let sse = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest sse client");
        Self { base, http, sse }
    }

    /// Client without a request timeout, for long-lived SSE streams.
    pub fn raw(&self) -> &reqwest::Client {
        &self.sse
    }

    pub async fn health(&self) -> Result<()> {
        let v: Value = self
            .http
            .get(format!("{}/global/health", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if v.get("healthy").and_then(|h| h.as_bool()).unwrap_or(false) {
            Ok(())
        } else {
            Err(anyhow!("unhealthy"))
        }
    }

    /// Directory the server was started in (to detect port collisions).
    pub async fn path_info(&self) -> Result<String> {
        let v: Value = self
            .http
            .get(format!("{}/path", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(s(&v, "directory").unwrap_or_default())
    }

    pub async fn create_session(&self, title: &str) -> Result<OcSession> {
        let body = if title.trim().is_empty() {
            json!({})
        } else {
            json!({ "title": title })
        };
        let v: Value = self
            .http
            .post(format!("{}/session", self.base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_session(&v).ok_or_else(|| anyhow!("bad session response"))
    }

    pub async fn get_session(&self, id: &str) -> Result<OcSession> {
        let v: Value = self
            .http
            .get(format!("{}/session/{id}", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_session(&v).ok_or_else(|| anyhow!("bad session response"))
    }

    pub async fn list_sessions(&self) -> Result<Vec<OcSession>> {
        let v: Value = self
            .http
            .get(format!("{}/session", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_session).collect())
            .unwrap_or_default())
    }

    /// List sessions for a specific directory using this server. OpenCode
    /// scopes sessions per project, but accepts a `directory` override, so a
    /// single server can enumerate every folder without starting more.
    pub async fn list_sessions_in(&self, directory: &str) -> Result<Vec<OcSession>> {
        let v: Value = self
            .http
            .get(format!("{}/session", self.base))
            .query(&[("directory", directory)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_session).collect())
            .unwrap_or_default())
    }

    pub async fn messages(&self, sid: &str, limit: u32) -> Result<Vec<Message>> {
        let v: Value = self
            .http
            .get(format!("{}/session/{sid}/message", self.base))
            .query(&[("limit", limit.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_message_with_parts).collect())
            .unwrap_or_default())
    }

    pub async fn prompt_async(
        &self,
        sid: &str,
        text: &str,
        model: Option<&ModelRef>,
        agent: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({ "parts": [{ "type": "text", "text": text }] });
        if let Some(m) = model {
            body["model"] = json!({ "providerID": m.provider_id, "modelID": m.model_id });
        }
        if let Some(a) = agent {
            if !a.is_empty() {
                body["agent"] = json!(a);
            }
        }
        let resp = self
            .http
            .post(format!("{}/session/{sid}/prompt_async", self.base))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("prompt failed ({status}): {text}"));
        }
        Ok(())
    }

    pub async fn abort(&self, sid: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/session/{sid}/abort", self.base))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    pub async fn permission_reply(&self, sid: &str, pid: &str, response: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!(
                "{}/session/{sid}/permissions/{pid}",
                self.base
            ))
            .json(&json!({ "response": response }))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    pub async fn providers(&self) -> Result<(Vec<ModelEntry>, Option<ModelRef>)> {
        let v: Value = self
            .http
            .get(format!("{}/config/providers", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok((parse_providers(&v), parse_default_model(&v)))
    }

    pub async fn find_files(&self, query: &str, limit: u32) -> Result<Vec<String>> {
        let v: Value = self
            .http
            .get(format!("{}/find/file", self.base))
            .query(&[
                ("query", query.to_string()),
                ("limit", limit.to_string()),
                ("type", "file".to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_find_file(&v))
    }

    pub async fn find_pattern(&self, pattern: &str, limit: u32) -> Result<Vec<GrepMatch>> {
        let resp = self
            .http
            .get(format!("{}/find", self.base))
            .query(&[
                ("pattern", pattern.to_string()),
                ("limit", limit.to_string()),
            ])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            // Retry with regex metacharacters escaped (plain-text search).
            let escaped = escape_regex(pattern);
            let resp = self
                .http
                .get(format!("{}/find", self.base))
                .query(&[
                    ("pattern", escaped),
                    ("limit", limit.to_string()),
                ])
                .send()
                .await?;
            if !resp.status().is_success() {
                return Err(anyhow!("search failed ({})", resp.status()));
            }
            let v: Value = resp.json().await?;
            return Ok(parse_grep(&v));
        }
        let v: Value = resp.json().await?;
        Ok(parse_grep(&v))
    }

    /// Returns (text content, unified diff when known).
    pub async fn file_content(&self, path: &str) -> Result<(Option<String>, Option<String>)> {
        let v: Value = self
            .http
            .get(format!("{}/file/content", self.base))
            .query(&[("path", path.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let typ = s(&v, "type").unwrap_or_default();
        if typ == "binary" {
            return Ok((None, None));
        }
        Ok((
            s(&v, "content"),
            s(&v, "diff").filter(|d| !d.trim().is_empty()),
        ))
    }

    pub async fn session_status_map(&self) -> Result<BTreeMap<String, Value>> {
        let v: Value = self
            .http
            .get(format!("{}/session/status", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.as_object()
            .map(|o| o.iter().map(|(k, val)| (k.clone(), val.clone())).collect())
            .unwrap_or_default())
    }

    pub async fn agents(&self) -> Result<Vec<AgentInfo>> {
        let v: Value = self
            .http
            .get(format!("{}/agent", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_agents(&v))
    }

    pub async fn commands(&self) -> Result<Vec<CustomCommand>> {
        let v: Value = self
            .http
            .get(format!("{}/command", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_commands(&v))
    }

    /// Run a server-side command (custom commands, /init, …).
    pub async fn run_command(&self, sid: &str, command: &str, arguments: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/session/{sid}/command", self.base))
            .json(&json!({ "command": command, "arguments": arguments }))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    pub async fn summarize(&self, sid: &str, model: &ModelRef) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/session/{sid}/summarize", self.base))
            .json(&json!({
                "providerID": model.provider_id,
                "modelID": model.model_id,
            }))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    pub async fn revert(&self, sid: &str, message_id: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/session/{sid}/revert", self.base))
            .json(&json!({ "messageID": message_id }))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    pub async fn unrevert(&self, sid: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/session/{sid}/unrevert", self.base))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    /// Share the session; returns the share URL when present.
    pub async fn share(&self, sid: &str) -> Result<Option<String>> {
        let v: Value = self
            .http
            .post(format!("{}/session/{sid}/share", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(v.get("share")
            .and_then(|sh| sh.get("url"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string()))
    }

    /// Fork a session. When `at` is given, the new session copies history up
    /// to (and including) that message, so it never inherits an in-progress
    /// turn from the source.
    pub async fn fork(&self, sid: &str, at: Option<&str>) -> Result<OcSession> {
        let body = match at {
            Some(id) => json!({ "messageID": id }),
            None => json!({}),
        };
        let v: Value = self
            .http
            .post(format!("{}/session/{sid}/fork", self.base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_session(&v).ok_or_else(|| anyhow!("bad fork response"))
    }

    /// All pending questions across sessions on this server.
    pub async fn questions(&self) -> Result<Vec<QuestionRequest>> {
        let v: Value = self
            .http
            .get(format!("{}/question", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_questions(&v))
    }

    /// Answer a question request (answers in order, each a list of labels).
    pub async fn reply_question(&self, id: &str, answers: Vec<Vec<String>>) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/question/{id}/reply", self.base))
            .json(&json!({ "answers": answers }))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    pub async fn reject_question(&self, id: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/question/{id}/reject", self.base))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }

    /// All pending permission requests across sessions on this server.
    pub async fn permissions(&self) -> Result<Vec<PermissionRequest>> {
        let v: Value = self
            .http
            .get(format!("{}/permission", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(parse_permission_requests(&v))
    }

    pub async fn unshare(&self, sid: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}/session/{sid}/share", self.base))
            .send()
            .await?;
        resp.error_for_status()?;
        Ok(())
    }
}

/// Escape regex metacharacters for plain-text search fallback.
pub fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
