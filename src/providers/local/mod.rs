//! Theta's own in-process backend: the local agent loop exposed behind the
//! same `AgentProvider` + `EventPump` contract as OpenCode.
//!
//! It needs no server. `send_message` spawns a turn that emits neutral
//! `RoutedEvent`s on an internal channel; `pump` forwards them to the manager,
//! so the rest of the app cannot tell local turns from OpenCode ones.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::agent::AgentLoop;
use crate::ai::ChatMessage;
use crate::tree::SessionTree;
use crate::harness::HarnessEvent;
use crate::providers::{
    AgentProvider, EventPump, EventSink, ModelId, ProviderError, ProviderKind, ProviderSession,
    RoutedEvent, SessionConfig,
};

struct LocalSession {
    session: ProviderSession,
    tree: SessionTree,
    cancel: Arc<AtomicBool>,
}

/// Everything the adapter needs, shared across its (async) methods.
struct Inner {
    agent: Arc<AgentLoop>,
    sessions: Mutex<HashMap<String, LocalSession>>,
}

pub struct LocalProvider {
    inner: Arc<Inner>,
    events_tx: UnboundedSender<RoutedEvent>,
    events_rx: tokio::sync::Mutex<UnboundedReceiver<RoutedEvent>>,
    seq: AtomicU64,
    default_dir: String,
}

impl LocalProvider {
    pub fn new(agent: Arc<AgentLoop>, default_dir: impl Into<String>) -> Self {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                agent,
                sessions: Mutex::new(HashMap::new()),
            }),
            events_tx,
            events_rx: tokio::sync::Mutex::new(events_rx),
            seq: AtomicU64::new(1),
            default_dir: default_dir.into(),
        }
    }

    /// Pre-create a session id without sending anything (used by connect).
    pub fn register(&self, dir: &str, title: &str) -> ProviderSession {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = format!("theta-local-{n}");
        let dir = if dir.is_empty() { self.default_dir.clone() } else { dir.to_string() };
        let session = ProviderSession {
            provider: ProviderKind::Local,
            id: id.clone(),
            directory: dir,
        };
        let tree = SessionTree::sidecar_path(&id)
            .and_then(|p| SessionTree::load(&p))
            .unwrap_or_default();
        self.inner.sessions.lock().unwrap().insert(
            id,
            LocalSession { session: session.clone(), tree, cancel: Arc::new(AtomicBool::new(false)) },
        );
        let _ = title;
        session
    }

    /// Snapshot a session's history tree (for the `/tree` overlay).
    pub fn tree_snapshot(&self, id: &str) -> Option<SessionTree> {
        self.inner.sessions.lock().unwrap().get(id).map(|s| s.tree.clone())
    }

    /// Navigate a session to an earlier entry, optionally injecting a branch
    /// summary of the abandoned work.
    pub fn navigate(&self, id: &str, entry: &str, summary: Option<String>) -> bool {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(id) else {
            return false;
        };
        let ok = s.tree.navigate_with_summary(entry, summary);
        if ok {
            if let Some(path) = SessionTree::sidecar_path(id) {
                let _ = s.tree.save(&path);
            }
        }
        ok
    }

    /// Navigate backwards, summarizing the abandoned branch with the model and
    /// carrying it forward as a branch-summary node (Pi's tree navigation).
    pub async fn navigate_auto(&self, id: &str, entry: &str) -> bool {
        let input = {
            let sessions = self.inner.sessions.lock().unwrap();
            let Some(s) = sessions.get(id) else {
                return false;
            };
            let Some(old) = s.tree.leaf.clone() else {
                return false;
            };
            if old == entry {
                return true;
            }
            // Only summarize when jumping to an ancestor (backwards).
            if s.tree.is_ancestor(entry, &old) {
                s.tree.branch_summary_input(&old, Some(entry), 20_000, 2_000)
            } else {
                None
            }
        };
        let summary = match input {
            Some(text) => self
                .inner
                .agent
                .summarize_branch(&text)
                .await
                .ok()
                .filter(|s| !s.trim().is_empty()),
            None => None,
        };
        self.navigate(id, entry, summary)
    }
}

impl AgentProvider for LocalProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Local
    }

    async fn create_session(&self, config: SessionConfig) -> Result<ProviderSession, ProviderError> {
        Ok(self.register(&config.directory, &config.title))
    }

    async fn resume_session(&self, provider_id: &str) -> Result<ProviderSession, ProviderError> {
        self.inner
            .sessions
            .lock()
            .unwrap()
            .get(provider_id)
            .map(|s| s.session.clone())
            .ok_or_else(|| ProviderError::SessionNotFound(provider_id.to_string()))
    }

    async fn send_message(
        &self,
        session: &ProviderSession,
        text: &str,
        _model: Option<ModelId>,
        _agent: Option<String>,
        _attachments: &[crate::mentions::Attachment],
    ) -> Result<(), ProviderError> {
        let (mut tree, cancel, dir) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let entry = sessions
                .get_mut(&session.id)
                .ok_or_else(|| ProviderError::SessionNotFound(session.id.clone()))?;
            entry.cancel.store(false, Ordering::Relaxed);
            (
                std::mem::take(&mut entry.tree),
                entry.cancel.clone(),
                entry.session.directory.clone(),
            )
        };
        // Rebuild the prompt from the active branch, then run.
        let mut history = tree.context();

        let agent = self.inner.agent.clone();
        let inner = self.inner.clone();
        let tx = self.events_tx.clone();
        let sid = session.id.clone();
        let text = text.to_string();
        let cwd = PathBuf::from(&dir);

        // Run the turn on its own task; events stream back through `pump`.
        tokio::spawn(async move {
            let mut emit = {
                let tx = tx.clone();
                let sid = sid.clone();
                move |event: HarnessEvent| {
                    let _ = tx.send(RoutedEvent { session_id: Some(sid.clone()), event });
                }
            };
            let mut journal: Vec<ChatMessage> = Vec::new();
            let result = agent
                .run_turn_journaled(&mut history, &text, &cwd, &cancel, &mut emit, &mut journal)
                .await;
            if let Err(e) = result {
                let _ = tx.send(RoutedEvent {
                    session_id: Some(sid.clone()),
                    event: HarnessEvent::SessionError(e.to_string()),
                });
                let _ = tx.send(RoutedEvent { session_id: Some(sid.clone()), event: HarnessEvent::SessionIdle });
            }
            // Record the turn in the session tree and persist it.
            tree.append(&ChatMessage::user(&text));
            for m in &journal {
                tree.append(m);
            }
            if let Some(path) = SessionTree::sidecar_path(&sid) {
                let _ = tree.save(&path);
            }
            if let Ok(mut sessions) = inner.sessions.lock() {
                if let Some(entry) = sessions.get_mut(&sid) {
                    entry.tree = tree;
                }
            }
        });

        Ok(())
    }

    async fn interrupt(&self, session: &ProviderSession) -> Result<(), ProviderError> {
        if let Some(s) = self.inner.sessions.lock().unwrap().get(&session.id) {
            s.cancel.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn fork(
        &self,
        _session: &ProviderSession,
        _at: Option<&str>,
    ) -> Result<ProviderSession, ProviderError> {
        Err(ProviderError::Unsupported("local fork".into()))
    }
}

impl EventPump for LocalProvider {
    fn pump(
        &self,
        out: EventSink,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ProviderError>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let next = self.events_rx.lock().await.recv().await;
                match next {
                    Some(ev) => {
                        let _ = out.send(ev);
                    }
                    None => return Ok(()),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentLoop;
    use crate::ai::{AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent, ToolCall};
    use std::collections::VecDeque;

    struct OneShot {
        turns: Mutex<VecDeque<AssistantTurn>>,
    }
    impl Provider for OneShot {
        fn id(&self) -> &'static str {
            "one-shot"
        }
        fn stream<'a>(
            &'a self,
            _r: ChatRequest,
            on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
        > {
            Box::pin(async move {
                let t = self.turns.lock().unwrap().pop_front().unwrap_or(AssistantTurn {
                    text: "bye".into(),
                    tool_calls: vec![],
                    finish: Some(FinishReason::Stop),
                });
                on_event(ProviderEvent::TextDelta(t.text.clone()));
                Ok(t)
            })
        }
    }

    #[tokio::test]
    async fn local_provider_streams_a_turn_through_the_pump() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Keep session-tree sidecars out of the real data dir.
        let session_dir = std::env::temp_dir().join(format!("theta-sessions-{}", std::process::id()));
        std::env::set_var("THETA_SESSION_DIR", &session_dir);
        let provider = OneShot {
            turns: Mutex::new(
                vec![AssistantTurn {
                    text: "hello".into(),
                    tool_calls: vec![ ToolCall { id: "c".into(), name: "read".into(), arguments: "{}".into() } ],
                    finish: None,
                }]
                .into(),
            ),
        };
        let agent = Arc::new(AgentLoop::new(Box::new(provider), "gpt-4o"));
        let local = Arc::new(LocalProvider::new(agent, &dir));

        // Start the pump first (like the manager does), then send.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pump_local = local.clone();
        let pump_task = tokio::spawn(async move {
            let _ = pump_local.pump(tx).await;
        });

        let session = local
            .create_session(SessionConfig { directory: dir, title: "t".into() })
            .await
            .unwrap();
        local.send_message(&session, "hi", None, None, &[]).await.unwrap();

        // Collect until we see SessionIdle (the loop's terminal event).
        let mut seen = Vec::new();
        loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("event within timeout")
                .expect("channel open");
            let terminal = matches!(ev.event, crate::harness::HarnessEvent::SessionIdle);
            seen.push(ev);
            if terminal {
                break;
            }
        }
        assert!(seen.iter().any(|e| e.session_id.as_deref() == Some(session.id.as_str())));
        assert!(seen.iter().any(|e| e.event == crate::harness::HarnessEvent::SessionWorking));
        assert!(seen.iter().any(|e| e.event == crate::harness::HarnessEvent::AssistantFinished));
        pump_task.abort();

        // The turn is recorded as a history tree with user + assistant nodes.
        let tree = local.tree_snapshot(&session.id).expect("tree");
        assert!(tree.entries.len() >= 2, "entries: {}", tree.entries.len());
        assert_eq!(tree.entries[0].kind, crate::tree::EntryKind::User);
        assert!(tree.entries.iter().any(|e| e.kind == crate::tree::EntryKind::Assistant));
        // The active branch rebuilds into a usable prompt.
        let ctx = tree.context();
        assert!(ctx.iter().any(|m| m.role == crate::ai::Role::User && m.text == "hi"));

        // Navigating to the first (user) entry succeeds and moves the leaf.
        let first = tree.entries[0].id.clone();
        assert!(local.navigate(&session.id, &first, None));
        assert_eq!(local.tree_snapshot(&session.id).unwrap().leaf.as_deref(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn navigate_auto_injects_a_branch_summary() {
        use crate::tree::EntryKind;
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let session_dir =
            std::env::temp_dir().join(format!("theta-sessions-{}", std::process::id()));
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let provider = OneShot {
            turns: Mutex::new(
                vec![AssistantTurn {
                    text: "answer one".into(),
                    tool_calls: vec![],
                    finish: Some(FinishReason::Stop),
                }]
                .into(),
            ),
        };
        let agent = Arc::new(AgentLoop::new(Box::new(provider), "m"));
        let local = Arc::new(LocalProvider::new(agent, &dir));
        let session = local
            .create_session(SessionConfig { directory: dir, title: "t".into() })
            .await
            .unwrap();

        local.send_message(&session, "hello", None, None, &[]).await.unwrap();
        // Wait for the spawned turn to record itself in the tree.
        for _ in 0..100 {
            let done = local
                .tree_snapshot(&session.id)
                .map(|t| t.entries.iter().any(|e| e.kind == EntryKind::Assistant))
                .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let tree = local.tree_snapshot(&session.id).expect("tree");
        let first_user = tree
            .entries
            .iter()
            .find(|e| e.kind == EntryKind::User)
            .expect("a user entry")
            .id
            .clone();

        assert!(local.navigate_auto(&session.id, &first_user).await);
        let after = local.tree_snapshot(&session.id).expect("tree");
        assert!(
            after.entries.iter().any(|e| e.kind == EntryKind::BranchSummary),
            "navigation added a branch-summary node"
        );
        // The abandoned work is carried forward into the rebuilt context.
        let ctx = after.context();
        assert!(ctx.iter().any(|m| m.text.contains("bye")), "summary present in context");

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn resume_unknown_session_is_an_error() {
        let provider = OneShot { turns: Mutex::new(VecDeque::new()) };
        let agent = Arc::new(AgentLoop::new(Box::new(provider), "gpt-4o"));
        let local = LocalProvider::new(agent, "/tmp");
        let err = local.resume_session("nope").await.unwrap_err();
        assert!(matches!(err, ProviderError::SessionNotFound(_)));
        assert!(!local.capabilities().native_fork);
    }
}
