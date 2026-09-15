//! Provider credentials (`~/.config/theta/keys.toml`).
//!
//! Stored separately from `config.toml` with `0600` permissions. Environment
//! variables remain the fallback, so nothing breaks for users who only export
//! keys. Override the path with `THETA_KEYS_FILE` (used by tests).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// provider id -> API key.
    #[serde(default)]
    pub api_keys: HashMap<String, String>,
}

impl Credentials {
    pub fn path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("THETA_KEYS_FILE") {
            return Some(PathBuf::from(p));
        }
        dirs::config_dir().map(|d| d.join("theta").join("keys.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Stored key for a provider, else the provider's env var.
    pub fn resolve(&self, provider: &str, api_key_env: &str) -> Option<String> {
        if let Some(k) = self.api_keys.get(provider) {
            if !k.trim().is_empty() {
                return Some(k.clone());
            }
        }
        if api_key_env.is_empty() {
            return None;
        }
        std::env::var(api_key_env).ok().filter(|k| !k.trim().is_empty())
    }

    pub fn set(&mut self, provider: &str, key: &str) -> Result<()> {
        self.api_keys.insert(provider.to_string(), key.to_string());
        self.save()
    }

    pub fn remove(&mut self, provider: &str) -> Result<()> {
        self.api_keys.remove(provider);
        self.save()
    }

    pub fn providers(&self) -> Vec<String> {
        let mut v: Vec<String> = self.api_keys.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("theta-keys-{tag}-{}", std::process::id()))
    }

    #[test]
    fn set_save_load_and_resolve_with_env_fallback() {
        let path = temp("rt");
        let _ = std::fs::remove_file(&path);
        std::env::set_var("THETA_KEYS_FILE", &path);

        let mut c = Credentials::default();
        c.set("anthropic", "sk-ant-test").unwrap();
        let back = Credentials::load();
        assert_eq!(back.resolve("anthropic", "NOPE_ENV"), Some("sk-ant-test".into()));
        // Falls back to env when not stored.
        std::env::set_var("THETA_TEST_KEY_ENV", "env-key");
        assert_eq!(back.resolve("openai", "THETA_TEST_KEY_ENV"), Some("env-key".into()));
        assert_eq!(back.resolve("openai", ""), None);
        assert_eq!(back.providers(), vec!["anthropic".to_string()]);
        let _ = std::fs::remove_file(&path);
    }
}
