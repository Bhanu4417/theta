//! Small shared data types for sessions, models, and agent questions.
//!
//! These were once the OpenCode client's shapes; Theta now owns its harness, so
//! they live here as provider-neutral value types the UI and events use.

use serde_json::Value;

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

