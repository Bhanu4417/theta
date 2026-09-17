pub mod local;

use crate::harness::HarnessEvent;
use std::fmt;
use std::pin::Pin;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProviderKind {
    #[default]
    Local,
}

impl ProviderKind {
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::Local => "local",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Local => "Theta",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "local" => Some(ProviderKind::Local),
            _ => None,
        }
    }

    pub fn capabilities(self) -> ProviderCapabilities {
        match self {
            ProviderKind::Local => ProviderCapabilities::LOCAL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub native_resume: bool,
    pub native_fork: bool,
    pub streaming: bool,
    pub permissions: bool,
    pub questions: bool,
    pub filesystem: bool,
}

impl ProviderCapabilities {
    pub const LOCAL: Self = Self {
        native_resume: true,
        native_fork: true,
        streaming: true,
        permissions: true,
        questions: true,
        filesystem: true,
    };
}

#[derive(Debug, Clone)]
pub enum ProviderError {
    #[allow(dead_code)]
    Unavailable(String),
    SessionNotFound(String),
    Transport(String),
    Protocol(String),
    /// A non-2xx HTTP response, with the status preserved.
    ///
    /// The status is carried as a number rather than only appearing inside the
    /// message text, so retry decisions can match on it. Substring-matching the
    /// message (the previous approach) recognized 500, 502, 503 and 504 but
    /// missed every other 5xx, and could be fooled by a body that happened to
    /// contain those digits.
    Status {
        code: u16,
        message: String,
        /// `Retry-After`, in milliseconds, when the server sent one.
        retry_after_ms: Option<u64>,
    },
    Auth(String),
    Unsupported(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Unavailable(m) => write!(f, "provider unavailable: {m}"),
            ProviderError::SessionNotFound(m) => write!(f, "session not found: {m}"),
            ProviderError::Transport(m) => write!(f, "transport error: {m}"),
            ProviderError::Protocol(m) => write!(f, "protocol error: {m}"),
            ProviderError::Status { code, message, .. } => {
                write!(f, "{code}: {message}")
            }
            ProviderError::Auth(m) => write!(f, "authentication error: {m}"),
            ProviderError::Unsupported(m) => write!(f, "unsupported operation: {m}"),
        }
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSession {
    pub provider: ProviderKind,
    pub id: String,
    pub directory: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub directory: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelId {
    pub provider: String,
    pub model: String,
}


#[derive(Debug, Clone)]
pub struct RoutedEvent {
    pub session_id: Option<String>,
    pub event: HarnessEvent,
}

pub type EventSink = UnboundedSender<RoutedEvent>;

pub trait EventPump {
    fn pump(
        &self,
        out: EventSink,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ProviderError>> + Send + '_>>;
}

impl<T: EventPump + ?Sized> EventPump for std::sync::Arc<T> {
    fn pump(
        &self,
        out: EventSink,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ProviderError>> + Send + '_>> {
        (**self).pump(out)
    }
}

pub trait AgentProvider {
    #[allow(dead_code)]
    fn kind(&self) -> ProviderKind;

    #[allow(dead_code)]
    fn capabilities(&self) -> ProviderCapabilities {
        self.kind().capabilities()
    }

    #[allow(dead_code)]
    async fn create_session(&self, config: SessionConfig)
        -> Result<ProviderSession, ProviderError>;

    async fn resume_session(&self, provider_id: &str) -> Result<ProviderSession, ProviderError>;

    async fn send_message(
        &self,
        session: &ProviderSession,
        text: &str,
        model: Option<ModelId>,
        agent: Option<String>,
        attachments: &[crate::mentions::Attachment],
    ) -> Result<(), ProviderError>;

    async fn interrupt(&self, session: &ProviderSession) -> Result<(), ProviderError>;

    async fn fork(
        &self,
        session: &ProviderSession,
        at: Option<&str>,
    ) -> Result<ProviderSession, ProviderError>;
}

#[cfg(test)]
pub mod testing {

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

    impl crate::providers::EventPump for MockProvider {
        fn pump(
            &self,
            out: crate::providers::EventSink,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), ProviderError>> + Send + '_>,
        > {
            Box::pin(async move {
                for event in self.script() {
                    let _ = out.send(RoutedEvent {
                        session_id: Some("mock-ses".into()),
                        event,
                    });
                }
                Ok(())
            })
        }
    }

    impl AgentProvider for MockProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Local
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
            _attachments: &[crate::mentions::Attachment],
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
    async fn event_pump_contract_delivers_routed_events() {
        let p = MockProvider::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RoutedEvent>();
        let handle = tokio::spawn(async move {
            p.pump(tx).await.expect("mock pump should succeed");
        });
        let got: Vec<RoutedEvent> = {
            let mut v = Vec::new();
            while let Some(ev) = rx.recv().await {
                v.push(ev);
            }
            v
        };
        handle.await.unwrap();
        assert_eq!(got.len(), 7, "script size: {:?}", got.len());
        for ev in &got {
            assert_eq!(ev.session_id.as_deref(), Some("mock-ses"));
        }
        assert!(matches!(
            got[0].event,
            HarnessEvent::SessionWorking
        ));
        assert!(got.last().unwrap().event == HarnessEvent::SessionIdle);
    }

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
        assert_eq!(s.provider, ProviderKind::Local);
        p.send_message(&s, "hello", None, None, &[]).await.unwrap();
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
        let err = p.send_message(&s, "x", None, None, &[]).await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)));
        assert!(err.to_string().contains("transport error"));
        let e = p.resume_session("").await.unwrap_err();
        assert!(matches!(e, ProviderError::SessionNotFound(_)));
    }

    #[test]
    fn capabilities_and_ids() {
        assert_eq!(ProviderKind::Local.id(), "local");
        assert_eq!(ProviderKind::from_id("local"), Some(ProviderKind::Local));
        assert_eq!(ProviderKind::from_id("nope"), None);
        assert_eq!(
            ProviderKind::Local.capabilities(),
            ProviderCapabilities::LOCAL
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
