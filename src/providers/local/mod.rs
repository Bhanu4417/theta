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
use crate::harness::transcript::{Message, Part, PartKind, Role as TRole, TranscriptUpdate};
use crate::harness::HarnessEvent;
use crate::providers::{
    AgentProvider, EventPump, EventSink, ModelId, ProviderError, ProviderKind, ProviderSession,
    RoutedEvent, SessionConfig,
};

struct LocalSession {
    session: ProviderSession,
    tree: SessionTree,
    cancel: Arc<AtomicBool>,
    /// The agent currently serving this session (may change when the user
    /// picks a different model).
    agent: Arc<AgentLoop>,
    /// `(provider, model, agent)` the session's agent was built for.
    agent_key: (String, String, String),
    /// Leaves abandoned by `/undo`, newest last, for `/redo`.
    redo: Vec<String>,
}

/// Builds an agent for a `(provider, model, agent)` triple. The manager supplies
/// a factory so picking a model or agent rebuilds the adapter on demand.
pub type AgentFactory =
    Arc<dyn Fn(&str, &str, &str) -> Result<Arc<AgentLoop>, ProviderError> + Send + Sync>;

/// Everything the adapter needs, shared across its (async) methods.
struct Inner {
    factory: AgentFactory,
    default_agent: Arc<AgentLoop>,
    default_key: (String, String, String),
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
    /// Fixed-agent provider (tests, single-model use).
    pub fn new(agent: Arc<AgentLoop>, default_dir: impl Into<String>) -> Self {
        let fixed = agent.clone();
        let factory: AgentFactory = Arc::new(move |_p, _m, _a| Ok(fixed.clone()));
        let model = agent.model().to_string();
        Self::with_factory(factory, agent, ("".into(), model, String::new()), default_dir)
    }

    /// Provider whose agent can be rebuilt per `(provider, model)` selection.
    pub fn with_factory(
        factory: AgentFactory,
        default_agent: Arc<AgentLoop>,
        default_key: (String, String, String),
        default_dir: impl Into<String>,
    ) -> Self {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
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

    /// Choose (building if needed) the agent for a session keyed by
    /// `(provider, model)`. Caller must hold no lock on `sessions`.
    fn agent_for(
        &self,
        desired: &(String, String, String),
    ) -> Result<Arc<AgentLoop>, ProviderError> {
        // Fast path: the default agent already matches.
        if desired == &self.inner.default_key {
            return Ok(self.inner.default_agent.clone());
        }
        (self.inner.factory)(&desired.0, &desired.1, &desired.2)
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

    /// Adopt an existing on-disk session (resume): load its tree, replay the
    /// visible history into the transcript, and register it under `id`.
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
        // Replace (don't stack on) the hydrated cache: the tree is authoritative.
        let _ = self.events_tx.send(RoutedEvent {
            session_id: Some(id.to_string()),
            event: HarnessEvent::Transcript(TranscriptUpdate::Reset),
        });
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

    /// Re-emit a stored transcript so a resumed pane shows prior turns.
    fn replay(&self, sid: &str, tree: &SessionTree) {
        if tree.is_empty() {
            return;
        }
        let mut n = 0u64;
        for entry in tree.active_path() {
            let (role, text) = match entry.kind {
                crate::tree::EntryKind::User => (TRole::User, entry.text.clone()),
                crate::tree::EntryKind::Assistant => (TRole::Assistant, entry.text.clone()),
                _ => continue,
            };
            if text.trim().is_empty() {
                continue;
            }
            n += 1;
            let message_id = format!("{sid}-hist-{n}");
            let part_id = format!("{message_id}-p1");
            let _ = self.events_tx.send(RoutedEvent {
                session_id: Some(sid.to_string()),
                event: HarnessEvent::Transcript(TranscriptUpdate::MessageMeta(Message {
                    id: message_id.clone(),
                    role,
                    error: None,
                    completed: Some(entry.timestamp_ms),
                    created: Some(entry.timestamp_ms),
                    cost: None,
                    tokens: None,
                    parts: Vec::new(),
                })),
            });
            let _ = self.events_tx.send(RoutedEvent {
                session_id: Some(sid.to_string()),
                event: HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                    id: part_id,
                    message_id,
                    kind: PartKind::Text { text, synthetic: false },
                })),
            });
        }
    }

    /// Force a compaction of a live session (the `/compact` command). Rebuilds
    /// the tree as `[compaction summary] + retained tail` and persists it.
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
            // Nothing to compact; put the untouched tree back.
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

    /// Force every session's agent to be rebuilt on its next turn (used after
    /// credentials change, so a newly added key takes effect).
    pub fn invalidate_agents(&self) {
        let mut sessions = self.inner.sessions.lock().unwrap();
        for s in sessions.values_mut() {
            // A key no real selection can equal, so the next send rebuilds.
            s.agent_key = ("\u{0}invalid".to_string(), String::new(), String::new());
        }
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
            // Only summarize when jumping to an ancestor (backwards).
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

    /// `/undo`: move the active leaf to just before `entry`, remembering the
    /// old leaf so `/redo` can restore it. The abandoned branch stays on disk
    /// (history is never deleted). Returns the rewound user text when `entry`
    /// is a user entry, so the UI can put it back in the chatbox.
    pub fn rewind(&self, id: &str, entry: &str) -> Option<String> {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let s = sessions.get_mut(id)?;
        let text = s.tree.node(entry).map(|e| e.text.clone());
        let target = s.tree.node(entry).and_then(|e| e.parent.clone());
        let old = s.tree.leaf.clone()?;
        // Restore the working tree to the state before the rewound turns.
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

    /// Rewind to the most recent user entry (used by the OpenCode-style
    /// `/undo` path; the rewind overlay uses [`Self::rewind`] directly).
    pub fn rewind_user(&self, id: &str, _message_id: &str) -> bool {
        let entry = {
            let sessions = self.inner.sessions.lock().unwrap();
            let Some(s) = sessions.get(id) else {
                return false;
            };
            s.tree
                .entries
                .iter()
                .rev()
                .find(|e| e.kind == crate::tree::EntryKind::User)
                .map(|e| e.id.clone())
        };
        match entry {
            Some(e) => self.rewind(id, &e).is_some(),
            None => false,
        }
    }

    /// `/redo`: restore the leaf most recently abandoned by [`Self::rewind`].
    pub fn redo(&self, id: &str) -> bool {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let Some(s) = sessions.get_mut(id) else {
            return false;
        };
        let Some(leaf) = s.redo.pop() else {
            return false;
        };
        if !s.tree.set_leaf(&leaf) {
            // The leaf no longer exists; push it back so state stays consistent.
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
        // Split attachments: images become vision content parts; everything
        // else is inlined as text into the prompt.
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
        // Resolve the model the user picked and rebuild the session's agent if
        // it changed. This is what makes the model picker work on the local
        // backend.
        let mut desired = match model {
            Some(m) => (m.provider, m.model, String::new()),
            None => self.inner.default_key.clone(),
        };
        // Fall back to the session's default agent when the caller didn't pick.
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
        // Rebuild the prompt from the active branch, then run.
        let mut history = tree.context();

        let inner = self.inner.clone();
        let tx = self.events_tx.clone();
        let sid = session.id.clone();
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
            let mut snapshots: Vec<crate::agent::tools::FileSnapshot> = Vec::new();
            let result = agent
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
            // Attach the turn's file pre-images to its last node so `/undo` can
            // restore the working tree.
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

    /// Provider-native fork: duplicate the active branch into a new session id
    /// (the source is untouched). `at` pins the copy to an ancestor entry.
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
            // Pin the fork: drop everything after `entry` on the active path.
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
    async fn compact_session_folds_history_into_a_summary() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let session_dir =
            std::env::temp_dir().join(format!("theta-cs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&session_dir);
        std::env::set_var("THETA_SESSION_DIR", &session_dir);

        // The provider answers both the turn and the summarization request.
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
        });
        local.replay("theta-old-1", &tree);

        let mut texts = Vec::new();
        for _ in 0..4 {
            let Ok(Some(ev)) =
                tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
            else {
                break;
            };
            if let crate::harness::HarnessEvent::Transcript(
                crate::harness::transcript::TranscriptUpdate::Part(p),
            ) = ev.event
            {
                if let crate::harness::transcript::PartKind::Text { text, .. } = p.kind {
                    texts.push(text);
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
        let session_dir =
            std::env::temp_dir().join(format!("theta-switch-{}", std::process::id()));
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
        let session_dir = std::env::temp_dir().join(format!("theta-att-s-{}", std::process::id()));
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
        let session_dir = std::env::temp_dir().join(format!("theta-fork-{}", std::process::id()));
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

        // Pin a fork at the first (user) entry.
        let first = src.entries[0].id.clone();
        let pinned = local.fork(&session, Some(&first)).await.unwrap();
        let ptree = local.tree_snapshot(&pinned.id).unwrap();
        assert_eq!(ptree.entries.len(), 1);
        // Source is unchanged.
        assert_eq!(local.tree_snapshot(&session.id).unwrap().entries.len(), src.entries.len());

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[tokio::test]
    async fn rewind_and_redo_move_the_active_leaf() {
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let session_dir = std::env::temp_dir().join(format!("theta-rw-{}", std::process::id()));
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
        let session_dir = std::env::temp_dir().join(format!("theta-rwf-s-{}", std::process::id()));
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
        // The pre-image (file did not exist) is recorded on the turn's node.
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
        // The local backend now advertises fork/resume/permissions/questions.
        assert!(local.capabilities().native_fork);
        assert!(local.capabilities().native_resume);
    }
}
