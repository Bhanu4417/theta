//! Workspace persistence (`~/.local/share/theta/workspace.toml`).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedSession {
    pub name: String,
    pub dir: String,
    pub oc_sid: Option<String>,
    pub model: Option<(String, String)>,
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedRow {
    pub weight: f32,
    /// (weight, session index into `sessions`)
    pub cells: Vec<(f32, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Workspace {
    pub sessions: Vec<SavedSession>,
    pub rows: Vec<SavedRow>,
    pub focused: usize,
    pub maximized: Option<usize>,
    #[serde(default)]
    pub scheme: Option<String>,
}

pub fn state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("theta").join("workspace.toml"))
}

pub fn save(ws: &Workspace) -> Result<()> {
    if let Some(path) = state_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(ws)?)?;
    }
    Ok(())
}

pub fn load() -> Option<Workspace> {
    let path = state_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

pub fn clear() {
    if let Some(path) = state_path() {
        let _ = std::fs::remove_file(path);
    }
}
