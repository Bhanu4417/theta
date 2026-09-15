//! Theta configuration (`~/.config/theta/config.toml`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub opencode: OcConfig,
    pub ui: UiConfig,
    pub behavior: Behavior,
    /// Which harness backend to drive: `"opencode"` (default) or `"local"`
    /// (Theta's own in-process agent loop).
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Settings for the local backend (LLM provider + model).
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
    /// Model used for agy CLI prompts (gemini-3.8-flash-medium etc).
    #[serde(default = "default_agy_model")]
    pub agy_model: String,
    /// Active theme name (see src/theme.rs THEMES). Defaults to Theta Night
    /// (Tokyo Night); only a user change rewrites it.
    #[serde(default = "default_theme")]
    pub theme: String,
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
            model_overrides: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OcConfig {
    /// Binary to launch for headless servers.
    pub binary: String,
    /// First port tried when hosting a per-directory server.
    pub port_base: u16,
    /// Milliseconds to wait for a spawned server to become healthy.
    pub startup_timeout_ms: u64,
    /// Leave servers running on exit so the next launch reuses them and
    /// connects near-instantly instead of paying the cold-start cost again.
    pub keep_alive: bool,
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
}

impl Default for OcConfig {
    fn default() -> Self {
        Self {
            binary: "opencode".into(),
            port_base: 4310,
            startup_timeout_ms: 90_000,
            keep_alive: true,
        }
    }
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
        }
    }
}

fn default_theme() -> String {
    "theta-night".to_string()
}

fn default_backend() -> String {
    "opencode".to_string()
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            base_url: String::new(),
            api_key_env: "OPENAI_API_KEY".into(),
            model: "gpt-4o".into(),
        }
    }
}

fn default_agy_model() -> String {
    "gemini-3.8-flash-medium".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opencode: OcConfig::default(),
            ui: UiConfig::default(),
            behavior: Behavior::default(),
            backend: default_backend(),
            ai: AiConfig::default(),
            compaction: CompactionConfig::default(),
            keys: HashMap::new(),
            theme: "theta-night".into(),
            agy_model: "gemini-3.8-flash-medium".into(),
            last_model: None,
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
        Ok(cfg)
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
