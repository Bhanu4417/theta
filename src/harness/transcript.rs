//! Provider-neutral transcript model.
//!
//! These are the normalized shapes the UI renders. They are produced by the
//! active provider adapter (e.g. [`crate::providers::opencode`]) from native
//! events, so the application never handles provider protocol directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Pending,
    Running,
    Completed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub tool: String,
    pub call_id: String,
    pub status: ToolStatus,
    pub title: Option<String>,
    pub input: Value,
    pub output: Option<String>,
    pub error: Option<String>,
    pub metadata: Value,
    /// Epoch millis when the tool started (for elapsed-time progress fallback).
    pub start_ms: Option<i64>,
}

impl ToolInfo {
    pub fn meta_str(&self, keys: &[&str]) -> Option<String> {
        for k in keys {
            if let Some(s) = self.metadata.get(*k).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
        None
    }

    pub fn input_str(&self, keys: &[&str]) -> Option<String> {
        for k in keys {
            if let Some(s) = self.input.get(*k).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
        None
    }

    /// Human readable one-liner, preferring the server-provided title.
    pub fn display_title(&self) -> String {
        if let Some(t) = self.title.as_deref() {
            if !t.trim().is_empty() {
                return t.to_string();
            }
        }
        match self.tool.as_str() {
            "bash" => self
                .input_str(&["command", "cmd"])
                .map(|c| format!("$ {c}"))
                .unwrap_or_else(|| "shell".into()),
            "read" => self
                .meta_str(&["filePath", "file_path", "path"])
                .or_else(|| self.input_str(&["filePath", "file_path", "path"]))
                .map(|f| format!("Reading {f}"))
                .unwrap_or_else(|| "read".into()),
            "edit" | "write" | "multiedit" | "patch" => self
                .meta_str(&["filePath", "file_path", "path"])
                .or_else(|| self.input_str(&["filePath", "file_path", "path"]))
                .map(|f| format!("Editing {f}"))
                .unwrap_or_else(|| "edit".into()),
            "grep" => self
                .input_str(&["pattern"])
                .map(|p| format!("Searching \"{p}\""))
                .unwrap_or_else(|| "search".into()),
            "glob" => self
                .input_str(&["pattern"])
                .map(|p| format!("Finding {p}"))
                .unwrap_or_else(|| "glob".into()),
            "webfetch" => self
                .input_str(&["url"])
                .map(|u| format!("Fetching {u}"))
                .unwrap_or_else(|| "webfetch".into()),
            "task" => self
                .input_str(&["description", "prompt"])
                .map(|d| format!("Agent: {d}"))
                .unwrap_or_else(|| "agent".into()),
            "todowrite" | "todoread" => "Updating plan".into(),
            other => other.to_string(),
        }
    }

    /// Unified diff text when the tool reported one (edit tools).
    pub fn diff(&self) -> Option<String> {
        self.meta_str(&["diff", "patch"])
            .filter(|d| !d.trim().is_empty())
    }

    /// File path touched by the tool, when applicable.
    pub fn file_path(&self) -> Option<String> {
        self.meta_str(&["filePath", "file_path", "path"])
            .or_else(|| self.input_str(&["filePath", "file_path", "path"]))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PartKind {
    Text {
        text: String,
        synthetic: bool,
    },
    Reasoning {
        text: String,
        running: bool,
        start: Option<i64>,
        end: Option<i64>,
    },
    Tool(ToolInfo),
    StepStart,
    StepFinish,
    /// A context-compaction boundary. Rendered as a centered divider; the
    /// summary itself is kept in the model prompt, not the transcript.
    Compaction {
        tokens_before: u64,
    },
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub message_id: String,
    pub kind: PartKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenUsage {
    /// Approximate prompt-side context size for the next request.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.reasoning
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub error: Option<String>,
    pub completed: Option<i64>,
    pub created: Option<i64>,
    pub cost: Option<f64>,
    pub tokens: Option<TokenUsage>,
    pub parts: Vec<Part>,
}

/// An incremental transcript change produced by a provider adapter and applied
/// to the Theta session transcript. Streaming is preserved because partial
/// parts arrive as repeated `Part` updates.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TranscriptUpdate {
    /// Message metadata (from a `message.updated`-style native event).
    MessageMeta(Message),
    /// A part was created or updated (text delta, tool state, reasoning).
    Part(Part),
    /// A part was removed.
    PartRemoved { message_id: String, part_id: String },
}
