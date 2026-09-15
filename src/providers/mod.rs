//! Provider-independent agent interface.
//!
//! Theta's harness talks to agent runtimes through this boundary. The first
//! (and only, for now) implementation is the OpenCode adapter in
//! [`opencode`]. New providers are added by implementing [`AgentProvider`]
//! and declaring [`ProviderCapabilities`]; the UI/harness never depend on
//! provider protocol details.

pub mod opencode;

use std::fmt;

/// Identifies which agent runtime a Theta session is backed by. This is
/// provider metadata — the Theta session itself has its own identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProviderKind {
    #[default]
    OpenCode,
}

impl ProviderKind {
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::OpenCode => "opencode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::OpenCode => "OpenCode",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "opencode" => Some(ProviderKind::OpenCode),
            _ => None,
        }
    }

    /// Capabilities for this provider, computed without I/O.
    pub fn capabilities(self) -> ProviderCapabilities {
        match self {
            ProviderKind::OpenCode => ProviderCapabilities::OPENCODE,
        }
    }
}

/// What a provider can actually do. The harness branches on these instead of
/// assuming every provider behaves like OpenCode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Can re-attach to a previously created provider session.
    pub native_resume: bool,
    /// Can duplicate a session with its context (provider-native).
    pub native_fork: bool,
    /// Streams assistant output incrementally.
    pub streaming: bool,
    /// Emits interactive permission requests.
    pub permissions: bool,
    /// Emits interactive `ask` questions.
    pub questions: bool,
    /// Provides file read/diff content.
    pub filesystem: bool,
}

impl ProviderCapabilities {
    pub const OPENCODE: Self = Self {
        native_resume: true,
        native_fork: true,
        streaming: true,
        permissions: true,
        questions: true,
        filesystem: true,
    };
}

/// Categorized provider failures. Provider-specific errors are converted into
/// these before reaching the UI, so the harness never surfaces raw protocol
/// strings.
#[derive(Debug, Clone)]
pub enum ProviderError {
    /// The runtime/binary is not reachable or not installed.
    Unavailable(String),
    /// The requested session no longer exists.
    SessionNotFound(String),
    /// Transport/connection failure.
    Transport(String),
    /// The provider replied in a shape we could not understand.
    Protocol(String),
    /// Authentication/authorization failure.
    Auth(String),
    /// The provider does not support the requested operation.
    Unsupported(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Unavailable(m) => write!(f, "provider unavailable: {m}"),
            ProviderError::SessionNotFound(m) => write!(f, "session not found: {m}"),
            ProviderError::Transport(m) => write!(f, "transport error: {m}"),
            ProviderError::Protocol(m) => write!(f, "protocol error: {m}"),
            ProviderError::Auth(m) => write!(f, "authentication error: {m}"),
            ProviderError::Unsupported(m) => write!(f, "unsupported operation: {m}"),
        }
    }
}

impl std::error::Error for ProviderError {}

/// A provider-owned session handle. The `id` is opaque and only meaningful to
/// the provider; Theta tracks its own session identity separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSession {
    pub provider: ProviderKind,
    pub id: String,
    pub directory: String,
}

/// Everything the harness needs to ask a provider for a fresh session.
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub directory: String,
    pub title: String,
}

/// A provider-scoped model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelId {
    pub provider: String,
    pub model: String,
}

/// The interface every agent runtime adapter implements.
///
/// This is intentionally small: session lifecycle, messaging and interruption.
/// Streaming/event delivery is provider-specific and exposed through
/// [`crate::harness::HarnessEvent`] by the adapter's event pump.
pub trait AgentProvider {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> ProviderCapabilities {
        self.kind().capabilities()
    }

    async fn create_session(&self, config: SessionConfig)
        -> Result<ProviderSession, ProviderError>;

    async fn resume_session(&self, provider_id: &str) -> Result<ProviderSession, ProviderError>;

    async fn send_message(
        &self,
        session: &ProviderSession,
        text: &str,
        model: Option<ModelId>,
        agent: Option<String>,
    ) -> Result<(), ProviderError>;

    async fn interrupt(&self, session: &ProviderSession) -> Result<(), ProviderError>;

    /// Provider-native fork, when [`ProviderCapabilities::native_fork`] holds.
    async fn fork(
        &self,
        session: &ProviderSession,
        at: Option<&str>,
    ) -> Result<ProviderSession, ProviderError>;
}

#[cfg(test)]
pub mod testing {
    //! A fully in-memory provider used by harness tests. No model calls.

    use super::*;
    use crate::harness::HarnessEvent;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockProvider {
        pub created: Mutex<usize>,
        pub sent: Mutex<Vec<String>>,
        pub interrupted: Mutex<usize>,
        pub forks: Mutex<usize>,
        pub fail_send: bool,
    }

    impl MockProvider {
        pub fn new() -> Self {
            Self::default()
        }

        /// A deterministic provider-neutral event script worth of a small
        /// agent turn. Used to drive the harness/UI without a real provider.
        pub fn script(&self) -> Vec<HarnessEvent> {
            use crate::harness::transcript::{
                Part, PartKind, ToolInfo, ToolStatus, TranscriptUpdate,
            };
            use serde_json::json;
            let text = |id: &str, body: &str| {
                HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                    id: id.into(),
                    message_id: "m1".into(),
                    kind: PartKind::Text {
                        text: body.into(),
                        synthetic: false,
                    },
                }))
            };
            vec![
                HarnessEvent::SessionWorking,
                text("p1", "Hello"),
                HarnessEvent::ToolStarted {
                    tool: "bash".into(),
                    title: "Run tests".into(),
                },
                HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                    id: "p2".into(),
                    message_id: "m1".into(),
                    kind: PartKind::Tool(ToolInfo {
                        tool: "bash".into(),
                        call_id: "c1".into(),
                        status: ToolStatus::Running,
                        title: Some("Run tests".into()),
                        input: json!({"command": "cargo test"}),
                        output: None,
                        error: None,
                        metadata: json!({}),
                        start_ms: None,
                    }),
                })),
                text("p3", "Done"),
                HarnessEvent::AssistantFinished,
                HarnessEvent::SessionIdle,
            ]
        }
    }

    impl AgentProvider for MockProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenCode
        }

        async fn create_session(
            &self,
            config: SessionConfig,
        ) -> Result<ProviderSession, ProviderError> {
            let mut n = self.created.lock().unwrap();
            *n += 1;
            Ok(ProviderSession {
                provider: self.kind(),
                id: format!("mock-session-{n}"),
                directory: config.directory,
            })
        }

        async fn resume_session(
            &self,
            provider_id: &str,
        ) -> Result<ProviderSession, ProviderError> {
            if provider_id.is_empty() {
                return Err(ProviderError::SessionNotFound("empty".into()));
            }
            Ok(ProviderSession {
                provider: self.kind(),
                id: provider_id.to_string(),
                directory: "/mock".into(),
            })
        }

        async fn send_message(
            &self,
            _session: &ProviderSession,
            text: &str,
            _model: Option<ModelId>,
            _agent: Option<String>,
        ) -> Result<(), ProviderError> {
            if self.fail_send {
                return Err(ProviderError::Transport("mock failure".into()));
            }
            self.sent.lock().unwrap().push(text.to_string());
            Ok(())
        }

        async fn interrupt(&self, _session: &ProviderSession) -> Result<(), ProviderError> {
            *self.interrupted.lock().unwrap() += 1;
            Ok(())
        }

        async fn fork(
            &self,
            session: &ProviderSession,
            _at: Option<&str>,
        ) -> Result<ProviderSession, ProviderError> {
            *self.forks.lock().unwrap() += 1;
            Ok(ProviderSession {
                provider: self.kind(),
                id: format!("{}-fork", session.id),
                directory: session.directory.clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MockProvider;
    use super::*;

    #[tokio::test]
    async fn mock_provider_lifecycle() {
        let p = MockProvider::new();
        assert!(p.capabilities().native_fork);
        let s = p
            .create_session(SessionConfig {
                directory: "/proj".into(),
                title: "t".into(),
            })
            .await
            .unwrap();
        assert_eq!(s.provider, ProviderKind::OpenCode);
        p.send_message(&s, "hello", None, None).await.unwrap();
        assert_eq!(p.sent.lock().unwrap().as_slice(), &["hello".to_string()]);
        p.interrupt(&s).await.unwrap();
        assert_eq!(*p.interrupted.lock().unwrap(), 1);
        let f = p.fork(&s, None).await.unwrap();
        assert_ne!(f.id, s.id);
        assert_eq!(*p.forks.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn provider_errors_are_categorized_and_displayable() {
        let p = MockProvider {
            fail_send: true,
            ..Default::default()
        };
        let s = p
            .create_session(SessionConfig {
                directory: "/x".into(),
                title: "x".into(),
            })
            .await
            .unwrap();
        let err = p.send_message(&s, "x", None, None).await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)));
        assert!(err.to_string().contains("transport error"));
        let e = p.resume_session("").await.unwrap_err();
        assert!(matches!(e, ProviderError::SessionNotFound(_)));
    }

    #[test]
    fn capabilities_and_ids() {
        assert_eq!(ProviderKind::OpenCode.id(), "opencode");
        assert_eq!(
            ProviderKind::from_id("opencode"),
            Some(ProviderKind::OpenCode)
        );
        assert_eq!(ProviderKind::from_id("nope"), None);
        assert_eq!(
            ProviderKind::OpenCode.capabilities(),
            ProviderCapabilities::OPENCODE
        );
    }

    #[test]
    fn mock_script_drives_neutral_transcript_rendering() {
        use crate::harness::transcript::{Message, Role};
        use crate::harness::{HarnessEvent, TranscriptUpdate};
        let p = MockProvider::new();
        let mut s =
            crate::session::SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        let mut lifecycle = Vec::new();
        for ev in p.script() {
            match ev {
                HarnessEvent::Transcript(TranscriptUpdate::Part(part)) => {
                    let meta = Message {
                        id: part.message_id.clone(),
                        role: Role::Assistant,
                        error: None,
                        completed: None,
                        created: None,
                        cost: None,
                        tokens: None,
                        parts: vec![part.clone()],
                    };
                    s.upsert_part(&meta, part);
                }
                HarnessEvent::Transcript(TranscriptUpdate::MessageMeta(m)) => {
                    s.upsert_message_meta(&m)
                }
                HarnessEvent::Transcript(TranscriptUpdate::PartRemoved {
                    message_id,
                    part_id,
                }) => s.remove_part(&message_id, &part_id),
                other => lifecycle.push(other),
            }
        }
        // The mock drove the same neutral render path OpenCode would.
        let cache = crate::ui::conversation::build_cache(&s, 60, 0);
        let text: String = cache
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Hello"));
        assert!(text.contains("Done"));
        assert!(text.contains("Run tests"));
        assert!(lifecycle
            .iter()
            .any(|e| matches!(e, HarnessEvent::AssistantFinished)));
        assert!(lifecycle
            .iter()
            .any(|e| matches!(e, HarnessEvent::SessionIdle)));
    }
}
