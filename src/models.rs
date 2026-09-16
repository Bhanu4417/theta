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

