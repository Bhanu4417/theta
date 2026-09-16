use std::path::PathBuf;
use std::time::Instant;

use crate::providers::ProviderKind;

pub mod transcript;

pub use transcript::TranscriptUpdate;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum HarnessEvent {
    SessionIdle,
    SessionWorking,
    SessionThinking,
    SessionRetrying(String),
    SessionError(String),
    SessionInterrupted,

    PermissionAsked {
        id: String,
        kind: String,
        detail: String,
    },
    PermissionReplied,

    QuestionAsked(QuestionPrompt),
    QuestionReplied,

    ToolStarted {
        tool: String,
        title: String,
    },
    ToolFinished {
        tool: String,
        ok: bool,
    },

    AssistantFinished,

    CompactionStarted,
    CompactionFinished { tokens_before: u64 },

    Transcript(TranscriptUpdate),

    FilesChanged,
    BranchChanged,

    ProviderError(String),
    ProviderDisconnected,

    ProviderSpecific {
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuestionChoice {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionChoice>,
    pub multiple: bool,
    pub custom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuestionPrompt {
    pub id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u32);

impl SessionId {
    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Interrupted
        )
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: u64,
    pub title: String,
    pub provider: ProviderKind,
    pub workspace: PathBuf,
    pub session: SessionId,
    pub status: TaskStatus,
    pub created: Instant,
    pub started: Option<Instant>,
    pub completed: Option<Instant>,
}

impl Task {
    pub fn new(id: u64, title: impl Into<String>, workspace: PathBuf, session: SessionId) -> Self {
        Self {
            id,
            title: title.into(),
            provider: ProviderKind::Local,
            workspace,
            session,
            status: TaskStatus::Pending,
            created: Instant::now(),
            started: None,
            completed: None,
        }
    }

    pub fn start(&mut self) -> bool {
        if self.status == TaskStatus::Pending {
            self.status = TaskStatus::Running;
            self.started = Some(Instant::now());
            true
        } else {
            false
        }
    }

    pub fn wait(&mut self) -> bool {
        if self.status == TaskStatus::Running {
            self.status = TaskStatus::Waiting;
            true
        } else {
            false
        }
    }

    pub fn finish(&mut self, status: TaskStatus) -> bool {
        if self.status.is_terminal() {
            return false;
        }
        self.status = status;
        self.completed = Some(Instant::now());
        true
    }
}

pub struct NotificationPolicy {
    window_ms: u128,
    last: Option<Instant>,
    pending: usize,
}

impl NotificationPolicy {
    pub fn new(window_ms: u128) -> Self {
        Self {
            window_ms,
            last: None,
            pending: 0,
        }
    }

    pub fn should_notify(&mut self) -> bool {
        let now = Instant::now();
        match self.last {
            Some(t) if now.duration_since(t).as_millis() < self.window_ms => {
                self.pending += 1;
                false
            }
            _ => {
                self.last = Some(now);
                self.pending = 0;
                true
            }
        }
    }

    pub fn suppressed(&self) -> usize {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle_transitions() {
        let mut t = Task::new(1, "prompt", PathBuf::from("/tmp"), SessionId(7));
        assert_eq!(t.status, TaskStatus::Pending);
        assert!(t.start());
        assert_eq!(t.status, TaskStatus::Running);
        assert!(!t.start(), "cannot start twice");
        assert!(t.wait());
        assert_eq!(t.status, TaskStatus::Waiting);
        assert!(t.finish(TaskStatus::Completed));
        assert!(t.status.is_terminal());
        assert!(t.completed.is_some());
        assert!(!t.finish(TaskStatus::Failed), "terminal is final");
    }

    #[test]
    fn task_can_fail_or_interrupt() {
        let mut a = Task::new(1, "a", PathBuf::from("/a"), SessionId(1));
        a.start();
        assert!(a.finish(TaskStatus::Failed));
        let mut b = Task::new(2, "b", PathBuf::from("/b"), SessionId(2));
        b.start();
        assert!(b.finish(TaskStatus::Interrupted));
    }

    #[test]
    fn notification_policy_debounces() {
        let mut p = NotificationPolicy::new(10_000);
        assert!(p.should_notify(), "first passes");
        assert!(!p.should_notify(), "second within window suppressed");
        assert!(!p.should_notify());
        assert_eq!(p.suppressed(), 2);
    }

    #[test]
    fn session_identity_is_owned_by_theta() {
        let s = SessionId(42);
        assert_eq!(s.get(), 42);
    }
}
