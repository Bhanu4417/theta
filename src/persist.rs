//! Workspace persistence (`~/.local/share/theta/workspace.toml`).

use crate::harness::transcript::Message;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedSession {
    pub name: String,
    pub dir: String,
    pub oc_sid: Option<String>,
    pub model: Option<(String, String)>,
    #[serde(default)]
    pub agent: Option<String>,
    /// Provider id (e.g. "opencode"). Absent in older files → default provider.
    #[serde(default)]
    pub provider: Option<String>,
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
    /// Directories Theta has opened — used to list sessions across projects.
    #[serde(default)]
    pub known_dirs: Vec<String>,
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

// ---------------------------------------------------------------------------
// Transcript cache
//
// A display-only cache of the last messages per session so restored panes
// render instantly while the provider replays authoritative history. It is
// always replaced by provider history once `HistoryLoaded` arrives; it is
// never the source of truth and never deletes server history.
// ---------------------------------------------------------------------------

/// Maximum messages cached per session.
pub const TRANSCRIPT_CACHE_LIMIT: usize = 120;

pub fn transcript_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("theta").join("transcripts.json"))
}

pub fn save_transcripts(map: &HashMap<String, Vec<Message>>) -> Result<()> {
    if let Some(path) = transcript_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string(map)?)?;
    }
    Ok(())
}

pub fn load_transcripts() -> HashMap<String, Vec<Message>> {
    let Some(path) = transcript_path() else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::harness::transcript::{Message, Part, PartKind, Role};

    #[test]
    fn transcript_cache_roundtrips_through_json() {
        let msg = Message {
            id: "msg_1".into(),
            role: Role::Assistant,
            error: None,
            completed: Some(1),
            created: Some(1),
            cost: Some(0.01),
            tokens: None,
            parts: vec![Part {
                id: "p1".into(),
                message_id: "msg_1".into(),
                kind: PartKind::Text {
                    text: "hello".into(),
                    synthetic: false,
                },
            }],
        };
        let mut map: HashMap<String, Vec<Message>> = HashMap::new();
        map.insert("/proj|ses_1".to_string(), vec![msg]);
        let json = serde_json::to_string(&map).unwrap();
        let back: HashMap<String, Vec<Message>> = serde_json::from_str(&json).unwrap();
        assert_eq!(back["/proj|ses_1"][0].parts.len(), 1);
        assert_eq!(back["/proj|ses_1"][0].role, Role::Assistant);
    }
}
