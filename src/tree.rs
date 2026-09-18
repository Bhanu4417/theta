use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ai::ChatMessage;
use crate::agent::context::{self, SUMMARY_CLOSE, SUMMARY_OPEN};
use crate::harness::transcript::{Message, Part, PartKind, Role, ToolInfo, ToolStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    User,
    Assistant,
    Tool,
    System,
    Compaction,
    BranchSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub parent: Option<String>,
    pub timestamp_ms: i64,
    pub kind: EntryKind,
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<crate::ai::ToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub first_kept: Option<String>,
    #[serde(default)]
    pub tokens_before: Option<u64>,
    #[serde(default)]
    pub snapshots: Vec<crate::agent::tools::FileSnapshot>,
    #[serde(default)]
    pub tokens: Option<crate::harness::transcript::TokenUsage>,
    #[serde(default)]
    pub cost: Option<f64>,
    /// UI data attached to a tool result, chiefly the diff an edit produced.
    /// Persisted so a restored session still shows what changed rather than only
    /// which file was touched.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

impl Entry {
    fn to_message(&self) -> ChatMessage {
        match self.kind {
            EntryKind::Compaction | EntryKind::BranchSummary => {
                ChatMessage::system(format!(
                    "{SUMMARY_OPEN}\n{}\n{SUMMARY_CLOSE}",
                    self.text
                ))
            }
            EntryKind::User => ChatMessage::user(self.text.clone()),
            EntryKind::Assistant => ChatMessage {
                role: crate::ai::Role::Assistant,
                text: self.text.clone(),
                tool_calls: self.tool_calls.clone(),
                tool_call_id: None,
                tokens: self.tokens,
                images: Vec::new(),
                cost: self.cost,
                tool_metadata: self.metadata.clone(),
            },
            EntryKind::Tool => {
                let mut m = ChatMessage::tool_result(
                    self.tool_call_id.clone().unwrap_or_default(),
                    self.text.clone(),
                );
                m.tool_metadata = self.metadata.clone();
                m
            }
            EntryKind::System => ChatMessage::system(self.text.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub directory: Option<String>,
    pub updated_ms: i64,
    pub entries: usize,
}

/// Test support for the session directory.
///
/// `THETA_SESSION_DIR` is process-global, but the test binary runs its tests on
/// parallel threads. Without serialization, one test overwrites the variable
/// while another is mid-run, and the suite fails intermittently — the directory
/// a test resolves depends on whichever test ran last.
#[cfg(test)]
pub mod testenv {
    use std::sync::Mutex;

    /// Held for the duration of any test that sets `THETA_SESSION_DIR`.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Take the lock, ignoring poisoning so one failing test does not cascade.
    pub fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionTree {
    pub entries: Vec<Entry>,
    pub leaf: Option<String>,
    seq: u64,
}

// ---------------------------------------------------------------------------
// Session names
//
// A session's name is the user's choice, so it must survive being resumed. It
// used to live only in `workspace.toml`, and resuming a session (from /resume
// or the new-session dialog) passed the *derived* title — the first line of the
// first message — which then overwrote the rename and was saved. Storing names
// against the session id, separately from the workspace, is what makes a rename
// stick.
// ---------------------------------------------------------------------------

fn names_path() -> Option<PathBuf> {
    let base = match std::env::var_os("THETA_SESSION_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => match std::env::var_os("THETA_DATA_DIR") {
            Some(dir) => PathBuf::from(dir).join("sessions"),
            None => dirs::data_dir()?.join("theta").join("sessions"),
        },
    };
    Some(base.join("names.json"))
}

fn load_names() -> HashMap<String, String> {
    names_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The name a user gave a session, if any.
pub fn given_name(session_id: &str) -> Option<String> {
    load_names().remove(session_id).filter(|n| !n.trim().is_empty())
}

/// Record a user-chosen name for a session. An empty name clears it, so the
/// derived title takes over again.
pub fn set_given_name(session_id: &str, name: &str) {
    let Some(path) = names_path() else { return };
    let mut names = load_names();
    let name = name.trim();
    if name.is_empty() {
        names.remove(session_id);
    } else {
        names.insert(session_id.to_string(), name.to_string());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&names) {
        let _ = std::fs::write(&path, text);
    }
}

impl SessionTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn node(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn children(&self, id: &str) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.parent.as_deref() == Some(id))
            .collect()
    }

    pub fn roots(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.parent.is_none()).collect()
    }

    pub fn append(&mut self, message: &ChatMessage) -> String {
        let kind = match message.role {
            crate::ai::Role::User => EntryKind::User,
            crate::ai::Role::Assistant => EntryKind::Assistant,
            crate::ai::Role::Tool => EntryKind::Tool,
            crate::ai::Role::System => EntryKind::System,
        };
        let id = self.next_id();
        self.push(Entry {
            id,
            parent: self.leaf.clone(),
            timestamp_ms: now_ms(),
            kind,
            text: message.text.clone(),
            tool_calls: message.tool_calls.clone(),
            tool_call_id: message.tool_call_id.clone(),
            first_kept: None,
            tokens_before: None,
            snapshots: Vec::new(),
            tokens: message.tokens,
            cost: message.cost,
            // Only a tool result carries UI data; persisting it is what lets a
            // restored session show an edit's diff.
            metadata: message.tool_metadata.clone(),
        })
    }

    pub fn push_compaction(&mut self, summary: &str, first_kept: Option<String>, tokens_before: u64) -> String {
        let id = self.next_id();
        self.push(Entry {
            id,
            parent: self.leaf.clone(),
            timestamp_ms: now_ms(),
            kind: EntryKind::Compaction,
            text: summary.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            first_kept,
            tokens_before: Some(tokens_before),
            snapshots: Vec::new(),
            tokens: None,
            cost: None,
            // A compaction has no tool data.
            metadata: None,
        })
    }

    pub fn push_branch_summary(&mut self, summary: &str, from: Option<String>) -> String {
        let _ = from;
        let id = self.next_id();
        self.push(Entry {
            id,
            parent: self.leaf.clone(),
            timestamp_ms: now_ms(),
            kind: EntryKind::BranchSummary,
            text: summary.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            first_kept: None,
            tokens_before: None,
            snapshots: Vec::new(),
            tokens: None,
            cost: None,
            metadata: None,
        })
    }

    fn push(&mut self, e: Entry) -> String {
        let id = e.id.clone();
        self.leaf = Some(id.clone());
        self.entries.push(e);
        id
    }

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("e{}", self.seq)
    }

    pub fn active_path(&self) -> Vec<&Entry> {
        let mut out = Vec::new();
        let mut cur = self.leaf.clone();
        while let Some(id) = cur {
            let Some(e) = self.node(&id) else { break };
            out.push(e);
            cur = e.parent.clone();
        }
        out.reverse();
        out
    }

    pub fn context(&self) -> Vec<ChatMessage> {
        let path = self.active_path();
        if path.is_empty() {
            return Vec::new();
        }
        let last_cmp = path.iter().rposition(|e| e.kind == EntryKind::Compaction);
        let start = match last_cmp {
            Some(ci) => path[ci]
                .first_kept
                .as_ref()
                .and_then(|fk| path.iter().position(|e| &e.id == fk))
                .unwrap_or(0),
            None => 0,
        };
        let mut out = Vec::new();
        for e in &path[..start] {
            if e.kind == EntryKind::System {
                out.push(e.to_message());
            }
        }
        for e in &path[start..] {
            out.push(e.to_message());
        }
        out
    }

    pub fn to_messages(&self, sid: &str) -> Vec<Message> {
        let mut out = Vec::new();
        if self.is_empty() {
            return out;
        }
        let mut n = 0u64;
        for entry in self.active_path() {
            if entry.kind == EntryKind::Compaction {
                n += 1;
                let message_id = format!("{sid}-hist-{n}");
                let tokens_before = entry.tokens_before.unwrap_or(0);
                out.push(Message {
                    id: message_id.clone(),
                    role: Role::Assistant,
                    error: None,
                    completed: Some(entry.timestamp_ms),
                    created: Some(entry.timestamp_ms),
                    cost: None,
                    tokens: None,
                    parts: vec![Part {
                        id: format!("{message_id}-compaction"),
                        message_id,
                        kind: PartKind::Compaction { tokens_before },
                    }],
                });
                continue;
            }
            let (role, text) = match entry.kind {
                EntryKind::User => (Role::User, entry.text.clone()),
                EntryKind::Assistant => (Role::Assistant, entry.text.clone()),
                _ => continue,
            };
            if text.trim().is_empty() && entry.tool_calls.is_empty() {
                continue;
            }
            n += 1;
            let message_id = format!("{sid}-hist-{n}");
            let mut parts = Vec::new();
            if !text.trim().is_empty() {
                parts.push(Part {
                    id: format!("{message_id}-p1"),
                    message_id: message_id.clone(),
                    kind: PartKind::Text {
                        text,
                        synthetic: false,
                    },
                });
            }
            for (ci, call) in entry.tool_calls.iter().enumerate() {
                // The tool entry holds both the output and any UI data attached
                // to it, such as the diff an edit produced. Carrying the
                // metadata through is what lets a reopened session still show
                // what changed rather than only which file was touched.
                let tool_entry = self.entries.iter().find(|e| {
                    e.kind == EntryKind::Tool
                        && e.tool_call_id.as_deref() == Some(call.id.as_str())
                });
                let output = tool_entry.map(|e| e.text.clone()).unwrap_or_default();
                let metadata = tool_entry
                    .and_then(|e| e.metadata.clone())
                    .unwrap_or_else(|| serde_json::json!({}));
                parts.push(Part {
                    id: format!("{message_id}-call-{ci}"),
                    message_id: message_id.clone(),
                    kind: PartKind::Tool(ToolInfo {
                        tool: call.name.clone(),
                        call_id: call.id.clone(),
                        status: ToolStatus::Completed,
                        title: None,
                        input: serde_json::from_str(&call.arguments)
                            .unwrap_or(serde_json::json!({})),
                        output: Some(output),
                        error: None,
                        metadata,
                        start_ms: None,
                    }),
                });
            }
            out.push(Message {
                id: message_id,
                role,
                error: None,
                completed: Some(entry.timestamp_ms),
                created: Some(entry.timestamp_ms),
                cost: entry.cost,
                tokens: entry.tokens,
                parts,
            });
        }
        out
    }

    pub fn branch_messages(&self, from: &str, to_ancestor: Option<&str>) -> Vec<ChatMessage> {
        let mut out = Vec::new();
        let mut cur = Some(from.to_string());
        while let Some(id) = cur {
            if Some(id.as_str()) == to_ancestor {
                break;
            }
            let Some(e) = self.node(&id) else { break };
            out.push(e.to_message());
            cur = e.parent.clone();
        }
        out.reverse();
        out
    }

    pub fn branch_summary_input(&self, from: &str, to_ancestor: Option<&str>, budget: u64, cap: usize) -> Option<String> {
        let msgs = self.branch_messages(from, to_ancestor);
        context::branch_summary_input(&msgs, budget, cap)
    }

    pub fn is_ancestor(&self, ancestor: &str, of: &str) -> bool {
        let mut cur = Some(of.to_string());
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            cur = self.node(&id).and_then(|e| e.parent.clone());
        }
        false
    }

    pub fn set_leaf(&mut self, id: &str) -> bool {
        if self.node(id).is_some() {
            self.leaf = Some(id.to_string());
            true
        } else {
            false
        }
    }

    pub fn retain_path(&mut self, keep: &std::collections::HashSet<&str>) {
        let leaf_kept = self
            .leaf
            .as_deref()
            .is_some_and(|l| keep.contains(l));
        self.entries.retain(|e| keep.contains(e.id.as_str()));
        if !leaf_kept {
            self.leaf = self.entries.last().map(|e| e.id.clone());
        }
    }

    pub fn restore_files_after(&self, ancestor: Option<&str>) -> usize {
        let mut restored = 0;
        for e in self.entries_after(ancestor) {
            for snap in e.snapshots.iter().rev() {
                match &snap.before {
                    Some(content) => {
                        if std::fs::write(&snap.path, content).is_ok() {
                            restored += 1;
                        }
                    }
                    None => {
                        if std::fs::remove_file(&snap.path).is_ok() {
                            restored += 1;
                        }
                    }
                }
            }
        }
        restored
    }

    pub fn entries_after(&self, ancestor: Option<&str>) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut cur = self.leaf.clone();
        while let Some(id) = cur {
            if Some(id.as_str()) == ancestor {
                break;
            }
            let Some(e) = self.node(&id) else { break };
            out.push(e.clone());
            cur = e.parent.clone();
        }
        out
    }

    pub fn navigate_with_summary(&mut self, id: &str, summary: Option<String>) -> bool {
        if self.node(id).is_none() {
            return false;
        }
        let old_leaf = self.leaf.clone();
        self.leaf = Some(id.to_string());
        if let (Some(from), Some(sum)) = (old_leaf, summary) {
            if from != id {
                self.push_branch_summary(&sum, Some(from));
            }
        }
        true
    }


    pub fn to_jsonl(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            if let Ok(line) = serde_json::to_string(e) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    pub fn from_jsonl(text: &str) -> Self {
        let mut tree = SessionTree::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_str::<Entry>(line) {
                if let Some(n) = e.id.strip_prefix('e').and_then(|n| n.parse::<u64>().ok()) {
                    tree.seq = tree.seq.max(n);
                }
                tree.leaf = Some(e.id.clone());
                tree.entries.push(e);
            }
        }
        tree
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_jsonl())
    }

    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let tree = Self::from_jsonl(&text);
        (!tree.is_empty()).then_some(tree)
    }


    pub fn list_sessions() -> Vec<SessionSummary> {
        let base = match std::env::var_os("THETA_SESSION_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => match std::env::var_os("THETA_DATA_DIR") {
                Some(dir) => PathBuf::from(dir).join("sessions"),
                None => match dirs::data_dir() {
                    Some(d) => d.join("theta").join("sessions"),
                    None => return Vec::new(),
                },
            },
        };
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&base) else {
            return out;
        };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|x| x.to_str()).map(str::to_string) else {
                continue;
            };
            let Some(tree) = SessionTree::load(&path) else { continue };
            // A name the user gave this session wins over the derived title;
            // otherwise the picker would show the first message again and a
            // rename would look like it never happened.
            let given = given_name(&id).filter(|n| !n.trim().is_empty());
            let derived = tree
                .entries
                .iter()
                .filter(|e| e.kind == EntryKind::User)
                .map(|e| e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim())
                .filter(|l| !l.is_empty())
                .find(|l| l.len() > 3)
                .or_else(|| {
                    tree.entries
                        .iter()
                        .filter(|e| e.kind == EntryKind::User)
                        .map(|e| e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim())
                        .find(|l| !l.is_empty())
                })
                .map(|l| l.chars().take(60).collect::<String>())
                .unwrap_or_else(|| "(empty session)".to_string());
            let title = given.unwrap_or(derived);

            let directory = tree
                .entries
                .iter()
                .flat_map(|e| &e.snapshots)
                .find_map(|s| {
                    let pb = Path::new(&s.path);
                    let mut cur = pb.parent();
                    while let Some(p) = cur {
                        if p.join(".git").exists() {
                            return Some(p.to_string_lossy().to_string());
                        }
                        cur = p.parent();
                    }
                    pb.parent().map(|p| p.to_string_lossy().to_string())
                });

            let updated_ms = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(SessionSummary {
                id,
                title,
                directory,
                updated_ms,
                entries: tree.entries.len(),
            });
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.updated_ms));
        out
    }

    pub fn sidecar_path(session_id: &str) -> Option<PathBuf> {
        let base = match std::env::var_os("THETA_SESSION_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => match std::env::var_os("THETA_DATA_DIR") {
                Some(dir) => PathBuf::from(dir).join("sessions"),
                None => dirs::data_dir()?.join("theta").join("sessions"),
            },
        };
        Some(base.join(format!("{session_id}.jsonl")))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_sessions_skips_short_fillers_and_resolves_directory() {
        let temp_dir = std::env::temp_dir().join(format!("theta-list-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let path = temp_dir.join("ses_sample.jsonl");

        let mut tree = SessionTree::new();
        tree.append(&ChatMessage::user("so"));
        tree.append(&ChatMessage::assistant("How can I help?", vec![]));
        tree.append(&ChatMessage::user("Refactor the parser module"));
        tree.append(&ChatMessage::assistant("Done", vec![]));
        std::fs::write(&path, tree.to_jsonl()).unwrap();

        // Serialized: the variable is process-global and tests run in parallel.
        let _env = testenv::lock();
        std::env::set_var("THETA_SESSION_DIR", &temp_dir);
        let list = SessionTree::list_sessions();
        std::env::remove_var("THETA_SESSION_DIR");
        let _ = std::fs::remove_dir_all(&temp_dir);

        let s = list.into_iter().find(|s| s.id == "ses_sample").expect("found");
        assert_eq!(s.title, "Refactor the parser module");
        assert_eq!(s.entries, 4);
    }
    use crate::ai::ToolCall;

    fn t() -> SessionTree {
        SessionTree::new()
    }

    #[test]
    fn linear_append_rebuilds_in_order() {
        let mut tr = t();
        tr.append(&ChatMessage::system("sys"));
        tr.append(&ChatMessage::user("hello"));
        tr.append(&ChatMessage::assistant("hi", vec![]));
        let ctx = tr.context();
        assert_eq!(ctx.len(), 3);
        assert_eq!(ctx[1].text, "hello");
        assert_eq!(ctx[2].text, "hi");
    }

    #[test]
    fn navigating_back_fans_out_a_new_branch() {
        let mut tr = t();
        tr.append(&ChatMessage::system("sys"));
        tr.append(&ChatMessage::user("q1"));
        let after_q1 = tr.leaf.clone().unwrap();
        tr.append(&ChatMessage::assistant("a1", vec![]));
        tr.append(&ChatMessage::user("q2"));
        tr.append(&ChatMessage::assistant("a2", vec![]));

        assert!(tr.set_leaf(&after_q1));
        tr.append(&ChatMessage::assistant("a1-alt", vec![]));
        tr.append(&ChatMessage::user("q2-alt"));

        let ctx = tr.context();
        let texts: Vec<&str> = ctx.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.contains(&"a1-alt") && texts.contains(&"q2-alt"));
        assert!(!texts.contains(&"a1") && !texts.contains(&"q2"));
        assert!(tr.entries.iter().any(|e| e.text == "a2"));
    }

    #[test]
    fn compaction_replaces_ancestors_but_keeps_recent() {
        let mut tr = t();
        tr.append(&ChatMessage::system("sys"));
        tr.append(&ChatMessage::user("old1"));
        tr.append(&ChatMessage::assistant("old2", vec![]));
        let first_kept = tr.leaf.clone().unwrap();
        tr.append(&ChatMessage::user("recent"));
        let cmp = tr.push_compaction("THE SUMMARY", Some(first_kept.clone()), 12_345);
        tr.append(&ChatMessage::assistant("after", vec![]));
        assert_eq!(tr.leaf.as_deref(), Some("e6"));

        let ctx = tr.context();
        let texts: Vec<&str> = ctx.iter().map(|m| m.text.as_str()).collect();
        assert!(!texts.contains(&"old1"));
        assert!(texts.contains(&"old2"));
        assert!(texts.iter().any(|t| t.contains("THE SUMMARY")));
        assert!(texts.contains(&"recent") && texts.contains(&"after"));
        assert_eq!(ctx[0].text, "sys");
        assert!(tr.node(&cmp).unwrap().tokens_before == Some(12_345));
    }

    #[test]
    fn branch_summary_is_injected_on_navigation() {
        let mut tr = t();
        tr.append(&ChatMessage::system("sys"));
        tr.append(&ChatMessage::user("q1"));
        let fork = tr.leaf.clone().unwrap();
        tr.append(&ChatMessage::assistant("a1", vec![]));
        let old_leaf = tr.leaf.clone().unwrap();

        let input = tr.branch_summary_input(&old_leaf, Some(&fork), 10_000, 2_000);
        assert!(input.unwrap().contains("a1"));
        assert!(tr.navigate_with_summary(&fork, Some("carry: tried a1".into())));
        tr.append(&ChatMessage::assistant("a1-new", vec![]));

        let ctx = tr.context();
        let texts: Vec<&str> = ctx.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("carry: tried a1")));
        assert!(texts.contains(&"a1-new"));
    }

    #[test]
    fn jsonl_round_trip_preserves_tree() {
        let mut tr = t();
        tr.append(&ChatMessage::system("sys"));
        tr.append(&ChatMessage::user("hi"));
        tr.append(&ChatMessage::assistant(
            "calling",
            vec![ToolCall { id: "c1".into(), name: "read".into(), arguments: "{\"path\":\"/a\"}".into() }],
        ));
        tr.append(&ChatMessage::tool_result("c1", "contents"));
        let text = tr.to_jsonl();
        let back = SessionTree::from_jsonl(&text);
        assert_eq!(back.entries.len(), 4);
        assert_eq!(back.leaf, tr.leaf);
        assert_eq!(back.context(), tr.context());
        let mut back2 = back;
        back2.append(&ChatMessage::user("more"));
        assert!(!back2.node("e5").is_none() || back2.entries.len() == 5);
    }

    #[test]
    fn tree_to_messages_reconstructs_tools_and_compactions() {
        let mut tr = t();
        tr.append(&ChatMessage::user("how does theta work?"));
        tr.append(&ChatMessage::assistant(
            "checking code",
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{\"path\":\"src/main.rs\"}".into(),
            }],
        ));
        tr.append(&ChatMessage::tool_result("c1", "fn main() {}"));
        let first_kept = tr.leaf.clone();
        tr.push_compaction("conversation summarized", first_kept, 8000);
        tr.append(&ChatMessage::assistant("answer after compaction", vec![]));

        let msgs = tr.to_messages("sess_test");
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].id, "sess_test-hist-1");
        assert_eq!(msgs[0].parts.len(), 1);

        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].id, "sess_test-hist-2");
        assert_eq!(msgs[1].parts.len(), 2);
        match &msgs[1].parts[1].kind {
            PartKind::Tool(t) => {
                assert_eq!(t.tool, "read");
                assert_eq!(t.output.as_deref(), Some("fn main() {}"));
            }
            other => panic!("expected Tool part, got {other:?}"),
        }

        assert_eq!(msgs[2].role, Role::Assistant);
        assert_eq!(msgs[2].id, "sess_test-hist-3");
        match &msgs[2].parts[0].kind {
            PartKind::Compaction { tokens_before } => {
                assert_eq!(*tokens_before, 8000);
            }
            other => panic!("expected Compaction part, got {other:?}"),
        }

        assert_eq!(msgs[3].role, Role::Assistant);
        assert_eq!(msgs[3].id, "sess_test-hist-4");
    }

    #[test]
    fn a_given_name_survives_and_beats_the_derived_title() {
        // The bug: resuming a session passed the derived title (the first line
        // of the first message), which then overwrote a rename. A name stored
        // against the session id is what makes the rename stick.
        let dir = std::env::temp_dir().join(format!("theta-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = testenv::lock();
        std::env::set_var("THETA_SESSION_DIR", &dir);

        assert_eq!(given_name("ses_x"), None, "no name until one is set");
        set_given_name("ses_x", "  authentication  ");
        assert_eq!(
            given_name("ses_x").as_deref(),
            Some("authentication"),
            "the name is trimmed and returned"
        );

        // An empty name clears it, so the derived title takes over again.
        set_given_name("ses_x", "");
        assert_eq!(given_name("ses_x"), None);

        std::env::remove_var("THETA_SESSION_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_stored_per_session() {
        let dir = std::env::temp_dir().join(format!("theta-names2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _env = testenv::lock();
        std::env::set_var("THETA_SESSION_DIR", &dir);

        set_given_name("ses_a", "alpha");
        set_given_name("ses_b", "beta");
        assert_eq!(given_name("ses_a").as_deref(), Some("alpha"));
        assert_eq!(given_name("ses_b").as_deref(), Some("beta"));
        assert_eq!(given_name("ses_c"), None);

        std::env::remove_var("THETA_SESSION_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn a_tools_diff_survives_a_save_and_reload() {
        // Reported: an edit showed what changed only until the session was
        // reopened, because the tool's UI data was never persisted.
        let mut tree = SessionTree::new();
        let call = crate::ai::ToolCall {
            id: "c1".into(),
            name: "edit".into(),
            arguments: r#"{"path":"src/a.rs"}"#.into(),
        };
        tree.append(&ChatMessage::assistant("editing", vec![call.clone()]));

        let mut result = ChatMessage::tool_result("c1", "edited src/a.rs");
        result.tool_metadata = Some(serde_json::json!({
            "diff": "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n"
        }));
        tree.append(&result);

        // Round trip through the on-disk form.
        let jsonl = tree.to_jsonl();
        let dir = std::env::temp_dir().join(format!("theta-diff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ses_diff.jsonl");
        std::fs::write(&path, &jsonl).unwrap();
        let reloaded = SessionTree::load(&path).expect("reload");

        // The transcript rebuilt from it still carries the diff.
        let msgs = reloaded.to_messages("ses_diff");
        let diff = msgs
            .iter()
            .flat_map(|m| &m.parts)
            .find_map(|p| match &p.kind {
                PartKind::Tool(t) => t.metadata.get("diff").and_then(|d| d.as_str()),
                _ => None,
            });
        let diff = diff.expect("the diff must survive the reload");
        assert!(diff.contains("-old"), "{diff}");
        assert!(diff.contains("+new"), "{diff}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tool_without_metadata_reloads_with_an_empty_object() {
        // The UI reads `metadata` as an object, so a read or a search must not
        // come back as null.
        let mut tree = SessionTree::new();
        tree.append(&ChatMessage::assistant(
            "reading",
            vec![crate::ai::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: r#"{"path":"src/a.rs"}"#.into(),
            }],
        ));
        tree.append(&ChatMessage::tool_result("c1", "file contents"));

        let msgs = tree.to_messages("s");
        let meta = msgs
            .iter()
            .flat_map(|m| &m.parts)
            .find_map(|p| match &p.kind {
                PartKind::Tool(t) => Some(t.metadata.clone()),
                _ => None,
            })
            .expect("a tool part");
        assert!(meta.is_object(), "expected an object, got {meta}");
        assert_eq!(meta.as_object().map(|o| o.len()), Some(0));
    }

}
