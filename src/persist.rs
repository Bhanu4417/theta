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
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub scroll: u64,
    #[serde(default = "follow_bottom_default")]
    pub stick_bottom: bool,
}

fn follow_bottom_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedRow {
    pub weight: f32,
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
    #[serde(default)]
    pub known_dirs: Vec<String>,
}

pub fn state_path() -> Option<PathBuf> {
    let base = match std::env::var_os("THETA_DATA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()?.join("theta"),
    };
    Some(base.join("workspace.toml"))
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


pub const TRANSCRIPT_CACHE_LIMIT: usize = 500;

pub fn transcript_path() -> Option<PathBuf> {
    let base = match std::env::var_os("THETA_DATA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()?.join("theta"),
    };
    Some(base.join("transcripts.json"))
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

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn scroll_position_roundtrips_through_toml() {
        let ws = Workspace {
            sessions: vec![SavedSession {
                name: "auth".into(),
                dir: "/proj".into(),
                scroll: 42,
                stick_bottom: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&ws).unwrap();
        let back: Workspace = toml::from_str(&text).unwrap();
        assert_eq!(back.sessions[0].scroll, 42);
        assert!(!back.sessions[0].stick_bottom);
    }

    #[test]
    fn older_files_without_scroll_fields_still_follow_the_bottom() {
        let text = r#"
            focused = 0
            rows = []
            [[sessions]]
            name = "auth"
            dir = "/proj"
        "#;
        let ws: Workspace = toml::from_str(text).unwrap();
        assert_eq!(ws.sessions[0].scroll, 0);
        assert!(ws.sessions[0].stick_bottom, "absent field means follow bottom");
    }
}
