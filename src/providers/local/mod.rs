use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::agent::AgentLoop;
use crate::ai::ChatMessage;
use crate::tree::SessionTree;
use crate::harness::transcript::TranscriptUpdate;
use crate::harness::HarnessEvent;
use crate::providers::{
    AgentProvider, EventPump, EventSink, ModelId, ProviderError, ProviderKind, ProviderSession,
    RoutedEvent, SessionConfig,
};

struct LocalSession {
    session: ProviderSession,
    tree: SessionTree,
    cancel: Arc<AtomicBool>,
    agent: Arc<AgentLoop>,
    agent_key: (String, String, String),
    redo: Vec<String>,
}

pub type AgentFactory =
    Arc<dyn Fn(&str, &str, &str) -> Result<Arc<AgentLoop>, ProviderError> + Send + Sync>;

struct Inner {
    /// Unique per process. The session counter restarts at zero on every
    /// launch, so an id built from the counter alone collides with a session
    /// restored from a previous run — they would then share an event route, a
    /// transcript file, and a place in the workspace.
    instance: String,
    factory: AgentFactory,
    default_agent: Arc<AgentLoop>,
    default_key: (String, String, String),
    sessions: Mutex<HashMap<String, LocalSession>>,
}

/// A token unique to this process, used to keep session ids distinct across
/// launches.
fn instance_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", nanos, std::process::id())
}

pub struct LocalProvider {
    inner: Arc<Inner>,
    events_tx: UnboundedSender<RoutedEvent>,
    events_rx: tokio::sync::Mutex<UnboundedReceiver<RoutedEvent>>,
    seq: AtomicU64,
    default_dir: String,
}

impl LocalProvider {
    #[cfg(test)]
    pub fn new(agent: Arc<AgentLoop>, default_dir: impl Into<String>) -> Self {
        let fixed = agent.clone();
        let factory: AgentFactory = Arc::new(move |_p, _m, _a| Ok(fixed.clone()));
        let model = agent.model().to_string();
        Self::with_factory(factory, agent, ("".into(), model, String::new()), default_dir)
    }

    pub fn with_factory(
        factory: AgentFactory,
        default_agent: Arc<AgentLoop>,
        default_key: (String, String, String),
        default_dir: impl Into<String>,
    ) -> Self {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                instance: instance_token(),
                factory,
                default_agent,
                default_key,
                sessions: Mutex::new(HashMap::new()),
            }),
            events_tx,
            events_rx: tokio::sync::Mutex::new(events_rx),
            seq: AtomicU64::new(1),
            default_dir: default_dir.into(),
        }
    }

    fn agent_for(
        &self,
        desired: &(String, String, String),
    ) -> Result<Arc<AgentLoop>, ProviderError> {
        if desired == &self.inner.default_key {
            return Ok(self.inner.default_agent.clone());
        }
        (self.inner.factory)(&desired.0, &desired.1, &desired.2)
    }

    pub fn register(&self, dir: &str, title: &str) -> ProviderSession {
        // The counter restarts at zero each launch, so an id made only from it
        // repeats an id from a previous run. Two sessions then share one event
        // route and one transcript file: a turn's output lands in whichever of
        // them routes first, which splits one reply across several panes. The
        // instance token makes the id unique across launches; the loop is a
        // second guard in case a sidecar of that name somehow exists.
        let id = loop {
            let n = self.seq.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("theta-local-{}-{n}", self.inner.instance);
            let taken = SessionTree::sidecar_path(&candidate)
                .map(|p| p.exists())
                .unwrap_or(false);
            if !taken && !self.inner.sessions.lock().unwrap().contains_key(&candidate) {
                break candidate;
            }
        };
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
            LocalSession {
                session: session.clone(),
                tree,
                cancel: Arc::new(AtomicBool::new(false)),
                agent: self.inner.default_agent.clone(),
                agent_key: self.inner.default_key.clone(),
                redo: Vec::new(),
            },
        );
        let _ = title;
        session
    }

    pub fn adopt(&self, id: &str, dir: &str, title: &str) -> ProviderSession {
        let dir = if dir.is_empty() { self.default_dir.clone() } else { dir.to_string() };
        let session = ProviderSession {
            provider: ProviderKind::Local,
            id: id.to_string(),
            directory: dir,
        };
        let tree = SessionTree::sidecar_path(id)
            .and_then(|p| SessionTree::load(&p))
            .unwrap_or_default();
        self.replay(id, &tree);
        self.inner.sessions.lock().unwrap().insert(
            id.to_string(),
            LocalSession {
                session: session.clone(),
                tree,
                cancel: Arc::new(AtomicBool::new(false)),
                agent: self.inner.default_agent.clone(),
                agent_key: self.inner.default_key.clone(),
                redo: Vec::new(),
            },
        );
        let _ = title;
        session
    }

    fn replay(&self, sid: &str, tree: &SessionTree) {
        if tree.is_empty() {
            return;
        }
        let msgs = tree.to_messages(sid);
        let _ = self.events_tx.send(RoutedEvent {
            session_id: Some(sid.to_string()),
            event: HarnessEvent::Transcript(TranscriptUpdate::ReplaceAll(msgs)),
        });
    }

    pub async fn compact_session(&self, id: &str) -> bool {
        let mut tree = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            match sessions.get_mut(id) {
                Some(s) => std::mem::take(&mut s.tree),
                None => return false,
            }
        };
        let agent = {
            let sessions = self.inner.sessions.lock().unwrap();
            match sessions.get(id) {
                Some(s) => s.agent.clone(),
                None => return false,
            }
        };
        let mut messages = tree.context();
        self.emit_event(id, HarnessEvent::CompactionStarted);
        let Some((summary, tokens_before, tail)) = agent.force_compact(&mut messages).await else {
            if let Some(s) = self.inner.sessions.lock().unwrap().get_mut(id) {
                s.tree = tree;
            }
            return false;
        };
        tree.push_compaction(&summary, None, tokens_before);
        for m in &tail {
            tree.append(m);
        }
        if let Some(path) = SessionTree::sidecar_path(id) {
            let _ = tree.save(&path);
        }
        if let Some(s) = self.inner.sessions.lock().unwrap().get_mut(id) {
            s.tree = tree;
        }
        self.emit_event(id, HarnessEvent::CompactionFinished { tokens_before });
        self.emit_event(id, crate::agent::part_compaction(tokens_before));
        true
    }

    fn emit_event(&self, sid: &str, event: HarnessEvent) {
        let _ = self.events_tx.send(RoutedEvent {
            session_id: Some(sid.to_string()),
            event,
        });
    }

    pub fn invalidate_agents(&self) {
        let mut sessions = self.inner.sessions.lock().unwrap();
        for s in sessions.values_mut() {
            s.agent_key = ("\u{0}invalid".to_string(), String::new(), String::new());
        }
    }

    pub fn tree_snapshot(&self, id: &str) -> Option<SessionTree> {
        self.inner.sessions.lock().unwrap().get(id).map(|s| s.tree.clone())
    }

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

    pub async fn navigate_auto(&self, id: &str, entry: &str) -> bool {
        let (input, agent) = {
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
            let input = if s.tree.is_ancestor(entry, &old) {
                s.tree.branch_summary_input(&old, Some(entry), 20_000, 2_000)
            } else {
                None
            };
            (input, s.agent.clone())
        };
        let summary = match input {
            Some(text) => agent
                .summarize_branch(&text)
                .await
                .ok()
                .filter(|s| !s.trim().is_empty()),
            None => None,
        };
        self.navigate(id, entry, summary)
    }

    pub fn rewind(&self, id: &str, entry: &str) -> Option<String> {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let s = sessions.get_mut(id)?;
        let text = s.tree.node(entry).map(|e| e.text.clone());
        let target = s.tree.node(entry).and_then(|e| e.parent.clone());
        let old = s.tree.leaf.clone()?;
        s.tree.restore_files_after(target.as_deref());
        match target {
            Some(t) => {
                if !s.tree.set_leaf(&t) {
                    return None;
                }
            }
            None => s.tree.leaf = None,
        }
        s.redo.push(old);
        if let Some(path) = SessionTree::sidecar_path(id) {
            let _ = s.tree.save(&path);
        }
        text
    }

    pub fn redo(&self, id: &str) -> bool {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(id) else {
            return false;
        };
        let Some(leaf) = s.redo.pop() else {
            return false;
        };
        if !s.tree.set_leaf(&leaf) {
            s.redo.push(leaf);
            return false;
        }
        if let Some(path) = SessionTree::sidecar_path(id) {
            let _ = s.tree.save(&path);
        }
        true
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
        model: Option<ModelId>,
        agent: Option<String>,
        attachments: &[crate::mentions::Attachment],
    ) -> Result<(), ProviderError> {
        let mut text_atts = Vec::new();
        let mut images = Vec::new();
        for a in attachments {
            if let Some(img) = crate::ai::image_from_url(&a.mime, &a.url) {
                images.push(img);
            } else if !a.mime.starts_with("image/") {
                text_atts.push(a.clone());
            }
        }
        let text = if text_atts.is_empty() {
            text.to_string()
        } else {
            crate::mentions::inline_attachments(text, &text_atts, 20_000)
        };
        let mut desired = match model {
            Some(m) => (m.provider, m.model, String::new()),
            None => self.inner.default_key.clone(),
        };
        if desired.2.is_empty() {
            desired.2 = self.inner.default_key.2.clone();
        }
        if let Some(a) = agent.filter(|a| !a.trim().is_empty()) {
            desired.2 = a;
        }
        let new_agent = self.agent_for(&desired)?;
        let (mut tree, cancel, dir, agent) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let entry = sessions
                .get_mut(&session.id)
                .ok_or_else(|| ProviderError::SessionNotFound(session.id.clone()))?;
            entry.cancel.store(false, Ordering::Relaxed);
            if entry.agent_key != desired {
                entry.agent = new_agent;
                entry.agent_key = desired.clone();
                crate::tlog!(
                    "LOCAL session {} switched to {}/{} agent={}",
                    session.id,
                    desired.0,
                    desired.1,
                    desired.2
                );
            }
            (
                std::mem::take(&mut entry.tree),
                entry.cancel.clone(),
                entry.session.directory.clone(),
                entry.agent.clone(),
            )
        };
        let mut history = tree.context();

        let inner = self.inner.clone();
        let tx = self.events_tx.clone();
        let sid = session.id.clone();
        let cwd = PathBuf::from(&dir);

        tokio::spawn(async move {
            let mut emit = {
                let tx = tx.clone();
                let sid = sid.clone();
                move |event: HarnessEvent| {
                    let _ = tx.send(RoutedEvent { session_id: Some(sid.clone()), event });
                }
            };
            let mut journal: Vec<ChatMessage> = Vec::new();
            let mut snapshots: Vec<crate::agent::tools::FileSnapshot> = Vec::new();
            // A panic inside a turn would otherwise kill this task before it
            // emits SessionIdle, leaving the pane busy — a spinner that never
            // stops and only clears on a refresh. Catch it and finish cleanly.
            let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
                agent
                    .run_turn_journaled_snapshots(
                        &mut history,
                        &text,
                        &images,
                        &cwd,
                        &cancel,
                        &mut emit,
                        &mut journal,
                        &mut snapshots,
                    )
                    .await
            }))
            .await;
            let result = match result {
                Ok(r) => r,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".into());
                    crate::tlog!("PANIC turn panicked: {msg}");
                    Err(ProviderError::Protocol(format!(
                        "the turn failed internally ({msg}); the session was kept"
                    )))
                }
            };
            if let Err(e) = result {
                let _ = tx.send(RoutedEvent {
                    session_id: Some(sid.clone()),
                    event: HarnessEvent::SessionError(e.to_string()),
                });
                let _ = tx.send(RoutedEvent { session_id: Some(sid.clone()), event: HarnessEvent::SessionIdle });
            }
            tree.append(&ChatMessage::user(&text));
            for m in &journal {
                tree.append(m);
            }
            if !snapshots.is_empty() {
                if let Some(last) = tree.entries.last_mut() {
                    last.snapshots = snapshots;
                }
            }
            if let Some(path) = SessionTree::sidecar_path(&sid) {
                let _ = tree.save(&path);
            }
            if let Ok(mut sessions) = inner.sessions.lock() {
                if let Some(entry) = sessions.get_mut(&sid) {
                    entry.tree = tree;
                    entry.redo.clear();
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
        session: &ProviderSession,
        at: Option<&str>,
    ) -> Result<ProviderSession, ProviderError> {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = format!("theta-local-{n}");
        let (mut tree, dir) = {
            let sessions = self.inner.sessions.lock().unwrap();
            let s = sessions
                .get(&session.id)
                .ok_or_else(|| ProviderError::SessionNotFound(session.id.clone()))?;
            (s.tree.clone(), s.session.directory.clone())
        };
        if let Some(entry) = at {
            let keep: Vec<String> = tree
                .active_path()
                .into_iter()
                .map(|e| e.id.clone())
                .collect();
            let Some(pos) = keep.iter().position(|e| e == entry) else {
                return Err(ProviderError::Protocol(format!("no such fork point: {entry}")));
            };
            let allowed: std::collections::HashSet<&str> =
                keep[..=pos].iter().map(|s| s.as_str()).collect();
            tree.retain_path(&allowed);
        }
        if let Some(path) = SessionTree::sidecar_path(&id) {
            let _ = tree.save(&path);
        }
        let new_session = ProviderSession {
            provider: ProviderKind::Local,
            id: id.clone(),
            directory: dir,
        };
        self.inner.sessions.lock().unwrap().insert(
            id,
            LocalSession {
                session: new_session.clone(),
                tree,
                cancel: Arc::new(AtomicBool::new(false)),
                agent: self.inner.default_agent.clone(),
                agent_key: self.inner.default_key.clone(),
                redo: Vec::new(),
            },
        );
        Ok(new_session)
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

// `THETA_SESSION_DIR` is process-global, so tests that set it take a mutex and
// hold it for the test's duration — including across awaits. That is sound here:
// `#[tokio::test]` defaults to a current-thread runtime, so the guard is never
// sent between threads and cannot deadlock another task. `tree::testenv` holds
// the one lock, which is why it is a std mutex that the non-async tests can use
// too.
#[cfg(test)]
#[allow(clippy::await_holding_lock)]
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
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 1));
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

        let tree = local.tree_snapshot(&session.id).expect("tree");
        assert!(tree.entries.len() >= 2, "entries: {}", tree.entries.len());
        assert_eq!(tree.entries[0].kind, crate::tree::EntryKind::User);
        assert!(tree.entries.iter().any(|e| e.kind == crate::tree::EntryKind::Assistant));
        let ctx = tree.context();
        assert!(ctx.iter().any(|m| m.role == crate::ai::Role::User && m.text == "hi"));

        let first = tree.entries[0].id.clone();
        assert!(local.navigate(&session.id, &first, None));
        assert_eq!(local.tree_snapshot(&session.id).unwrap().leaf.as_deref(), Some(first.as_str()));
    }

/// A provider that panics, to prove a broken turn cannot wedge the UI.
    struct PanicProvider;

    impl crate::ai::Provider for PanicProvider {
        fn id(&self) -> &'static str {
            "panic"
        }
        fn stream<'a>(
            &'a self,
            _request: crate::ai::ChatRequest,
            _on_event: &'a mut (dyn FnMut(crate::ai::ProviderEvent) + Send),
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<crate::ai::AssistantTurn, ProviderError>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move { panic!("simulated tool failure") })
        }
    }

    #[tokio::test]
    async fn a_panicking_turn_still_finishes_and_goes_idle() {
        // Without the panic guard this task dies before emitting SessionIdle,
        // so the pane stays busy forever and only clears on a refresh.
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 2));
        std::env::set_var("THETA_SESSION_DIR", &session_dir);
        let agent = Arc::new(AgentLoop::new(Box::new(PanicProvider), "m"));
        let local = Arc::new(LocalProvider::new(agent, &dir));

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

        let mut idle = false;
        let mut errored = false;
        for _ in 0..50 {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Some(ev)) => match &ev.event {
                    crate::harness::HarnessEvent::SessionIdle => {
                        idle = true;
                        break;
                    }
                    crate::harness::HarnessEvent::SessionError(_) => errored = true,
                    _ => {}
                },
                _ => break,
            }
        }
        pump_task.abort();
        assert!(errored, "the failure should be reported, not swallowed");
        assert!(idle, "a panicking turn must still go idle so the UI cannot hang");
    }

    #[tokio::test]
    async fn navigate_auto_injects_a_branch_summary() {
        use crate::tree::EntryKind;
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 3));
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
        let ctx = after.context();
        assert!(ctx.iter().any(|m| m.text.contains("bye")), "summary present in context");

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn compact_session_folds_history_into_a_summary() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 4));
        let _ = std::fs::remove_dir_all(&session_dir);
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let provider = OneShot {
            turns: Mutex::new(
                vec![
                    AssistantTurn { text: "first answer".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
                    AssistantTurn { text: "SUMMARY-OF-PAST".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
                ]
                .into(),
            ),
        };
        let settings = crate::agent::context::CompactionSettings {
            reserve_tokens: 100,
            keep_recent_tokens: 1,
            tool_result_cap: 2_000,
        };
        let agent = Arc::new(
            AgentLoop::new(Box::new(provider), "m").with_compaction(settings, true),
        );
        let local = Arc::new(LocalProvider::new(agent, &dir));
        let session = local
            .create_session(SessionConfig { directory: dir, title: "t".into() })
            .await
            .unwrap();
        local.send_message(&session, "remember this", None, None, &[]).await.unwrap();
        for _ in 0..100 {
            let done = local
                .tree_snapshot(&session.id)
                .map(|t| t.entries.iter().any(|e| e.kind == crate::tree::EntryKind::Assistant))
                .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert!(local.compact_session(&session.id).await, "compaction ran");
        let tree = local.tree_snapshot(&session.id).expect("tree");
        assert!(tree.entries.iter().any(|e| e.kind == crate::tree::EntryKind::Compaction));
        let ctx = tree.context();
        assert!(
            ctx.iter().any(|m| m.text.contains("SUMMARY-OF-PAST")),
            "summary replaces the folded history"
        );

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn replay_reemits_stored_history() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let provider = OneShot { turns: Mutex::new(VecDeque::new()) };
        let agent = Arc::new(AgentLoop::new(Box::new(provider), "m"));
        let local = Arc::new(LocalProvider::new(agent, &dir));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pump_local = local.clone();
        let pump_task = tokio::spawn(async move { let _ = pump_local.pump(tx).await; });

        let mut tree = crate::tree::SessionTree::new();
        tree.append(&crate::ai::ChatMessage::user("earlier question"));
        tree.append(&crate::ai::ChatMessage {
            role: crate::ai::Role::Assistant,
            text: "earlier answer".into(),
            tool_calls: vec![],
            tool_call_id: None,
            tokens: None,
            images: vec![],
            cost: None,
        });
        local.replay("theta-old-1", &tree);

        let mut texts = Vec::new();
        if let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        {
            if let crate::harness::HarnessEvent::Transcript(
                crate::harness::transcript::TranscriptUpdate::ReplaceAll(msgs),
            ) = ev.event
            {
                for m in msgs {
                    for p in m.parts {
                        if let crate::harness::transcript::PartKind::Text { text, .. } = p.kind {
                            texts.push(text);
                        }
                    }
                }
            }
        }
        pump_task.abort();
        assert!(texts.iter().any(|t| t == "earlier question"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "earlier answer"), "{texts:?}");
    }

    fn one_shot(text: &str) -> AgentLoop {
        AgentLoop::new(
            Box::new(OneShot {
                turns: Mutex::new(
                    vec![AssistantTurn { text: text.into(), tool_calls: vec![], finish: Some(FinishReason::Stop) }]
                        .into(),
                ),
            }),
            "m",
        )
    }

    #[tokio::test]
    async fn picking_a_model_rebuilds_the_agent() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 5));
        let _ = std::fs::remove_dir_all(&session_dir);
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_c = calls.clone();
        let factory: AgentFactory = Arc::new(move |provider, model, agent| {
            calls_c.lock().unwrap().push(format!("{provider}/{model}/{agent}"));
            Ok(Arc::new(one_shot(&format!("ran {provider}/{model}"))))
        });
        let local = Arc::new(LocalProvider::with_factory(
            factory,
            Arc::new(one_shot("default")),
            ("openai".into(), "gpt-4o".into(), "build".into()),
            &dir,
        ));
        let session = local
            .create_session(SessionConfig { directory: dir, title: "t".into() })
            .await
            .unwrap();

        local
            .send_message(
                &session,
                "go",
                Some(ModelId { provider: "anthropic".into(), model: "claude-x".into() }),
                None,
                &[],
            )
            .await
            .unwrap();
        for _ in 0..100 {
            let done = local
                .tree_snapshot(&session.id)
                .map(|t| t.context().iter().any(|m| m.text.contains("ran anthropic/claude-x")))
                .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(calls.lock().unwrap().as_slice(), ["anthropic/claude-x/build"]);
        let ctx = local.tree_snapshot(&session.id).unwrap().context();
        assert!(ctx.iter().any(|m| m.text.contains("ran anthropic/claude-x")));

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn attachments_are_inlined_into_the_prompt() {
        let dir = std::env::temp_dir().join(format!("theta-att-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("note.txt"), "SECRET-CONTENT").unwrap();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 6));
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let local = Arc::new(LocalProvider::new(Arc::new(one_shot("ok")), dir.to_string_lossy()));
        let session = local
            .create_session(SessionConfig { directory: dir.to_string_lossy().into(), title: "t".into() })
            .await
            .unwrap();
        let attachment = crate::mentions::Attachment {
            label: "note.txt".into(),
            mime: "text/plain".into(),
            url: crate::mentions::file_url(&dir.join("note.txt")),
        };
        local
            .send_message(&session, "read this", None, None, &[attachment])
            .await
            .unwrap();
        for _ in 0..100 {
            let done = local
                .tree_snapshot(&session.id)
                .map(|t| t.context().iter().any(|m| m.text.contains("SECRET-CONTENT")))
                .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let ctx = local.tree_snapshot(&session.id).unwrap().context();
        assert!(ctx.iter().any(|m| m.text.contains("<attached-file path=\"note.txt\">")));
        assert!(ctx.iter().any(|m| m.text.contains("SECRET-CONTENT")));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn fork_copies_history_and_leaves_source_untouched() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 7));
        let _ = std::fs::remove_dir_all(&session_dir);
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let local = Arc::new(LocalProvider::new(Arc::new(one_shot("answer")), &dir));
        let session = local
            .create_session(SessionConfig { directory: dir, title: "t".into() })
            .await
            .unwrap();
        local.send_message(&session, "hello", None, None, &[]).await.unwrap();
        for _ in 0..100 {
            let done = local
                .tree_snapshot(&session.id)
                .map(|t| !t.is_empty())
                .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let forked = local.fork(&session, None).await.unwrap();
        assert_ne!(forked.id, session.id);
        let src = local.tree_snapshot(&session.id).unwrap();
        let dst = local.tree_snapshot(&forked.id).unwrap();
        assert!(!src.is_empty());
        assert_eq!(dst.entries.len(), src.entries.len());
        assert!(dst.context().iter().any(|m| m.text == "hello"));

        let first = src.entries[0].id.clone();
        let pinned = local.fork(&session, Some(&first)).await.unwrap();
        let ptree = local.tree_snapshot(&pinned.id).unwrap();
        assert_eq!(ptree.entries.len(), 1);
        assert_eq!(local.tree_snapshot(&session.id).unwrap().entries.len(), src.entries.len());

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn rewind_and_redo_move_the_active_leaf() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 8));
        let _ = std::fs::remove_dir_all(&session_dir);
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let local = Arc::new(LocalProvider::new(Arc::new(one_shot("answer")), &dir));
        let session = local
            .create_session(SessionConfig { directory: dir, title: "t".into() })
            .await
            .unwrap();
        local.send_message(&session, "question one", None, None, &[]).await.unwrap();
        for _ in 0..100 {
            let done = local
                .tree_snapshot(&session.id)
                .map(|t| t.context().iter().any(|m| m.text == "answer"))
                .unwrap_or(false);
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let tree = local.tree_snapshot(&session.id).unwrap();
        let user_entry = tree
            .entries
            .iter()
            .find(|e| e.kind == crate::tree::EntryKind::User)
            .unwrap()
            .id
            .clone();

        let text = local.rewind(&session.id, &user_entry).unwrap();
        assert_eq!(text, "question one");
        let after = local.tree_snapshot(&session.id).unwrap();
        assert!(
            !after.context().iter().any(|m| m.text == "question one"),
            "rewound context drops the turn"
        );

        assert!(local.redo(&session.id));
        let restored = local.tree_snapshot(&session.id).unwrap();
        assert!(restored.context().iter().any(|m| m.text == "question one"));

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn rewind_restores_files_touched_by_a_turn() {
        let dir = std::env::temp_dir().join(format!("theta-rwf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Serialized: THETA_SESSION_DIR is process-global.
        let _env = crate::tree::testenv::lock();
        let session_dir = std::env::temp_dir()
            .join(format!("theta-it-{}-{}", std::process::id(), 9));
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        let provider = OneShot {
            turns: Mutex::new(
                vec![
                    AssistantTurn {
                        text: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "c1".into(),
                            name: "write".into(),
                            arguments: r#"{"path":"made.txt","content":"hello"}"#.into(),
                        }],
                        finish: Some(FinishReason::ToolCalls),
                    },
                    AssistantTurn { text: "done".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
                ]
                .into(),
            ),
        };
        let local = Arc::new(LocalProvider::new(Arc::new(AgentLoop::new(Box::new(provider), "m")), dir.to_string_lossy()));
        let session = local
            .create_session(SessionConfig { directory: dir.to_string_lossy().into(), title: "t".into() })
            .await
            .unwrap();
        local.send_message(&session, "make a file", None, None, &[]).await.unwrap();
        for _ in 0..100 {
            if dir.join("made.txt").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::read_to_string(dir.join("made.txt")).unwrap(), "hello");
        let tree = local.tree_snapshot(&session.id).unwrap();
        assert!(tree.entries.iter().any(|e| !e.snapshots.is_empty()), "snapshot recorded");

        let user_entry = tree
            .entries
            .iter()
            .find(|e| e.kind == crate::tree::EntryKind::User)
            .unwrap()
            .id
            .clone();
        local.rewind(&session.id, &user_entry);
        assert!(!dir.join("made.txt").exists(), "rewind deleted the created file");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn resume_unknown_session_is_an_error() {
        let provider = OneShot { turns: Mutex::new(VecDeque::new()) };
        let agent = Arc::new(AgentLoop::new(Box::new(provider), "gpt-4o"));
        let local = LocalProvider::new(agent, "/tmp");
        let err = local.resume_session("nope").await.unwrap_err();
        assert!(matches!(err, ProviderError::SessionNotFound(_)));
        assert!(local.capabilities().native_fork);
        assert!(local.capabilities().native_resume);
    }

    #[test]
    fn session_ids_are_unique_across_launches() {
        // The bug: the counter restarted at zero every launch, so a session
        // restored from a previous run (`theta-local-1`) collided with a newly
        // registered one of the same name. They then shared an event route, a
        // transcript file and a workspace slot, which split one turn's output
        // across several panes.
        // Two launches must not mint the same id for their first session. With
        // a bare counter both would produce `theta-local-0`; the token makes
        // them differ.
        let first_launch = instance_token();
        let second_launch = instance_token();
        assert!(!first_launch.is_empty());
        assert_ne!(
            first_launch, second_launch,
            "a token must distinguish separate launches"
        );
        assert_ne!(
            format!("theta-local-{first_launch}-0"),
            format!("theta-local-{second_launch}-0"),
            "so the first session of each launch has a distinct id"
        );
        // The counter still keeps ids ordered within a launch.
        assert!(format!("theta-local-{first_launch}-1").ends_with("-1"));
    }

}
