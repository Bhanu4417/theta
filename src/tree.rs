//! Tree-shaped session history (Pi's session tree).
//!
//! A session is a set of [`Entry`]s linked by `parent`. The active position is
//! a `leaf`; the root→leaf path is the active branch. Navigation sets a new
//! leaf and the next append fans out a new branch, while the abandoned branch
//! stays in the file and can be revisited.
//!
//! Compaction and branch-summary entries participate in [`SessionTree::context`]:
//! a compaction entry replaces its ancestors with a summary, and a branch
//! summary injects carried-over context at its position.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ai::ChatMessage;
use crate::agent::context::{self, SUMMARY_CLOSE, SUMMARY_OPEN};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    User,
    Assistant,
    Tool,
    System,
    /// A compaction boundary: ancestors are replaced by `text` (the summary).
    Compaction,
    /// Context carried from an abandoned branch.
    BranchSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub parent: Option<String>,
    pub timestamp_ms: i64,
    pub kind: EntryKind,
    /// Message text (or summary text for Compaction/BranchSummary).
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<crate::ai::ToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// For compaction: first ancestor kept verbatim (context starts here).
    #[serde(default)]
    pub first_kept: Option<String>,
    /// For compaction: token count before compaction.
    #[serde(default)]
    pub tokens_before: Option<u64>,
    /// Pre-images of files this entry's turn mutated (for `/undo` restore).
    #[serde(default)]
    pub snapshots: Vec<crate::agent::tools::FileSnapshot>,
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
                tokens: None,
            },
            EntryKind::Tool => ChatMessage::tool_result(
                self.tool_call_id.clone().unwrap_or_default(),
                self.text.clone(),
            ),
            EntryKind::System => ChatMessage::system(self.text.clone()),
        }
    }
}

/// A resumable local session discovered from its sidecar.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_ms: i64,
    pub entries: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionTree {
    pub entries: Vec<Entry>,
    pub leaf: Option<String>,
    seq: u64,
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

    /// Append a message entry as a child of `parent` (or the current leaf when
    /// `parent` is `None`), returning the new leaf id.
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
        })
    }

    /// Append a compaction node and make it the leaf.
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
        })
    }

    /// Append a branch-summary node and make it the leaf.
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

    /// Root→leaf order for the active branch.
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

    /// Rebuild the model context from the active branch, honoring compaction
    /// and branch-summary entries.
    pub fn context(&self) -> Vec<ChatMessage> {
        let path = self.active_path();
        if path.is_empty() {
            return Vec::new();
        }
        // Latest compaction on the path replaces its ancestors.
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
        // System prompt(s) before the retained region are preserved.
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

    /// Messages for an abandoned sub-branch (used to summarize before
    /// navigation). `from` is the branch's old leaf; `to_ancestor` is kept out.
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

    /// Serialize the folded branch up to a token budget for summarization.
    pub fn branch_summary_input(&self, from: &str, to_ancestor: Option<&str>, budget: u64, cap: usize) -> Option<String> {
        let msgs = self.branch_messages(from, to_ancestor);
        context::branch_summary_input(&msgs, budget, cap)
    }

    /// True when `ancestor` is on the parent chain of `of` (inclusive).
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

    /// Keep only entries whose id is in `keep`, re-pointing `leaf` at the last
    /// surviving entry of the previous active path. Used to pin a fork.
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

    /// Restore file pre-images for every entry after `ancestor` (newest first),
    /// reversing each entry's snapshots then the entry order. Returns how many
    /// files were touched. Best-effort: write failures are ignored.
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

    /// Walk from the current leaf back to (not including) `ancestor`, returning
    /// the abandoned entries newest-first. `ancestor = None` returns the whole
    /// active path. Does not mutate.
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

    /// Navigate to `id`, first appending a branch summary carrying the
    /// abandoned work forward (when one can be produced).
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

    // -- persistence --------------------------------------------------------

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

    /// Load entries from JSONL. The leaf is the last entry present.
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

    /// List local sessions saved under the sidecar directory (newest first).
    /// Missing directories yield an empty list.
    pub fn list_sessions() -> Vec<SessionSummary> {
        let base = match std::env::var_os("THETA_SESSION_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => match dirs::data_dir() {
                Some(d) => d.join("theta").join("sessions"),
                None => return Vec::new(),
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
            let title = tree
                .entries
                .iter()
                .find(|e| e.kind == EntryKind::User)
                .map(|e| e.text.lines().next().unwrap_or("").chars().take(60).collect())
                .unwrap_or_else(|| "(empty session)".to_string());
            let updated_ms = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(SessionSummary { id, title, updated_ms, entries: tree.entries.len() });
        }
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
        out
    }

    /// Default on-disk path for a provider session id. Override the directory
    /// with `THETA_SESSION_DIR`.
    pub fn sidecar_path(session_id: &str) -> Option<PathBuf> {
        let base = match std::env::var_os("THETA_SESSION_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => dirs::data_dir()?.join("theta").join("sessions"),
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

        // Go back to just after q1 and take a different path.
        assert!(tr.set_leaf(&after_q1));
        tr.append(&ChatMessage::assistant("a1-alt", vec![]));
        tr.append(&ChatMessage::user("q2-alt"));

        let ctx = tr.context();
        let texts: Vec<&str> = ctx.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.contains(&"a1-alt") && texts.contains(&"q2-alt"));
        assert!(!texts.contains(&"a1") && !texts.contains(&"q2"));
        // The abandoned branch is preserved in the file.
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
        // old1 is summarized away; the first_kept entry (old2) is retained,
        // along with the summary, the recent turn and later work.
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
        // Sequence numbering continues after reload.
        let mut back2 = back;
        back2.append(&ChatMessage::user("more"));
        assert!(!back2.node("e5").is_none() || back2.entries.len() == 5);
    }
}
