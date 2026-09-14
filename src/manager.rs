//! Manager: owns per-directory OpenCode servers and bridges async work to the
//! UI event loop. All methods are fire-and-forget; results arrive as
//! `AppEvent`s on the shared channel.

use crate::config::Config;
use crate::events::{AppEvent, OcEvent, ReqId};
use crate::opencode::{Client, GrepMatch, ModelRef};
use anyhow::Result;
use futures::StreamExt;
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
    /// Handle to the spawned process. Kept alive so `kill_on_drop` stops the
    /// server when Theta exits (normal quit calls `shutdown_all` first).
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

        // SSE pump: forwards bus events for this directory to the app.
        let sse_tx = self.tx.clone();
        let sse_dir = dir.clone();
        let sse_client = client.clone();
        let sse_base = base.clone();
        tokio::spawn(async move {
            sse_loop(sse_client, sse_base, sse_dir, sse_tx).await;
        });

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
        Self {
            ref_: ManagerRef {
                tx,
                cfg,
                inner: Arc::new(Mutex::new(Inner::default())),
                children: Arc::new(Mutex::new(Vec::new())),
            },
            req_seq: AtomicU64::new(1),
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

            let session = match oc_sid.as_deref() {
                Some(sid) => match client.get_session(sid).await {
                    Ok(s) => s,
                    Err(_) => match client.create_session(&name).await {
                        Ok(s) => s,
                        Err(e) => {
                            m.emit(AppEvent::OcCreateFailed { req, error: e.to_string() });
                            return;
                        }
                    },
                },
                None => match client.create_session(&name).await {
                    Ok(s) => s,
                    Err(e) => {
                        m.emit(AppEvent::OcCreateFailed { req, error: e.to_string() });
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
    ) {
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
            match client
                .prompt_async(&oc_sid, &text, model.as_ref(), agent.as_deref())
                .await
            {
                Ok(()) => crate::tlog!("PROMPT ok session={oc_sid}"),
                Err(e) if model.is_some() => {
                    // A bad/unavailable model shouldn't swallow the prompt —
                    // retry on the server default so every provider works.
                    crate::tlog!("PROMPT retry without model session={oc_sid}: {e}");
                    match client.prompt_async(&oc_sid, &text, None, agent.as_deref()).await {
                        Ok(()) => crate::tlog!("PROMPT ok (default model) session={oc_sid}"),
                        Err(e) => {
                            crate::tlog!("PROMPT failed session={oc_sid}: {e}");
                            m.emit(AppEvent::SendFailed { dir, oc_sid, error: e.to_string() });
                        }
                    }
                }
                Err(e) => {
                    crate::tlog!("PROMPT failed session={oc_sid}: {e}");
                    m.emit(AppEvent::SendFailed { dir, oc_sid, error: e.to_string() });
                }
            }
        });
    }

    pub fn abort_session(&self, dir: PathBuf, oc_sid: String) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let Ok((_, client)) = m.ensure_server(&dir).await else {
                return;
            };
            if client.abort(&oc_sid).await.is_ok() {
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
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let Ok((_, client)) = m.ensure_server(&dir_c).await else {
                return;
            };
            match client.fork(&oc_sid, at.as_deref()).await {
                Ok(new) => {
                    m.emit(AppEvent::OcForked {
                        dir: dir_c,
                        session: new,
                        source,
                    });
                }
                Err(e) => {
                    m.emit(AppEvent::OpResult {
                        ok: false,
                        message: format!("fork failed: {e}"),
                    });
                }
            }
        });
    }

    /// Answer a pending agent question.
    pub fn reply_question(&self, dir: PathBuf, id: String, answers: Vec<Vec<String>>) {
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
        .kill_on_drop(true);
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

async fn sse_loop(
    client: Client,
    base: String,
    dir: PathBuf,
    tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    loop {
        let res = pump_once(&client, &base, &dir, &tx).await;
        if let Err(_e) = res {
            // Server gone or restarting: retry after a short pause.
            tokio::time::sleep(Duration::from_secs(1)).await;
        } else {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

async fn pump_once(
    client: &Client,
    base: &str,
    dir: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) -> Result<()> {
    let resp = client
        .raw()
        .get(format!("{base}/event"))
        .header("Accept", "text/event-stream")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("event stream status {}", resp.status());
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)]);
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start();
                if data.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(ev) = OcEvent::parse(v) {
                        crate::tlog!(
                            "SSE dir={} type={} {}",
                            dir.display(),
                            ev.typ,
                            crate::logging::snippet(data, 800)
                        );
                        let _ = tx.send(AppEvent::OcEvent {
                            dir: dir.to_path_buf(),
                            ev,
                        });
                    }
                }
            }
        }
    }
    anyhow::Ok(())
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
