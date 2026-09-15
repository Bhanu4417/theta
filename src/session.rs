//! Per-session state: transcript, input buffer, status, scroll.

use crate::harness::transcript::{Message, Part, PartKind, Role, ToolInfo, ToolStatus};
use crate::harness::{Question, Task};
use crate::opencode::ModelRef;
use crate::providers::ProviderKind;
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
    Question,
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

/// A question the agent is waiting on, plus the UI selection state.
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub id: String,
    pub questions: Vec<Question>,
    /// Index of the question currently being answered.
    pub qi: usize,
    /// Highlighted option per question.
    pub selected: Vec<usize>,
    /// Toggled options per question (for multi-select).
    pub chosen: Vec<Vec<bool>>,
    /// Accumulated answers per question (labels).
    pub answers: Vec<Vec<String>>,
    /// Free-text answer for the current question when `custom` is allowed.
    pub custom: String,
}

impl PendingQuestion {
    pub fn new(id: String, questions: Vec<Question>) -> Self {
        let n = questions.len();
        let selected = vec![0usize; n];
        let chosen = questions
            .iter()
            .map(|q| vec![false; q.options.len()])
            .collect();
        let answers = vec![Vec::new(); n];
        Self {
            id,
            questions,
            qi: 0,
            selected,
            chosen,
            answers,
            custom: String::new(),
        }
    }

    pub fn current(&self) -> Option<&Question> {
        self.questions.get(self.qi)
    }

    pub fn is_last(&self) -> bool {
        self.qi + 1 >= self.questions.len()
    }

    /// Record the answer for the current question and advance. Returns true
    /// once every question has been answered.
    pub fn commit_current(&mut self) -> bool {
        let Some(q) = self.questions.get(self.qi) else {
            return true;
        };
        let answer: Vec<String> = if q.custom && !self.custom.trim().is_empty() {
            vec![self.custom.trim().to_string()]
        } else if q.multiple {
            let picks = self.chosen.get(self.qi).cloned().unwrap_or_default();
            q.options
                .iter()
                .enumerate()
                .filter(|(i, _)| picks.get(*i).copied().unwrap_or(false))
                .map(|(_, o)| o.label.clone())
                .collect()
        } else {
            let sel = self.selected.get(self.qi).copied().unwrap_or(0);
            q.options
                .get(sel)
                .map(|o| vec![o.label.clone()])
                .unwrap_or_default()
        };
        if let Some(a) = self.answers.get_mut(self.qi) {
            *a = answer;
        }
        self.custom.clear();
        if self.is_last() {
            true
        } else {
            self.qi += 1;
            false
        }
    }
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
        self.buf.is_empty()
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

/// A transient status line shown in the workspace activity strip (e.g. a
/// `/push`), with the time it started so it can expire and whether it finished.
#[derive(Debug, Clone)]
pub struct Activity {
    pub text: String,
    pub started: std::time::Instant,
    pub done: bool,
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
    /// Theta-owned identity wrapper (provider ids are metadata).
    pub session_id: crate::harness::SessionId,
    /// Which agent runtime backs this session.
    pub provider: ProviderKind,
    /// The harness task for the current prompt, if any.
    pub task: Option<Task>,
    pub name: String,
    pub dir: PathBuf,
    pub oc_sid: Option<String>,
    pub model: Option<ModelRef>,
    /// Agent used for subsequent prompts (e.g. "build", "plan").
    pub agent: Option<String>,
    /// When set, prompts route through the agy CLI with this model
    /// (Gemini etc.) instead of the OpenCode provider.
    pub agy_model: Option<String>,
    /// Selected row in the slash-command popup.
    pub slash_selected: usize,
    /// Share URL when the session is shared.
    pub share_url: Option<String>,
    /// Last submission (time, text) — guards double-Enter duplicates.
    pub last_send: Option<(std::time::Instant, String)>,
    /// Prompt typed while the agent was busy — awaiting queue/fork choice.
    pub pending_send: Option<String>,
    /// Prompts queued while busy; sent in order when the agent idles.
    pub queue: Vec<String>,
    /// Set when Esc was pressed once while the agent is busy; a second Esc
    /// within a short window interrupts. Cleared on tick.
    pub interrupt_armed: Option<std::time::Instant>,
    /// Short status shown in the workspace activity strip (e.g. a `/push`).
    pub activity: Option<Activity>,
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
    /// A question the agent is waiting on.
    pub pending_question: Option<PendingQuestion>,
    pub last_error: Option<String>,
    /// Counter used to derive unique optimistic message ids.
    optimistic_seq: u64,
    /// Optimistic user messages not yet adopted by a real server message.
    unadopted_locals: u64,
    /// Sequence counter for synthetic (agy) message ids.
    synthetic_seq: u64,
    /// Invalidate the rendered-line cache.
    pub dirty: bool,
}

impl SessionState {
    pub fn new(id: u32, name: String, dir: PathBuf) -> Self {
        Self {
            id,
            name,
            dir,
            session_id: crate::harness::SessionId(id),
            provider: ProviderKind::OpenCode,
            task: None,
            oc_sid: None,
            model: None,
            agy_model: None,
            agent: None,
            slash_selected: 0,
            share_url: None,
            last_send: None,
            pending_send: None,
            queue: Vec::new(),
            interrupt_armed: None,
            activity: None,
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
            pending_question: None,
            last_error: None,
            optimistic_seq: 0,
            unadopted_locals: 0,
            synthetic_seq: 0,
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

    pub fn optimistic_seq(&mut self) -> u64 {
        self.synthetic_seq += 1;
        self.synthetic_seq
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_editing_and_cursor() {
        let mut input = InputState::default();
        input.insert("hello");
        assert_eq!(input.text(), "hello");
        assert_eq!(input.cursor, 5);
        input.left();
        input.left();
        input.insert("X");
        assert_eq!(input.text(), "helXlo");
        assert_eq!(input.cursor, 4);
        input.backspace();
        assert_eq!(input.text(), "hello");
        input.end();
        input.delete();
        assert_eq!(input.text(), "hello");
        input.clear();
        assert!(input.is_empty());
    }

    #[test]
    fn history_recalls_previous_prompts() {
        let mut input = InputState::default();
        input.push_history("first", 10);
        input.push_history("second", 10);
        input.hist_up();
        assert_eq!(input.text(), "second");
        input.hist_up();
        assert_eq!(input.text(), "first");
        input.hist_down();
        assert_eq!(input.text(), "second");
        input.hist_down();
        assert!(input.is_empty(), "descending past the end restores the draft");
    }

    #[test]
    fn optimistic_users_are_adopted_by_matching_text() {
        let mut s = SessionState::new(1, "s".into(), PathBuf::from("."));
        s.push_local_user("do the thing");
        assert_eq!(s.messages.len(), 1);
        assert!(s.messages[0].id.starts_with("local-"));
        let real = Message {
            id: "msg_1".into(),
            role: Role::User,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: Vec::new(),
        };
        assert!(s.adopt_by_text(&real, "do the thing"));
        assert_eq!(s.messages.len(), 1, "adoption replaces, never duplicates");
        assert_eq!(s.messages[0].id, "msg_1");
    }
}
