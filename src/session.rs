use crate::harness::transcript::{Message, Part, PartKind, Role, ToolInfo};
use crate::harness::{Question, Task};
use crate::models::ModelRef;
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
    Compacting,
}

impl SessStatus {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            SessStatus::Working
                | SessStatus::Thinking
                | SessStatus::Retrying(_)
                | SessStatus::Compacting
        )
    }
}

#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub id: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub id: String,
    pub questions: Vec<Question>,
    pub qi: usize,
    pub selected: Vec<usize>,
    pub chosen: Vec<Vec<bool>>,
    pub answers: Vec<Vec<String>>,
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
    pub cursor: usize,
    pub history: Vec<String>,
    pub hist_idx: Option<usize>,
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

#[derive(Debug, Clone)]
pub struct Activity {
    pub text: String,
    pub started: std::time::Instant,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolRef {
    pub msg: usize,
    pub part: usize,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub id: u32,
    pub session_id: crate::harness::SessionId,
    pub provider: ProviderKind,
    pub task: Option<Task>,
    pub name: String,
    pub dir: PathBuf,
    pub oc_sid: Option<String>,
    pub model: Option<ModelRef>,
    pub agent: Option<String>,
    pub slash_selected: usize,
    pub last_send: Option<(std::time::Instant, String)>,
    pub pending_send: Option<String>,
    pub queue: Vec<String>,
    pub interrupt_armed: Option<std::time::Instant>,
    pub activity: Option<Activity>,
    pub cost: f64,
    pub ctx_tokens: u64,
    /// Tokens freed by the most recent prune pass, with the number of tool
    /// results elided. Cleared when the turn ends.
    pub last_prune: Option<(u64, usize)>,
    pub messages: Vec<Message>,
    pub status: SessStatus,
    pub input: InputState,
    pub scroll: usize,
    pub stick_bottom: bool,
    pub expanded: HashSet<String>,
    pub tool_cursor: Option<ToolRef>,
    pub pending_perm: Option<PendingPermission>,
    pub pending_question: Option<PendingQuestion>,
    pub last_error: Option<String>,
    optimistic_seq: u64,
    unadopted_locals: u64,
    synthetic_seq: u64,
    pub paste_parts: Vec<crate::paste::PastePart>,
    pub paste_seq: u64,
    pub redo_snapshot: Option<Vec<Message>>,
    pub mention_results: Vec<String>,
    pub mention_selected: usize,
    pub dirty: bool,
}

impl SessionState {
    pub fn new(id: u32, name: String, dir: PathBuf) -> Self {
        Self {
            id,
            name,
            dir,
            session_id: crate::harness::SessionId(id),
            provider: ProviderKind::Local,
            task: None,
            oc_sid: None,
            model: None,
            agent: None,
            slash_selected: 0,
            last_send: None,
            pending_send: None,
            queue: Vec::new(),
            interrupt_armed: None,
            activity: None,
            cost: 0.0,
            ctx_tokens: 0,
            last_prune: None,
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
            paste_parts: Vec::new(),
            paste_seq: 0,
            redo_snapshot: None,
            mention_results: Vec::new(),
            mention_selected: 0,
            dirty: true,
        }
    }

    pub fn push_local_user(&mut self, text: &str) -> String {
        self.optimistic_seq += 1;
        self.unadopted_locals += 1;
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let local_id = format!("local-{created}-{}", self.optimistic_seq);
        let msg = Message {
            id: local_id.clone(),
            role: Role::User,
            error: None,
            completed: None,
            created: Some(created),
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

    pub fn upsert_message_meta(&mut self, msg: &Message) {
        if let Some(existing) = self.messages.iter_mut().find(|m| m.id == msg.id) {
            existing.role = msg.role;
            existing.error = msg.error.clone();
            existing.completed = msg.completed;
            existing.cost = msg.cost;
            existing.tokens = msg.tokens;
        } else {
            let mut meta = msg.clone();
            meta.parts.clear();
            self.messages.push(meta);
        }
        self.recompute_metrics();
        self.dirty = true;
    }

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
            .filter(|m| m.role == Role::Assistant)
            .find_map(|m| m.tokens)
            .map(|t| t.context())
            .unwrap_or(0);
    }

    pub fn upsert_part(&mut self, message: &Message, part: Part) {
        let dirty_msg_id = message.id.clone();
        if !self.messages.iter().any(|m| m.id == dirty_msg_id) {
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
            if message.error.is_some() {
                msg.error = message.error.clone();
            }
            if message.completed.is_some() {
                msg.completed = message.completed;
            }
            if message.cost.is_some() {
                msg.cost = message.cost;
            }
            if message.tokens.is_some() {
                msg.tokens = message.tokens;
            }
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
        self.recompute_metrics();
        self.dirty = true;
    }

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

    #[test]
    fn message_meta_inserts_a_row_so_parts_keep_their_role() {
        let mut s = SessionState::new(1, "s".into(), PathBuf::from("."));
        let meta = Message {
            id: "hist-1".into(),
            role: Role::User,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: Vec::new(),
        };
        s.upsert_message_meta(&meta);
        assert_eq!(s.messages.len(), 1, "meta must create the row");
        assert_eq!(s.messages[0].role, Role::User);

        let part = Part {
            id: "hist-1-p1".into(),
            message_id: "hist-1".into(),
            kind: PartKind::Text { text: "hi".into(), synthetic: false },
        };
        let mut with_part = meta.clone();
        with_part.parts = vec![part.clone()];
        s.upsert_part(&with_part, part);
        assert_eq!(s.messages[0].role, Role::User);
        assert_eq!(s.messages[0].parts.len(), 1);
    }

    #[test]
    fn a_part_does_not_wipe_recorded_tokens_or_cost() {
        use crate::harness::transcript::{Part, PartKind, TokenUsage};
        let mut s = SessionState::new(1, "s".into(), PathBuf::from("."));
        let usage = Message {
            id: "m1".into(),
            role: Role::Assistant,
            error: None,
            completed: None,
            created: None,
            cost: Some(0.25),
            tokens: Some(TokenUsage { input: 12_345, output: 67, ..Default::default() }),
            parts: Vec::new(),
        };
        s.upsert_message_meta(&usage);
        assert_eq!(s.ctx_tokens, 12_345);

        let part = Part {
            id: "m1-text".into(),
            message_id: "m1".into(),
            kind: PartKind::Text { text: "hi".into(), synthetic: false },
        };
        let mut with_part = usage.clone();
        with_part.cost = None;
        with_part.tokens = None;
        with_part.parts = vec![part.clone()];
        s.upsert_part(&with_part, part);

        assert_eq!(s.messages[0].tokens.map(|t| t.input), Some(12_345));
        assert_eq!(s.messages[0].cost, Some(0.25));
        assert_eq!(s.ctx_tokens, 12_345, "context readout must survive");
    }
}
