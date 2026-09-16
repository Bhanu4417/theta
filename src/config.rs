//! Theta configuration (`~/.config/theta/config.toml`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub behavior: Behavior,
    /// LLM provider + model for Theta's own agent loop.
    pub ai: AiConfig,
    /// Context compaction budgets (local backend).
    pub compaction: CompactionConfig,
    /// User keybindings: action name -> key spec ("ctrl+n").
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub keys: HashMap<String, String>,
    /// Last model the user picked (providerID, modelID) — default for new
    /// sessions until changed.
    #[serde(default)]
    pub last_model: Option<(String, String)>,
    /// Active theme name (see src/theme.rs THEMES). Defaults to Theta Night
    /// (Tokyo Night); only a user change rewrites it.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// stdio MCP servers, keyed by name (local backend).
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub mcp: HashMap<String, McpServerConfig>,
}

/// LLM provider settings for the local backend. Any OpenAI-compatible gateway
/// works: set `provider` to a known preset, or leave it `compat`/empty and set
/// `base_url` explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub provider: String,
    pub base_url: String,
    pub api_key_env: String,
    pub model: String,
    /// Retry transport/5xx failures this many times with exponential backoff.
    pub max_retries: u32,
    /// Base backoff delay in milliseconds (doubled per attempt).
    pub retry_base_ms: u64,
    /// Per-request timeout in seconds (0 = no timeout).
    pub timeout_secs: u64,
    /// Maximum provider round-trips (assistant response + its tool calls)
    /// before the loop stops with a notice. Each tool call consumes one, so a
    /// real task needs headroom; `0` (the default) means no limit — the model
    /// runs until it finishes and you interrupt with Ctrl+C, like OpenCode/Pi.
    pub max_turns: usize,
    /// Reasoning effort for reasoning models (`minimal`/`low`/`medium`/`high`);
    /// empty keeps the provider default. Lower is faster.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reasoning_effort: String,
}

/// One stdio MCP server (Model Context Protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Executable to spawn.
    pub command: String,
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Set false to keep the definition but not launch it.
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self { command: String::new(), args: Vec::new(), env: HashMap::new(), enabled: true }
    }
}

/// Context-compaction tuning, mirroring Pi's reserve/keep token budgets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Auto-compact when the prompt approaches the model's limit.
    pub enabled: bool,
    /// Tokens reserved for the model's response (trigger headroom).
    pub reserve_tokens: u64,
    /// Recent tokens kept verbatim (not summarized).
    pub keep_recent_tokens: u64,
    /// Model used to write summaries (fast/cheap). Empty = the session model,
    /// bounded by a small output cap so it stays quick either way.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Per-model overrides keyed by `provider/model` or bare `model`.
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
            keep_recent_tokens: 20_000,
            model: String::new(),
            model_overrides: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Restore the last workspace on startup.
    pub restore: bool,
    /// Width of the file explorer panel.
    pub explorer_width: u16,
    /// How many history entries are kept per session input.
    pub history_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Behavior {
    /// Automatically answer permission requests (uses "once").
    pub auto_approve_permissions: bool,
    /// Max transcript messages fetched when attaching/restoring a session.
    pub history_limit: u32,
    /// Ask before quitting while agents are working.
    pub confirm_quit: bool,
    /// Ring the bell / send a desktop notification when a background agent
    /// finishes or needs attention.
    pub notify: bool,
    /// Local-backend tool permission mode: `allow` (default, like OpenCode),
    /// `ask`, `deny`, or `read-only`. In `ask`, reads inside the working
    /// directory are auto-allowed; mutations, commands, and out-of-tree reads
    /// prompt.
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
            // OpenCode-style permissive default: tools run without prompting.
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
            max_retries: 3,
            retry_base_ms: 500,
            timeout_secs: 300,
            // Unlimited: run until the model finishes; Ctrl+C interrupts. Set a
            // positive number only if you want an automatic safety stop.
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

    /// `THETA_AI_*` environment overrides (useful for testing and CI).
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
    fn permissions_default_to_permissive_like_opencode() {
        assert_eq!(Behavior::default().local_permissions, "allow");
    }
}
