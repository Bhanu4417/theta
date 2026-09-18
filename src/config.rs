use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub behavior: Behavior,
    pub ai: AiConfig,
    pub compaction: CompactionConfig,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub keys: HashMap<String, String>,
    #[serde(default)]
    pub last_model: Option<(String, String)>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub mcp: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub provider: String,
    pub base_url: String,
    pub api_key_env: String,
    pub model: String,
    /// Attempts for a retryable failure. `0` keeps retrying — the session can
    /// always be interrupted, and a busy gateway is often unavailable for
    /// minutes rather than seconds.
    pub max_retries: u32,
    /// Longest a single retry waits (milliseconds). Exponential backoff is
    /// capped here, so a long outage keeps retrying without parking the session.
    pub retry_max_ms: u64,
    pub retry_base_ms: u64,
    pub timeout_secs: u64,
    pub max_turns: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reasoning_effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self { command: String::new(), args: Vec::new(), env: HashMap::new(), enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    pub enabled: bool,
    pub reserve_tokens: u64,
    /// Recent tokens kept verbatim across a compaction. `0` adapts to the
    /// model's window (a quarter of the usable context, clamped).
    pub keep_recent_tokens: u64,
    /// Roll old tool output out of the prompt. Tool output is the main way a
    /// session's context grows without bound, and this costs no model call.
    pub prune: bool,
    /// Recent tool output kept verbatim, counting back from the newest message.
    pub prune_protect_tokens: u64,
    /// Only prune once at least this much would be freed.
    pub prune_minimum_tokens: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub model_overrides: HashMap<String, ModelCompaction>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCompaction {
    pub reserve_tokens: Option<u64>,
    pub keep_recent_tokens: Option<u64>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            // Adaptive by default: a fixed budget suits no single model well.
            keep_recent_tokens: 0,
            prune: true,
            prune_protect_tokens: 40_000,
            prune_minimum_tokens: 20_000,
            model: String::new(),
            model_overrides: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub restore: bool,
    pub explorer_width: u16,
    pub history_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Behavior {
    pub auto_approve_permissions: bool,
    pub history_limit: u32,
    pub confirm_quit: bool,
    pub notify: bool,
    pub local_permissions: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            restore: true,
            explorer_width: 32,
            history_limit: 200,
        }
    }
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            auto_approve_permissions: false,
            history_limit: 200,
            confirm_quit: true,
            notify: true,
            // Allow everything by default. This is a multi-workspace tool: a
            // session can be opened in any project, and the built-in agents
            // reach across folders (they read a shared crate, run tests from a
            // workspace root, or fix a sibling package). Interrupting every one
            // of those turns the workspace into a prompt queue. `scoped` keeps
            // the folder-local safety net for anyone who wants it; `read-only`
            // and `deny` are the strict options.
            local_permissions: "allow".into(),
        }
    }
}

fn default_theme() -> String {
    "theta-night".to_string()
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            base_url: String::new(),
            api_key_env: "OPENAI_API_KEY".into(),
            model: "gpt-4o".into(),
            // Unlimited by default. A busy model behind a shared gateway can
            // return 500s and 429s for minutes, and giving up mid-turn loses
            // the work. The pane counts down and Ctrl+C stops it, so there is
            // no need for a cap. Raise `retry_max_ms` if the backoff should be
            // gentler.
            max_retries: 0,
            retry_base_ms: 500,
            retry_max_ms: 30_000,
            timeout_secs: 300,
            max_turns: 0,
            reasoning_effort: String::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ui: UiConfig::default(),
            behavior: Behavior::default(),
            ai: AiConfig::default(),
            compaction: CompactionConfig::default(),
            keys: HashMap::new(),
            theme: "theta-night".into(),
            last_model: None,
            mcp: HashMap::new(),
        }
    }
}

impl Config {
    pub fn config_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("theta").join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let mut cfg = Config::default();
        if let Some(path) = Self::config_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                cfg = toml::from_str(&text).unwrap_or(Config::default());
            }
        }
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("THETA_AI_PROVIDER") {
            if !v.trim().is_empty() {
                self.ai.provider = v;
            }
        }
        if let Ok(v) = std::env::var("THETA_AI_MODEL") {
            if !v.trim().is_empty() {
                self.ai.model = v;
            }
        }
        if let Ok(v) = std::env::var("THETA_AI_BASE_URL") {
            self.ai.base_url = v;
        }
        if std::env::var("THETA_AI_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false) {
            self.ai.api_key_env = "THETA_AI_API_KEY".into();
        }
        if let Ok(v) = std::env::var("THETA_AI_REASONING_EFFORT") {
            if !v.trim().is_empty() {
                self.ai.reasoning_effort = v;
            }
        }
    }

    pub fn save_default_if_missing() -> Result<()> {
        if let Some(path) = Self::config_path() {
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, toml::to_string_pretty(&Config::default())?)?;
            }
        }
        Ok(())
    }
}

impl Config {
    pub fn save(&self) -> Result<()> {
        if let Some(path) = Self::config_path() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, toml::to_string_pretty(self)?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_default_to_allow_for_multi_workspace_use() {
        assert_eq!(Behavior::default().local_permissions, "allow");
    }
}
