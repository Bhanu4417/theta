//! Theta's harness: provider-independent session/task/event concepts.
//!
//! The UI and the manager communicate in terms of these types, never raw
//! provider protocol. The local harness (`providers::local`) converts its
//! native events into [`HarnessEvent`]s.

use std::path::PathBuf;
use std::time::Instant;

use crate::providers::ProviderKind;

pub mod transcript;

pub use transcript::TranscriptUpdate;

/// Common, provider-neutral events. Where a provider produces something that
/// has no common representation, `ProviderSpecific` preserves it untouched.
// Some variants are reserved: they are part of the provider-neutral protocol
// that the UI, the manager and future adapters are written against, even when
// the current local harness does not emit them yet. Deleting one would break
// that contract, so they are intentionally kept.
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

    /// Context compaction is about to run (provider call in flight).
    CompactionStarted,
    /// Context compaction finished; the prompt was rebuilt from a summary.
    CompactionFinished { tokens_before: u64 },

    /// A provider-neutral transcript change (streaming text, tool state, …).
    Transcript(TranscriptUpdate),

    /// Files under the working directory changed.
    FilesChanged,
    /// The git branch changed.
    BranchChanged,

    ProviderError(String),
    ProviderDisconnected,

    /// An event with no common mapping; carries the native type name only.
    ProviderSpecific {
        kind: String,
    },
}

/// A single selectable option in an agent question.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuestionChoice {
    pub label: String,
    pub description: String,
}

/// A single question (provider-neutral form of the `ask` tool payload).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionChoice>,
    pub multiple: bool,
    pub custom: bool,
}

/// A pending question request from the agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QuestionPrompt {
    pub id: String,
    pub questions: Vec<Question>,
}

/// Theta-owned session identity. Provider session ids are metadata and never
/// used as the primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u32);

impl SessionId {
    pub fn get(self) -> u32 {
        self.0
    }
}

/// A provider-neutral unit of work tracked by the harness. This phase records
/// lifecycle only; it does not plan or dispatch autonomously.
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
            provider: ProviderKind::OpenCode,
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

/// Debounces/group-gates notifications so several agents finishing at once do
/// not spam desktop notifications or sounds.
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

    /// Returns true when a notification should actually be shown.
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

    /// Number of notifications suppressed within the current window.
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
        // Provider id lives in metadata; Theta identity is independent.
        let s = SessionId(42);
        assert_eq!(s.get(), 42);
    }
}
