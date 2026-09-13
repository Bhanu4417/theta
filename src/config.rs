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
}

impl Default for OcConfig {
    fn default() -> Self {
        Self {
            binary: "opencode".into(),
            port_base: 4310,
            startup_timeout_ms: 90_000,
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
        }
    }
}

fn default_theme() -> String {
    "theta-night".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opencode: OcConfig::default(),
            ui: UiConfig::default(),
            behavior: Behavior::default(),
            keys: HashMap::new(),
            theme: "theta-night".into(),
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
