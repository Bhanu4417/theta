//! Manager: owns per-directory OpenCode servers and bridges async work to the
//! UI event loop. All methods are fire-and-forget; results arrive as
//! `AppEvent`s on the shared channel.

use crate::config::Config;
use crate::events::{AppEvent, ReqId};
use crate::opencode::{Client, GrepMatch, ModelRef};
use crate::providers::local::LocalProvider;
use crate::providers::opencode::OpenCodeProvider;
use crate::providers::{
    AgentProvider, ModelId, ProviderError, ProviderKind, ProviderSession, SessionConfig,
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

struct ServerHandle {
    #[allow(dead_code)]
    base: String,
    client: Client,
    /// Handle to the spawned process. Not killed on drop: with `keep_alive`
    /// (default) servers persist so the next launch reuses them; they are
    /// stopped explicitly by `shutdown_all` when `keep_alive = false`.
    #[allow(dead_code)]
    child: Option<tokio::process::Child>,
}

#[derive(Default)]
struct Inner {
    servers: HashMap<PathBuf, ServerHandle>,
    providers_fetched: HashSet<PathBuf>,
    /// Directories whose pending questions/permissions were already fetched.
    presence_fetched: HashSet<PathBuf>,
    /// Serializes server spawn per directory (prevents double-spawn races).
    spawn_locks: HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>,
}

#[derive(Clone)]
struct ManagerRef {
    tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    cfg: Config,
    inner: Arc<Mutex<Inner>>,
    children: Arc<Mutex<Vec<u32>>>,
    /// Present when `cfg.backend == "local"`: the in-process agent backend.
    local: Option<Arc<LocalProvider>>,
    /// Interactive gates (permissions + questions) for the local backend.
    local_gates: Option<LocalGates>,
}

/// Shared interactive gates wired into every locally-built agent.
#[derive(Clone)]
pub struct LocalGates {
    pub permissions: Arc<crate::agent::permissions::Broker>,
    pub questions: Arc<crate::agent::permissions::QuestionBroker>,
}

impl ManagerRef {
    fn emit(&self, ev: AppEvent) {
        let _ = self.tx.send(ev);
    }

    async fn ensure_server(&self, dir: &Path) -> Result<(String, Client)> {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if !dir.is_dir() {
            crate::tlog!("SERVER dir missing: {}", dir.display());
            anyhow::bail!("directory does not exist: {}", dir.display());
        }
        {
            let inner = self.inner.lock().await;
            if let Some(h) = inner.servers.get(&dir) {
                return Ok((h.base.clone(), h.client.clone()));
            }
        }

        // Serialize spawn per directory so two sessions connecting at once
        // cannot both try to bind the same derived port.
        let lock = {
            let mut inner = self.inner.lock().await;
            inner
                .spawn_locks
                .entry(dir.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        // Another task may have finished spawning while we waited.
        {
            let inner = self.inner.lock().await;
            if let Some(h) = inner.servers.get(&dir) {
                return Ok((h.base.clone(), h.client.clone()));
            }
        }

        // Spawn/reuse outside the inner lock (health wait can take seconds).
        let (base, child) = spawn_server(&self.cfg, &dir).await?;
        if let Some(pid) = child.as_ref().and_then(|c| c.id()) {
            self.children.lock().await.push(pid);
        }
        let client = Client::new(base.clone());

        // Event delivery is part of the provider contract: each adapter owns
        // its native event pump and yields neutral routed events. The manager
        // only supervises reconnects and converts them into `AppEvent`s.
        spawn_event_pump(
            OpenCodeProvider::new(client.clone(), dir.to_string_lossy().to_string()),
            dir.clone(),
            self.tx.clone(),
        );

        let mut inner = self.inner.lock().await;
        inner.servers.insert(
            dir.clone(),
            ServerHandle {
                base: base.clone(),
                client: client.clone(),
                child,
            },
        );
        Ok((base, client))
    }
}

pub struct Manager {
    ref_: ManagerRef,
    req_seq: AtomicU64,
}

impl Manager {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<AppEvent>, cfg: Config) -> Self {
        let mut local_gates = None;
        let local = if cfg.backend == "local" {
            match build_local_provider(&cfg) {
                Ok((provider, gates)) => {
                    let local = Arc::new(provider);
                    local_gates = gates;
                    // Local events carry no workspace; routing is by session id
                    // (an empty dir is a wildcard in `App::route_session`).
                    spawn_event_pump(local.clone(), PathBuf::new(), tx.clone());
                    crate::tlog!("BACKEND local provider ready (model={})", cfg.ai.model);
                    Some(local)
                }
                Err(e) => {
                    crate::tlog!("BACKEND local init failed: {e}; falling back to opencode");
                    eprintln!("theta: local backend unavailable ({e}); using opencode");
                    None
                }
            }
        } else {
            None
        };
        Self {
            ref_: ManagerRef {
                tx,
                cfg,
                inner: Arc::new(Mutex::new(Inner::default())),
                children: Arc::new(Mutex::new(Vec::new())),
                local,
                local_gates,
            },
            req_seq: AtomicU64::new(1),
        }
    }

    /// True when the configured backend is Theta's own loop.
    pub fn is_local(&self) -> bool {
        self.ref_.local.is_some()
    }

    /// Guard for server-only operations under the local backend.
    fn local_unsupported(&self, what: &str) -> bool {
        if self.ref_.local.is_some() {
            self.ref_.emit(AppEvent::OpResult {
                ok: false,
                message: format!("{what} is not supported by the local backend yet"),
            });
            true
        } else {
            false
        }
    }

    /// Graceful shutdown: SIGTERM each server so OpenCode can checkpoint its
    /// database (SIGKILL leaves a huge WAL that slows the next boot).
    pub async fn shutdown_all(&self) {
        let pids = self.ref_.children.lock().await.clone();
        if pids.is_empty() {
            return;
        }
        #[cfg(unix)]
        for pid in &pids {
            unsafe {
                libc::kill(*pid as i32, libc::SIGTERM);
            }
        }
        #[cfg(not(unix))]
        {
            let m = self.ref_.clone();
            for pid in &pids {
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .spawn();
            }
            let _ = m;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        #[cfg(unix)]
        for pid in &pids {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
    }

    pub fn tx(&self) -> tokio::sync::mpsc::UnboundedSender<AppEvent> {
        self.ref_.tx.clone()
    }

    /// Request a local session's history tree (emits `TreeLoaded`).
    pub fn local_tree(&self, oc_sid: String) {
        let Some(local) = self.ref_.local.clone() else {
            self.ref_.emit(AppEvent::OpResult {
                ok: false,
                message: "tree view needs backend = \"local\"".into(),
            });
            return;
        };
        match local.tree_snapshot(&oc_sid) {
            Some(tree) => self.ref_.emit(AppEvent::TreeLoaded { oc_sid, tree }),
            None => self.ref_.emit(AppEvent::OpResult {
                ok: false,
                message: "no local session for this pane".into(),
            }),
        }
    }

    /// Synchronously read a local session's history tree (for `/undo`).
    pub fn local_tree_snapshot(&self, oc_sid: &str) -> Option<crate::tree::SessionTree> {
        self.ref_.local.as_ref()?.tree_snapshot(oc_sid)
    }

    /// Rewind a local session to just before `entry`; returns the user text.
    pub fn local_rewind(&self, oc_sid: String, entry: String) -> Option<String> {
        self.ref_.local.as_ref()?.rewind(&oc_sid, &entry)
    }

    /// Restore the leaf most recently abandoned by [`Self::local_rewind`].
    pub fn local_redo(&self, oc_sid: String) -> bool {
        self.ref_.local.as_ref().is_some_and(|l| l.redo(&oc_sid))
    }

    /// Force compaction of a local session (`/compact`).
    pub fn local_compact(&self, oc_sid: String) {
        let Some(local) = self.ref_.local.clone() else {
            self.ref_.emit(AppEvent::OpResult {
                ok: false,
                message: "compaction is only available on the local backend".into(),
            });
            return;
        };
        tokio::spawn(async move {
            let ok = local.compact_session(&oc_sid).await;
            if !ok {
                crate::tlog!("COMPACT local: nothing to compact for {oc_sid}");
            }
        });
    }

    /// Answer an interactive permission request for the local backend.
    pub fn local_permission_reply(&self, _oc_sid: String, id: String, response: String) {
        if let Some(gates) = &self.ref_.local_gates {
            let decision = crate::agent::permissions::decision_for(&response);
            if !gates.permissions.reply(&id, decision) {
                crate::tlog!("PERM local reply for unknown request {id}");
            }
        }
    }

    /// Navigate a local session to an earlier history entry (summarizing the
    /// abandoned branch with the model first).
    pub fn local_navigate(&self, oc_sid: String, entry: String) {
        let Some(local) = self.ref_.local.clone() else {
            return;
        };
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let ok = local.navigate_auto(&oc_sid, &entry).await;
            m.emit(AppEvent::OpResult {
                ok,
                message: if ok {
                    "jumped · branch summarized".into()
                } else {
                    "entry not found".into()
                },
            });
        });
    }

    pub fn next_req(&self) -> ReqId {
        self.req_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Connect (or restore) a Theta session to an OpenCode session.
    pub fn connect_session(
        &self,
        req: ReqId,
        dir: PathBuf,
        name: String,
        oc_sid: Option<String>,
        model: Option<ModelRef>,
        history_limit: u32,
    ) {
        if let Some(local) = self.ref_.local.clone() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
                let dir_s = dir_c.to_string_lossy().to_string();
                let session = match oc_sid.as_deref() {
                    Some(sid) => match local.resume_session(sid).await {
                        Ok(s) => s,
                        Err(_) => local.adopt(sid, &dir_s, &name),
                    },
                    None => local.register(&dir_s, &name),
                };
                m.emit(AppEvent::ServerReady {
                    dir: dir_c.clone(),
                    base: "local".into(),
                });
                m.emit(AppEvent::OcCreated {
                    req,
                    session: session.clone(),
                });
                // Model picker is fed from the built-in catalog.
                let cat = crate::ai::catalog::Catalog::builtin();
                let providers = cat
                    .models()
                    .iter()
                    .map(|s| crate::opencode::ModelEntry {
                        provider_id: s.provider.clone(),
                        model_id: s.id.clone(),
                        label: format!("{}/{}", s.provider, s.id),
                        context_limit: Some(s.context_limit),
                    })
                    .collect();
                let default = Some(ModelRef {
                    provider_id: m.cfg.ai.provider.clone(),
                    model_id: m.cfg.ai.model.clone(),
                });
                m.emit(AppEvent::ProvidersListed {
                    dir: dir_c.clone(),
                    providers,
                    default,
                });
                m.emit(AppEvent::AgentsListed {
                    dir: dir_c,
                    agents: crate::agent::agents::names()
                        .into_iter()
                        .map(|(name, description)| crate::opencode::AgentInfo { name, description })
                        .collect(),
                });
                let _ = (history_limit, model);
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let (_base, client) = match m.ensure_server(&dir_c).await {
                Ok(v) => v,
                Err(e) => {
                    let error = format!("cannot start opencode server: {e}");
                    m.emit(AppEvent::ServerFailed {
                        dir: dir_c.clone(),
                        error: error.clone(),
                    });
                    m.emit(AppEvent::OcCreateFailed { req, error });
                    return;
                }
            };
            m.emit(AppEvent::ServerReady {
                dir: dir_c.clone(),
                base: _base,
            });

            // Session lifecycle goes through the provider adapter so the
            // manager never speaks OpenCode protocol directly.
            let provider =
                OpenCodeProvider::new(client.clone(), dir_c.to_string_lossy().to_string());
            let config = SessionConfig {
                directory: dir_c.to_string_lossy().to_string(),
                title: name.clone(),
            };
            let session = match oc_sid.as_deref() {
                Some(sid) => match provider.resume_session(sid).await {
                    Ok(s) => s,
                    Err(_) => match provider.create_session(config).await {
                        Ok(s) => s,
                        Err(e) => {
                            m.emit(AppEvent::OcCreateFailed {
                                req,
                                error: e.to_string(),
                            });
                            return;
                        }
                    },
                },
                None => match provider.create_session(config).await {
                    Ok(s) => s,
                    Err(e) => {
                        m.emit(AppEvent::OcCreateFailed {
                            req,
                            error: e.to_string(),
                        });
                        return;
                    }
                },
            };
            m.emit(AppEvent::OcCreated {
                req,
                session: session.clone(),
            });

            if let Ok(msgs) = client.messages(&session.id, history_limit).await {
                m.emit(AppEvent::HistoryLoaded {
                    dir: dir_c.clone(),
                    oc_sid: session.id.clone(),
                    msgs,
                });
            }

            // Fetch supporting metadata once per directory, and only hold the
            // inner lock for the set updates — never across network calls, so
            // concurrent sessions cannot serialize behind each other.
            let (do_providers, do_presence) = {
                let mut inner = m.inner.lock().await;
                (
                    inner.providers_fetched.insert(dir_c.clone()),
                    inner.presence_fetched.insert(dir_c.clone()),
                )
            };
            if do_providers {
                if let Ok((providers, default)) = client.providers().await {
                    m.emit(AppEvent::ProvidersListed {
                        dir: dir_c.clone(),
                        providers,
                        default,
                    });
                }
                if let Ok(agents) = client.agents().await {
                    m.emit(AppEvent::AgentsListed {
                        dir: dir_c.clone(),
                        agents,
                    });
                }
                if let Ok(commands) = client.commands().await {
                    m.emit(AppEvent::CommandsListed {
                        dir: dir_c.clone(),
                        commands,
                    });
                }
            }
            // Re-surface anything the agent is waiting on (a refresh restores
            // these instead of losing them).
            if do_presence {
                if let Ok(questions) = client.questions().await {
                    if !questions.is_empty() {
                        m.emit(AppEvent::QuestionsListed {
                            dir: dir_c.clone(),
                            questions,
                        });
                    }
                }
                if let Ok(permissions) = client.permissions().await {
                    if !permissions.is_empty() {
                        m.emit(AppEvent::PermissionsListed {
                            dir: dir_c.clone(),
                            permissions,
                        });
                    }
                }
            }
            let _ = model;
        });
    }

    pub fn send_prompt(
        &self,
        dir: PathBuf,
        oc_sid: String,
        text: String,
        model: Option<ModelRef>,
        agent: Option<String>,
        attachments: Vec<crate::mentions::Attachment>,
    ) {
        if let Some(local) = self.ref_.local.clone() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let session = ProviderSession {
                    provider: ProviderKind::Local,
                    id: oc_sid.clone(),
                    directory: dir.to_string_lossy().to_string(),
                };
                let model_id = model.map(|m| ModelId {
                    provider: m.provider_id,
                    model: m.model_id,
                });
                if let Err(e) = local
                    .send_message(&session, &text, model_id, agent, &attachments)
                    .await
                {
                    m.emit(AppEvent::SendFailed {
                        dir,
                        oc_sid,
                        error: e.to_string(),
                    });
                }
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            crate::tlog!(
                "PROMPT dir={} session={} model={:?} agent={:?} text={}",
                dir.display(),
                oc_sid,
                model,
                agent,
                crate::logging::snippet(&text, 2000)
            );
            let provider =
                OpenCodeProvider::new(client.clone(), dir.to_string_lossy().to_string());
            let session = ProviderSession {
                provider: crate::providers::ProviderKind::OpenCode,
                id: oc_sid.clone(),
                directory: dir.to_string_lossy().to_string(),
            };
            let model_id = model.map(|m| ModelId {
                provider: m.provider_id,
                model: m.model_id,
            });
            match provider
                .send_message(&session, &text, model_id, agent, &attachments)
                .await
            {
                Ok(()) => crate::tlog!("PROMPT ok session={oc_sid}"),
                Err(e) => {
                    crate::tlog!("PROMPT failed session={oc_sid}: {e}");
                    m.emit(AppEvent::SendFailed {
                        dir,
                        oc_sid,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    pub fn abort_session(&self, dir: PathBuf, oc_sid: String) {
        if let Some(local) = self.ref_.local.clone() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let session = ProviderSession {
                    provider: ProviderKind::Local,
                    id: oc_sid.clone(),
                    directory: dir.to_string_lossy().to_string(),
                };
                let _ = local.interrupt(&session).await;
                m.emit(AppEvent::Aborted { dir, oc_sid });
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let provider =
                OpenCodeProvider::new(client, dir.to_string_lossy().to_string());
            let session = ProviderSession {
                provider: crate::providers::ProviderKind::OpenCode,
                id: oc_sid.clone(),
                directory: dir.to_string_lossy().to_string(),
            };
            if provider.interrupt(&session).await.is_ok() {
                m.emit(AppEvent::Aborted { dir, oc_sid });
            }
        });
    }

    pub fn reply_permission(&self, dir: PathBuf, oc_sid: String, pid: String, response: String) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let _ = client.permission_reply(&oc_sid, &pid, &response).await;
        });
    }

    pub fn search_files(&self, req: ReqId, dir: PathBuf, query: String) {
        if self.ref_.local.is_some() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
                let q = query.to_lowercase();
                let paths = crate::fsx::list_dir(&dir_c)
                    .into_iter()
                    .filter(|e| !e.is_dir && e.name.to_lowercase().contains(&q))
                    .map(|e| e.path)
                    .take(100)
                    .collect();
                m.emit(AppEvent::FilesFound { req, paths });
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if let Ok((_, client)) = m.ensure_server(&dir_c).await {
                if let Ok(paths) = client.find_files(&query, 100).await {
                    m.emit(AppEvent::FilesFound { req, paths });
                    return;
                }
            }
            // Local fallback.
            let q = query.to_lowercase();
            let paths = crate::fsx::list_dir(&dir_c)
                .into_iter()
                .filter(|e| !e.is_dir && e.name.to_lowercase().contains(&q))
                .map(|e| e.path)
                .take(100)
                .collect();
            m.emit(AppEvent::FilesFound { req, paths });
        });
    }

    pub fn search_pattern(&self, req: ReqId, dir: PathBuf, pattern: String) {
        if self.ref_.local.is_some() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
                let pat = pattern.clone();
                let matches: Vec<GrepMatch> =
                    tokio::task::spawn_blocking(move || crate::fsx::search_local(&dir_c, &pat, 200))
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|mm| GrepMatch { path: mm.path, line: mm.line, text: mm.text })
                        .collect();
                m.emit(AppEvent::MatchesFound { req, matches });
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if let Ok((_, client)) = m.ensure_server(&dir_c).await {
                if let Ok(matches) = client.find_pattern(&pattern, 200).await {
                    m.emit(AppEvent::MatchesFound { req, matches });
                    return;
                }
            }
            // Local fallback when the server is unavailable.
            let pat = pattern.clone();
            let matches: Vec<GrepMatch> =
                tokio::task::spawn_blocking(move || crate::fsx::search_local(&dir_c, &pat, 200))
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mm| GrepMatch {
                        path: mm.path,
                        line: mm.line,
                        text: mm.text,
                    })
                    .collect();
            m.emit(AppEvent::MatchesFound { req, matches });
        });
    }

    /// List a directory's sessions using an already-running server for
    /// `server_dir`. OpenCode accepts a `directory` override, so one server
    /// can enumerate every folder without spawning more.
    pub fn preload_dir(&self, server_dir: PathBuf, target_dir: PathBuf) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let sd = server_dir.canonicalize().unwrap_or(server_dir);
            let td = target_dir.canonicalize().unwrap_or(target_dir);
            let Ok((_, client)) = m.ensure_server(&sd).await else {
                return;
            };
            if let Ok(mut sessions) = client.list_sessions_in(&td.to_string_lossy()).await {
                sessions.sort_by_key(|s| -s.updated_ms.unwrap_or(0));
                m.emit(AppEvent::SessionsPreloaded {
                    dir: td,
                    sessions,
                });
            }
        });
    }

    /// Fork a session without interrupting the source. `at` pins the copy to
    /// the last stable message so an in-progress turn isn't inherited.
    pub fn fork_session(
        &self,
        dir: PathBuf,
        oc_sid: String,
        source: u32,
        at: Option<String>,
    ) {
        if let Some(local) = self.ref_.local.clone() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let session = ProviderSession {
                    provider: ProviderKind::Local,
                    id: oc_sid.clone(),
                    directory: dir.to_string_lossy().to_string(),
                };
                match local.fork(&session, at.as_deref()).await {
                    Ok(new) => m.emit(AppEvent::OcForked { dir, session: new, source }),
                    Err(e) => m.emit(AppEvent::OcForkFailed {
                        source,
                        error: format!("fork failed: {e}"),
                    }),
                }
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let Ok((_, client)) = m.ensure_server(&dir_c).await else {
                return;
            };
            let provider =
                OpenCodeProvider::new(client, dir_c.to_string_lossy().to_string());
            let session = ProviderSession {
                provider: crate::providers::ProviderKind::OpenCode,
                id: oc_sid.clone(),
                directory: dir_c.to_string_lossy().to_string(),
            };
            match provider.fork(&session, at.as_deref()).await {
                Ok(new) => {
                    m.emit(AppEvent::OcForked {
                        dir: dir_c,
                        session: new,
                        source,
                    });
                }
                Err(e) => {
                    m.emit(AppEvent::OcForkFailed {
                        source,
                        error: format!("fork failed: {e}"),
                    });
                }
            }
        });
    }

    /// Answer a pending agent question.
    pub fn reply_question(&self, dir: PathBuf, id: String, answers: Vec<Vec<String>>) {
        if let Some(gates) = &self.ref_.local_gates {
            let ok = gates.questions.reply(&id, answers);
            self.ref_.emit(AppEvent::OpResult {
                ok,
                message: if ok { "answer sent".into() } else { "answer expired".into() },
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let ok = client.reply_question(&id, answers).await.is_ok();
            m.emit(AppEvent::OpResult {
                ok,
                message: if ok { "answer sent".into() } else { "answer failed".into() },
            });
        });
    }

    /// Reject a pending agent question.
    pub fn reject_question(&self, dir: PathBuf, id: String) {
        if let Some(gates) = &self.ref_.local_gates {
            let ok = gates.questions.reject(&id);
            self.ref_.emit(AppEvent::OpResult {
                ok,
                message: if ok { "question rejected".into() } else { "question expired".into() },
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let ok = client.reject_question(&id).await.is_ok();
            m.emit(AppEvent::OpResult {
                ok,
                message: if ok { "question rejected".into() } else { "reject failed".into() },
            });
        });
    }

    pub fn refresh_providers(&self, dir: PathBuf) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if let Ok((_, client)) = m.ensure_server(&dir_c).await {
                if let Ok((providers, default)) = client.providers().await {
                    m.emit(AppEvent::ProvidersListed {
                        dir: dir_c,
                        providers,
                        default,
                    });
                }
            }
        });
    }

    /// Recent sessions stored on the server for a directory (for /resume).
    pub fn list_server_sessions(&self, req: ReqId, dir: PathBuf) {
        if self.ref_.local.is_some() {
            self.ref_.emit(AppEvent::ServerSessionsListed {
                req,
                dir,
                sessions: Vec::new(),
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if let Ok((_, client)) = m.ensure_server(&dir_c).await {
                if let Ok(mut sessions) = client.list_sessions().await {
                    sessions.sort_by_key(|s| -s_time_updated(s));
                    m.emit(AppEvent::ServerSessionsListed { req, dir: dir_c, sessions });
                }
            }
        });
    }

    pub fn run_command(&self, dir: PathBuf, oc_sid: String, command: String, arguments: String) {
        if self.local_unsupported("commands") {
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            match client.run_command(&oc_sid, &command, &arguments).await {
                Ok(()) => {
                    m.emit(AppEvent::OpResult {
                        ok: true,
                        message: format!("/{command} ok"),
                    });
                }
                Err(e) => {
                    m.emit(AppEvent::OpResult {
                        ok: false,
                        message: format!("/{command}: {e}"),
                    });
                }
            }
        });
    }

    pub fn summarize(&self, dir: PathBuf, oc_sid: String, model: ModelRef) {
        if self.local_unsupported("compact") {
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let ok = client.summarize(&oc_sid, &model).await.is_ok();
            m.emit(AppEvent::OpResult {
                ok,
                message: if ok {
                    "compacting conversation…".into()
                } else {
                    "compact failed".into()
                },
            });
        });
    }

    pub fn revert(&self, dir: PathBuf, oc_sid: String, message_id: String) {
        if let Some(local) = self.ref_.local.clone() {
            // Best effort: OpenCode message ids are meaningless locally, so we
            // fall back to rewinding to the matching user entry when possible.
            if local.rewind_user(&oc_sid, &message_id) {
                self.ref_.emit(AppEvent::OpResult { ok: true, message: "rewound".into() });
            }
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let ok = client.revert(&oc_sid, &message_id).await.is_ok();
            m.emit(AppEvent::OpResult {
                ok,
                message: if ok {
                    "reverted last message".into()
                } else {
                    "undo failed".into()
                },
            });
        });
    }

    pub fn unrevert(&self, dir: PathBuf, oc_sid: String) {
        if let Some(local) = self.ref_.local.clone() {
            if local.redo(&oc_sid) {
                self.ref_.emit(AppEvent::OpResult { ok: true, message: "redone".into() });
            }
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            let ok = client.unrevert(&oc_sid).await.is_ok();
            m.emit(AppEvent::OpResult {
                ok,
                message: if ok {
                    "redo applied".into()
                } else {
                    "nothing to redo".into()
                },
            });
        });
    }

    pub fn share(&self, dir: PathBuf, oc_sid: String, want: bool) {
        if self.ref_.local.is_some() {
            self.ref_.emit(AppEvent::OpResult {
                ok: false,
                message: if want {
                    "the local backend has no share links; use /export instead".into()
                } else {
                    "nothing to unshare on the local backend".into()
                },
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            if want {
                match client.share(&oc_sid).await {
                    Ok(Some(url)) => {
                        m.emit(AppEvent::OpResult {
                            ok: true,
                            message: format!("shared: {url}"),
                        });
                    }
                    Ok(None) => {
                        m.emit(AppEvent::OpResult {
                            ok: true,
                            message: "shared".into(),
                        });
                    }
                    Err(e) => {
                        m.emit(AppEvent::OpResult {
                            ok: false,
                            message: format!("share failed: {e}"),
                        });
                    }
                }
            } else {
                let ok = client.unshare(&oc_sid).await.is_ok();
                m.emit(AppEvent::OpResult {
                    ok,
                    message: if ok { "unshared".into() } else { "unshare failed".into() },
                });
            }
        });
    }

    pub fn load_file(&self, req: ReqId, dir: PathBuf, path: String) {
        if self.ref_.local.is_some() {
            let m = self.ref_.clone();
            tokio::spawn(async move {
                let content = std::fs::read_to_string(&path).ok();
                m.emit(AppEvent::FileLoaded { req, path, content, diff: None });
            });
            return;
        }
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if let Ok((_, client)) = m.ensure_server(&dir_c).await {
                if let Ok((content, diff)) = client.file_content(&path).await {
                    m.emit(AppEvent::FileLoaded {
                        req,
                        path,
                        content,
                        diff,
                    });
                    return;
                }
            }
            let content = std::fs::read_to_string(&path).ok();
            m.emit(AppEvent::FileLoaded {
                req,
                path,
                content,
                diff: None,
            });
        });
    }
}

/// Reuse a healthy server already on the derived port, else spawn one.
/// Resolve the configured binary to a real executable — shell wrappers
/// (mise/asdf shims) can hang when spawned from a TUI, and wrappers like
/// `mise use -g` mutate global state on every launch.
async fn resolve_binary(configured: &str) -> String {
    use tokio::process::Command;
    if Path::new(configured).is_absolute() {
        return configured.to_string();
    }
    let Ok(out) = Command::new("which").arg("-a").arg(configured).output().await else {
        return configured.to_string();
    };
    let candidates: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // Use the SAME binary the shell/`opencode` CLI resolves to (first on
    // PATH), so Theta and the user's terminal agree on the version. Skip
    // shell wrappers (mise/asdf shims) — they can hang or mutate state when
    // spawned from a TUI.
    let is_script = |p: &str| -> bool {
        std::fs::File::open(p)
            .and_then(|mut f| {
                use std::io::Read;
                let mut b = [0u8; 2];
                f.read_exact(&mut b)?;
                Ok(b == *b"#!")
            })
            .unwrap_or(false)
    };
    for cand in &candidates {
        if !is_script(cand) {
            return cand.clone();
        }
    }
    // Fallback: the mise installs layout (<installs>/<tool>/latest/<tool>).
    if let Some(home) = dirs::home_dir() {
        let mise = home
            .join(".local/share/mise/installs")
            .join(configured)
            .join("latest")
            .join(configured);
        if mise.exists() {
            return mise.to_string_lossy().to_string();
        }
    }
    configured.to_string()
}

async fn spawn_server(cfg: &Config, dir: &Path) -> Result<(String, Option<tokio::process::Child>)> {
    let binary = resolve_binary(&cfg.opencode.binary).await;
    let port = port_for_dir(dir, cfg.opencode.port_base);

    let candidate = Client::new(format!("http://127.0.0.1:{port}"));
    if candidate.health().await.is_ok() {
        if let Ok(server_dir) = candidate.path_info().await {
            if same_dir(&server_dir, dir) {
                crate::tlog!("SERVER reuse port={port} dir={}", dir.display());
                return Ok((candidate.base, None));
            }
        }
    }

    crate::tlog!(
        "SERVER spawn binary={binary} port={port} dir={}",
        dir.display()
    );
    let mut cmd = tokio::process::Command::new(&binary);
    cmd.args(["serve", "--port", &port.to_string(), "--hostname", "127.0.0.1"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    // Spawn may fail if something else just took the port; fall through to
    // the health loop, which also accepts a server started by someone else.
    let mut child = cmd.spawn().ok();

    let base = format!("http://127.0.0.1:{port}");
    let client = Client::new(base.clone());
    let deadline =
        std::time::Instant::now() + Duration::from_millis(cfg.opencode.startup_timeout_ms);
    loop {
        if client.health().await.is_ok() {
            return Ok((base, child));
        }
        if let Some(c) = &mut child {
            if c.id().is_none() && std::time::Instant::now() > deadline {
                anyhow::bail!("opencode serve exited and no server answered on port {port}");
            }
        } else if std::time::Instant::now() > deadline {
            anyhow::bail!("could not spawn opencode serve on port {port} and no server answered");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Build the local backend from config: an OpenAI-compatible LLM provider
/// wrapped in an agent loop (tools + permission policy + catalog).
fn build_local_provider(cfg: &Config) -> Result<(LocalProvider, Option<LocalGates>), ProviderError> {
    let gates = local_gates(cfg, true);
    // MCP servers are connected once and their tools shared across every agent
    // we build (model/agent switches included).
    let mcp_tools: Arc<Vec<Arc<dyn crate::agent::tools::Tool>>> =
        Arc::new(crate::mcp::connect_all(&cfg.mcp));
    if !mcp_tools.is_empty() {
        crate::tlog!("LOCAL loaded {} MCP tool(s)", mcp_tools.len());
    }
    let agent = build_agent_with(
        cfg,
        true,
        gates.clone(),
        true,
        crate::agent::agents::default_name(),
        &mcp_tools,
    )?;
    // Picking a different model or agent rebuilds the adapter on demand.
    let broker = gates.clone();
    let base_cfg = cfg.clone();
    let factory: crate::providers::local::AgentFactory = Arc::new(move |provider, model, agent| {
        let mut c = base_cfg.clone();
        if !provider.is_empty() {
            c.ai.provider = provider.to_string();
        }
        if !model.is_empty() {
            c.ai.model = model.to_string();
        }
        build_agent_with(&c, true, broker.clone(), true, agent, &mcp_tools).map(Arc::new)
    });
    let provider_id = cfg.ai.provider.to_ascii_lowercase();
    let default_model = resolved_model(cfg, &provider_id);
    let dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    let default_agent = crate::agent::agents::default_name().to_string();
    let local = LocalProvider::with_factory(
        factory,
        Arc::new(agent),
        (cfg.ai.provider.clone(), default_model, default_agent),
        dir,
    );
    Ok((local, gates))
}

/// The model a provider config resolves to (`ai.model`, else the preset default).
fn resolved_model(cfg: &Config, provider_id: &str) -> String {
    if cfg.ai.model.trim().is_empty() {
        default_model_for(provider_id).to_string()
    } else {
        cfg.ai.model.clone()
    }
}

/// Build the in-process agent from config (also used by headless modes).
pub fn build_agent(
    cfg: &Config,
    interactive: bool,
) -> Result<(crate::agent::AgentLoop, Option<LocalGates>), ProviderError> {
    let gates = local_gates(cfg, interactive);
    let mcp_tools = crate::mcp::connect_all(&cfg.mcp);
    let agent = build_agent_with(
        cfg,
        interactive,
        gates.clone(),
        true,
        crate::agent::agents::default_name(),
        &mcp_tools,
    )?;
    Ok((agent, gates))
}

/// Construct the LLM provider selected by `[ai]` (Anthropic/Google native, or
/// an OpenAI-compatible preset / custom `base_url`).
pub fn make_provider(
    cfg: &Config,
) -> Result<Box<dyn crate::ai::Provider>, ProviderError> {
    let ai = &cfg.ai;
    let provider_id = ai.provider.to_ascii_lowercase();
    let creds = crate::credentials::Credentials::load();
    let key = creds.resolve(&provider_id, &ai.api_key_env);
    if !ai.base_url.trim().is_empty() {
        return Ok(Box::new(crate::ai::openai::OpenAiCompat::new(ai.base_url.clone(), key)));
    }
    match provider_id.as_str() {
        "anthropic" | "claude" => Ok(Box::new(crate::ai::anthropic::Anthropic::new(key))),
        "google" | "gemini" => Ok(Box::new(crate::ai::google::Google::new(key))),
        _ => Ok(Box::new(crate::ai::openai::OpenAiCompat::preset(&provider_id, key).ok_or_else(
            || {
                ProviderError::Unsupported(format!(
                    "unknown ai.provider '{}' (use anthropic/google/openai/xai/… or set ai.base_url)",
                    ai.provider
                ))
            },
        )?)),
    }
}

/// Interactive gates are shared across every agent when permissions are `ask`.
fn local_gates(cfg: &Config, interactive: bool) -> Option<LocalGates> {
    if interactive && cfg.behavior.local_permissions == "ask" {
        Some(LocalGates {
            permissions: Arc::new(crate::agent::permissions::Broker::new()),
            questions: Arc::new(crate::agent::permissions::QuestionBroker::new()),
        })
    } else {
        None
    }
}

fn build_agent_with(
    cfg: &Config,
    interactive: bool,
    gates: Option<LocalGates>,
    allow_task: bool,
    agent_name: &str,
    extra_tools: &[Arc<dyn crate::agent::tools::Tool>],
) -> Result<crate::agent::AgentLoop, ProviderError>
{
    let def = crate::agent::agents::find(agent_name)
        .unwrap_or_else(|| crate::agent::agents::find(crate::agent::agents::default_name()).unwrap());
    let ai = &cfg.ai;
    let provider_id = ai.provider.to_ascii_lowercase();
    let provider = make_provider(cfg)?;
    let model = if ai.model.trim().is_empty() {
        default_model_for(&provider_id).to_string()
    } else {
        ai.model.clone()
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let registry =
        crate::extensions::Registry::discover(&crate::extensions::Registry::default_roots(&cwd));
    // System prompt = base + agent prompt + project context files + skills.
    let mut appendix = format!("\n\n{}", def.system_prompt);
    appendix.push_str(&crate::extensions::load_context_files(&cwd));
    appendix.push_str(&registry.system_appendix());
    let base = crate::agent::context::CompactionSettings {
        reserve_tokens: cfg.compaction.reserve_tokens,
        keep_recent_tokens: cfg.compaction.keep_recent_tokens,
        tool_result_cap: 2_000,
    };
    let overrides: crate::agent::context::ModelOverrides = cfg
        .compaction
        .model_overrides
        .iter()
        .map(|(k, v)| (k.clone(), (v.reserve_tokens, v.keep_recent_tokens)))
        .collect();
    let settings = crate::agent::context::resolve_settings(base, &overrides, &model);
    // Named agents restrict the tool set (plan/explore are read-only).
    let all_tools = crate::agent::tools::default_tools();
    let mut tools = crate::agent::agents::tools_for(&def, all_tools);
    // MCP tools follow the same read-only rule as built-ins.
    if def.tools == crate::agent::agents::ToolSet::All {
        tools.extend(extra_tools.iter().cloned());
    }
    let mut agent = crate::agent::AgentLoop::new(provider, model)
        .with_tools(tools)
        .with_compaction(settings, cfg.compaction.enabled)
        .with_retry(cfg.ai.max_retries, cfg.ai.retry_base_ms)
        .with_system_appendix(appendix);
    // Permissions: an agent preset (e.g. read-only) wins; otherwise the
    // config mode, with interactive `ask` for the TUI and auto-allow headless.
    let mode = if def.permission != "inherit" {
        def.permission
    } else {
        cfg.behavior.local_permissions.as_str()
    };
    let ask = interactive && mode == "ask" && gates.is_some();
    agent = agent.with_permission(if ask {
        Box::new(crate::agent::tools::AskGate)
    } else if interactive || gates.is_some() {
        gate_for(mode, interactive)
    } else {
        Box::new(crate::agent::tools::AllowAll)
    });
    if let Some(g) = &gates {
        agent = agent
            .with_broker(g.permissions.clone())
            .with_question_broker(g.questions.clone());
    }
    if allow_task && def.can_delegate {
        // Sub-agents run with the same model but auto-approved and without a
        // `task` tool of their own, so spawning cannot recurse.
        let sub_cfg = cfg.clone();
        let sub_extra = extra_tools.to_vec();
        let builder = Arc::new(move |ty: &str| {
            build_agent_with(&sub_cfg, false, None, false, ty, &sub_extra)
        });
        agent = agent.with_extra_tool(Arc::new(crate::agent::tools::TaskTool::new(builder)));
    }
    Ok(agent)
}

fn gate_for(mode: &str, interactive: bool) -> Box<dyn crate::agent::tools::PermissionGate> {
    use crate::agent::tools::{AllowAll, AskGate, DenyAll, ReadOnly};
    match mode {
        "allow" => Box::new(AllowAll),
        "deny" => Box::new(DenyAll),
        "read-only" | "readonly" => Box::new(ReadOnly),
        "ask" if interactive => Box::new(AskGate),
        _ => Box::new(AllowAll),
    }
}

/// Sensible default model per provider when `ai.model` is unset.
pub(crate) fn default_model_for(provider: &str) -> &'static str {
    match provider {
        "anthropic" | "claude" => "claude-3-7-sonnet-20250219",
        "google" | "gemini" => "gemini-2.0-flash",
        "xai" | "grok" => "grok-2-latest",
        "deepseek" => "deepseek-chat",
        "groq" => "llama-3.3-70b-versatile",
        _ => "gpt-4o",
    }
}

/// Supervisor for one adapter's event pump: reconnect-on-failure policy lives
/// here; the adapter owns transport and conversion. Neutral routed events are
/// converted into `AppEvent::Harness` — the only place that mapping happens.
fn spawn_event_pump<P>(provider: P, dir: PathBuf, tx: tokio::sync::mpsc::UnboundedSender<AppEvent>)
where
    P: crate::providers::EventPump + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let sink = wrap_sink(tx, dir.clone());
        loop {
            match provider.pump(sink.clone()).await {
                // Stream closed cleanly: the server may be restarting; retry
                // after a short pause.
                Ok(()) => tokio::time::sleep(Duration::from_millis(200)).await,
                // Server gone or restarting: retry after a longer pause.
                Err(_e) => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    });
}

/// Bridge between the neutral [`RoutedEvent`] sink an adapter expects and the
/// app's `AppEvent` channel: routing/session ids stay owned by the manager.
fn wrap_sink(
    tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    dir: PathBuf,
) -> tokio::sync::mpsc::UnboundedSender<crate::providers::RoutedEvent> {
    let (r_tx, mut r_rx) = tokio::sync::mpsc::unbounded_channel::<crate::providers::RoutedEvent>();
    tokio::spawn(async move {
        while let Some(ev) = r_rx.recv().await {
            let _ = tx.send(AppEvent::Harness {
                dir: dir.clone(),
                oc_sid: ev.session_id.unwrap_or_default(),
                event: ev.event,
            });
        }
    });
    r_tx
}

fn s_time_updated(s: &crate::opencode::OcSession) -> i64 {
    s.updated_ms.unwrap_or(0)
}

/// Deterministic port in `[port_base, port_base + 1500)` for a directory.
fn port_for_dir(dir: &Path, base: u16) -> u16 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.to_string_lossy().to_string().hash(&mut h);
    let v = (h.finish() % 1500) as u16;
    base + v
}

fn same_dir(a: &str, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let pa = Path::new(a);
    canon(pa) == canon(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_provider_builds_for_preset_and_custom_base() {
        let mut cfg = Config::default();
        cfg.backend = "local".into();
        cfg.ai.provider = "openai".into();
        assert!(build_local_provider(&cfg).is_ok(), "preset providers build");

        cfg.ai.provider = "compat".into();
        cfg.ai.base_url = "http://localhost:1234/v1".into();
        assert!(build_local_provider(&cfg).is_ok(), "explicit base_url builds");

        cfg.ai.base_url = String::new();
        assert!(
            matches!(build_local_provider(&cfg), Err(ProviderError::Unsupported(_))),
            "unknown provider without base_url is a clean error"
        );
    }

    #[tokio::test]
    async fn manager_reports_backend() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.backend = "local".into();
        let m = Manager::new(tx, cfg);
        assert!(m.is_local());
    }
}
