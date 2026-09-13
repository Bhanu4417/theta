//! Per-session state: transcript, input buffer, status, scroll.

use crate::opencode::{Message, ModelRef, Part, PartKind, Role, ToolInfo, ToolStatus};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum SessStatus {
    Connecting,
    Idle,
    Working,
    Thinking,
    Retrying(String),
    Error(String),
    Permission,
}

impl SessStatus {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            SessStatus::Working | SessStatus::Thinking | SessStatus::Retrying(_)
        )
    }
}

#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub id: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub buf: String,
    /// Cursor position in *chars* from the start of the buffer.
    pub cursor: usize,
    pub history: Vec<String>,
    pub hist_idx: Option<usize>,
    /// Draft saved when browsing history.
    draft: Option<String>,
}

impl InputState {
    pub fn insert(&mut self, text: &str) {
        let byte = self.byte_cursor();
        self.buf.insert_str(byte, text);
        self.cursor += text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte = self.byte_at_char(self.cursor - 1);
        let end = self.byte_at_char(self.cursor);
        self.buf.replace_range(byte..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        let end = self.byte_at_char(self.cursor + 1);
        let byte = self.byte_cursor();
        if end > byte && end <= self.buf.len() {
            self.buf.replace_range(byte..end, "");
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        let len = self.buf.chars().count();
        if self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buf.chars().count();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
        self.hist_idx = None;
        self.draft = None;
    }

    pub fn text(&self) -> &str {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.trim().is_empty()
    }

    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.buf);
        self.cursor = 0;
        self.hist_idx = None;
        self.draft = None;
        t.trim().to_string()
    }

    pub fn push_history(&mut self, entry: &str, limit: usize) {
        if entry.trim().is_empty() {
            return;
        }
        if self.history.last().map(|h| h == entry).unwrap_or(false) {
            return;
        }
        self.history.push(entry.to_string());
        if self.history.len() > limit {
            self.history.remove(0);
        }
    }

    pub fn hist_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.hist_idx {
            None => {
                self.draft = Some(self.buf.clone());
                self.history.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.hist_idx = Some(idx);
        self.buf = self.history[idx].clone();
        self.cursor = self.buf.chars().count();
    }

    pub fn hist_down(&mut self) {
        match self.hist_idx {
            None => {}
            Some(i) => {
                if i + 1 >= self.history.len() {
                    self.buf = self.draft.take().unwrap_or_default();
                    self.hist_idx = None;
                    self.cursor = self.buf.chars().count();
                } else {
                    self.hist_idx = Some(i + 1);
                    self.buf = self.history[i + 1].clone();
                    self.cursor = self.buf.chars().count();
                }
            }
        }
    }

    fn byte_cursor(&self) -> usize {
        self.byte_at_char(self.cursor)
    }

    fn byte_at_char(&self, n: usize) -> usize {
        self.buf
            .char_indices()
            .nth(n)
            .map(|(b, _)| b)
            .unwrap_or(self.buf.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolRef {
    /// Index into `messages`.
    pub msg: usize,
    /// Index into `messages[msg].parts`.
    pub part: usize,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub id: u32,
    pub name: String,
    pub dir: PathBuf,
    pub oc_sid: Option<String>,
    pub model: Option<ModelRef>,
    /// Agent used for subsequent prompts (e.g. "build", "plan").
    pub agent: Option<String>,
    /// Selected row in the slash-command popup.
    pub slash_selected: usize,
    /// Share URL when the session is shared.
    pub share_url: Option<String>,
    /// Last submission (time, text) — guards double-Enter duplicates.
    pub last_send: Option<(std::time::Instant, String)>,
    /// Cumulative assistant cost in USD (recomputed from the transcript).
    pub cost: f64,
    /// Prompt-side tokens of the latest assistant message (context size).
    pub ctx_tokens: u64,
    pub messages: Vec<Message>,
    pub status: SessStatus,
    pub input: InputState,
    /// Scroll offset in rendered lines from the top.
    pub scroll: usize,
    pub stick_bottom: bool,
    /// Tool part ids that are expanded.
    pub expanded: HashSet<String>,
    /// Selected tool for Enter/expand interactions.
    pub tool_cursor: Option<ToolRef>,
    pub pending_perm: Option<PendingPermission>,
    pub last_error: Option<String>,
    /// Counter used to derive unique optimistic message ids.
    optimistic_seq: u64,
    /// Optimistic user messages not yet adopted by a real server message.
    unadopted_locals: u64,
    /// Invalidate the rendered-line cache.
    pub dirty: bool,
}

impl SessionState {
    pub fn new(id: u32, name: String, dir: PathBuf) -> Self {
        Self {
            id,
            name,
            dir,
            oc_sid: None,
            model: None,
            agent: None,
            slash_selected: 0,
            share_url: None,
            last_send: None,
            cost: 0.0,
            ctx_tokens: 0,
            messages: Vec::new(),
            status: SessStatus::Connecting,
            input: InputState::default(),
            scroll: 0,
            stick_bottom: true,
            expanded: HashSet::new(),
            tool_cursor: None,
            pending_perm: None,
            last_error: None,
            optimistic_seq: 0,
            unadopted_locals: 0,
            dirty: true,
        }
    }

    pub fn title(&self) -> &str {
        &self.name
    }

    /// Optimistic local user message shown before the server acknowledges.
    pub fn push_local_user(&mut self, text: &str) -> String {
        self.optimistic_seq += 1;
        self.unadopted_locals += 1;
        let local_id = format!("local-{}", self.optimistic_seq);
        let msg = Message {
            id: local_id.clone(),
            role: Role::User,
            error: None,
            completed: None,
            created: Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            ),
            cost: None,
            tokens: None,
            parts: vec![Part {
                id: format!("{local_id}-p"),
                message_id: local_id.clone(),
                kind: PartKind::Text {
                    text: text.to_string(),
                    synthetic: false,
                },
            }],
        };
        self.messages.push(msg);
        self.dirty = true;
        local_id
    }

    /// Adopt a real server message in place of an optimistic one with
    /// matching first-text content. Returns true when adopted.
    pub fn adopt_by_text(&mut self, real: &Message, text: &str) -> bool {
        if self.unadopted_locals == 0 {
            return false;
        }
        let pos = self.messages.iter().position(|m| {
            m.id.starts_with("local-")
                && m.role == Role::User
                && m.parts.iter().any(|p| match &p.kind {
                    PartKind::Text { text: t, .. } => t == text,
                    _ => false,
                })
        });
        match pos {
            Some(i) => {
                let mut adopted = real.clone();
                adopted.parts = std::mem::take(&mut self.messages[i].parts);
                self.messages[i] = adopted;
                self.unadopted_locals -= 1;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Adopt the oldest optimistic message (FIFO) when no text is available
    /// to match on (message.updated carries no parts).
    pub fn adopt_oldest(&mut self, real: &Message) -> bool {
        if self.unadopted_locals == 0 {
            return false;
        }
        let pos = self
            .messages
            .iter()
            .position(|m| m.id.starts_with("local-") && m.role == Role::User);
        match pos {
            Some(i) => {
                let mut adopted = real.clone();
                adopted.parts = std::mem::take(&mut self.messages[i].parts);
                self.messages[i] = adopted;
                self.unadopted_locals -= 1;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Insert or update a message meta row (from message.updated).
    pub fn upsert_message_meta(&mut self, msg: &Message) {
        if let Some(existing) = self.messages.iter_mut().find(|m| m.id == msg.id) {
            existing.role = msg.role;
            existing.error = msg.error.clone();
            existing.completed = msg.completed;
            existing.cost = msg.cost;
            existing.tokens = msg.tokens;
            self.recompute_metrics();
            self.dirty = true;
        }
    }

    /// Session cost = sum of assistant message costs; context = the latest
    /// assistant message's prompt-side tokens.
    pub fn recompute_metrics(&mut self) {
        self.cost = self
            .messages
            .iter()
            .filter_map(|m| if m.role == Role::Assistant { m.cost } else { None })
            .sum();
        self.ctx_tokens = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| m.tokens)
            .map(|t| t.context())
            .unwrap_or(0);
    }

    /// Insert or replace a part, creating the message row if needed.
    pub fn upsert_part(&mut self, message: &Message, part: Part) {
        let dirty_msg_id = message.id.clone();
        if !self.messages.iter().any(|m| m.id == dirty_msg_id) {
            // A text part may be the first evidence of a real user message
            // we predicted optimistically — adopt instead of duplicating.
            if let PartKind::Text { text, .. } = &part.kind {
                if self.adopt_by_text(message, text) {
                    self.upsert_part_existing(message, part);
                    return;
                }
            }
            let mut meta = message.clone();
            meta.parts.clear();
            self.messages.push(meta);
        }
        self.upsert_part_existing(message, part);
    }

    fn upsert_part_existing(&mut self, message: &Message, part: Part) {
        let dirty_msg_id = message.id.clone();
        {
            let Some(msg) = self.messages.iter_mut().find(|m| m.id == dirty_msg_id) else {
                return;
            };
            msg.role = message.role;
            msg.error = message.error.clone();
            msg.completed = message.completed;
            msg.cost = message.cost;
            msg.tokens = message.tokens;
            // An adopted message still carries placeholder local parts; purge
            // them once the server delivers its own.
            if !part.id.starts_with("local-")
                && msg.parts.iter().any(|p| p.id.starts_with("local-"))
            {
                msg.parts.retain(|p| !p.id.starts_with("local-"));
            }
            if let Some(existing) = msg.parts.iter_mut().find(|p| p.id == part.id) {
                *existing = part;
            } else {
                msg.parts.push(part);
            }
        }
        self.recompute_metrics();
        self.dirty = true;
    }

    pub fn remove_part(&mut self, message_id: &str, part_id: &str) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.id == message_id) {
            m.parts.retain(|p| p.id != part_id);
            self.dirty = true;
        }
    }

    pub fn replace_history(&mut self, mut msgs: Vec<Message>) {
        msgs.retain(|m| {
            m.parts
                .iter()
                .any(|p| !matches!(&p.kind, PartKind::Text { synthetic: true, .. }))
                || m.role == Role::User
        });
        self.messages = msgs;
        self.unadopted_locals = 0;
        self.dirty = true;
    }

    /// Flat list of tool parts in transcript order.
    pub fn tools(&self) -> Vec<ToolRef> {
        let mut out = Vec::new();
        for (mi, m) in self.messages.iter().enumerate() {
            for (pi, p) in m.parts.iter().enumerate() {
                if matches!(p.kind, PartKind::Tool(_)) {
                    out.push(ToolRef { msg: mi, part: pi });
                }
            }
        }
        out
    }

    pub fn tool_at(&self, r: ToolRef) -> Option<&ToolInfo> {
        self.messages
            .get(r.msg)
            .and_then(|m| m.parts.get(r.part))
            .and_then(|p| match &p.kind {
                PartKind::Tool(t) => Some(t),
                _ => None,
            })
    }

    pub fn toggle_expanded(&mut self, r: ToolRef) {
        let Some(id) = self
            .messages
            .get(r.msg)
            .and_then(|m| m.parts.get(r.part))
            .map(|p| p.id.clone())
        else {
            return;
        };
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.dirty = true;
    }

    pub fn is_expanded(&self, part_id: &str) -> bool {
        self.expanded.contains(part_id)
    }

    pub fn any_tool_running(&self) -> bool {
        self.messages.iter().any(|m| {
            m.parts.iter().any(|p| match &p.kind {
                PartKind::Tool(t) => matches!(t.status, ToolStatus::Pending | ToolStatus::Running),
                _ => false,
            })
        })
    }

    pub fn last_assistant_unfinished(&self) -> bool {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.completed.is_none())
            .unwrap_or(false)
    }
}
