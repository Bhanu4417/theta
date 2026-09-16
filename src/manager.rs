use crate::config::Config;
use crate::events::{AppEvent, ReqId};
use crate::models::{GrepMatch, ModelRef, OcSession};
use crate::providers::local::LocalProvider;
use crate::providers::{AgentProvider, ModelId, ProviderError, ProviderKind, ProviderSession};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ManagerRef {
    tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    cfg: Arc<Mutex<Config>>,
    local: Option<Arc<LocalProvider>>,
    local_gates: Option<LocalGates>,
}

#[derive(Clone)]
pub struct LocalGates {
    pub permissions: Arc<crate::agent::permissions::Broker>,
    pub questions: Arc<crate::agent::permissions::QuestionBroker>,
}

impl ManagerRef {
    fn emit(&self, ev: AppEvent) {
        let _ = self.tx.send(ev);
    }

    fn cfg(&self) -> Config {
        self.cfg.lock().unwrap().clone()
    }
}

pub struct Manager {
    ref_: ManagerRef,
    req_seq: AtomicU64,
}

impl Manager {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<AppEvent>, cfg: Config) -> Self {
        let cfg = Arc::new(Mutex::new(cfg));
        let (local, local_gates) = match build_local_provider(&cfg) {
            Ok((provider, gates)) => {
                let provider = Arc::new(provider);
                spawn_event_pump(provider.clone(), tx.clone());
                crate::tlog!("LOCAL provider ready (model={})", cfg.lock().unwrap().ai.model);
                (Some(provider), gates)
            }
            Err(e) => {
                crate::tlog!("LOCAL init failed: {e}");
                eprintln!("theta: backend failed to start: {e}");
                (None, None)
            }
        };
        Self {
            ref_: ManagerRef { tx, cfg, local, local_gates },
            req_seq: AtomicU64::new(1),
        }
    }

    pub async fn shutdown_all(&self) {
    }

    pub fn tx(&self) -> tokio::sync::mpsc::UnboundedSender<AppEvent> {
        self.ref_.tx.clone()
    }

    fn result(&self, ok: bool, message: impl Into<String>) {
        self.ref_.emit(AppEvent::OpResult { ok, message: message.into() });
    }


    pub fn local_tree(&self, oc_sid: String) {
        if let Some(local) = &self.ref_.local {
            match local.tree_snapshot(&oc_sid) {
                Some(tree) => self.ref_.emit(AppEvent::TreeLoaded { oc_sid, tree }),
                None => self.result(false, "no history for this session"),
            }
        }
    }

    pub fn local_tree_snapshot(&self, oc_sid: &str) -> Option<crate::tree::SessionTree> {
        self.ref_.local.as_ref()?.tree_snapshot(oc_sid)
    }

    pub fn reload_credentials(&self) {
        if let Some(local) = &self.ref_.local {
            local.invalidate_agents();
        }
    }

    pub fn set_ai(&self, provider: &str, model: &str, base_url: &str) {
        {
            let mut c = self.ref_.cfg.lock().unwrap();
            if !provider.is_empty() {
                c.ai.provider = provider.to_string();
            }
            if !model.is_empty() {
                c.ai.model = model.to_string();
            }
            c.ai.base_url = base_url.to_string();
        }
        self.reload_credentials();
    }

    pub fn local_rewind(&self, oc_sid: String, entry: String) -> Option<String> {
        self.ref_.local.as_ref()?.rewind(&oc_sid, &entry)
    }

    pub fn local_redo(&self, oc_sid: String) -> bool {
        self.ref_.local.as_ref().is_some_and(|l| l.redo(&oc_sid))
    }

    pub fn local_compact(&self, oc_sid: String) {
        let Some(local) = self.ref_.local.clone() else {
            self.result(false, "compaction requires the local harness");
            return;
        };
        tokio::spawn(async move {
            if !local.compact_session(&oc_sid).await {
                crate::tlog!("COMPACT: nothing to compact for {oc_sid}");
            }
        });
    }

    pub fn local_permission_reply(&self, id: String, response: String) {
        if let Some(gates) = &self.ref_.local_gates {
            let decision = crate::agent::permissions::decision_for(&response);
            if !gates.permissions.reply(&id, decision) {
                crate::tlog!("PERM reply for unknown request {id}");
            }
        }
    }

    pub fn local_navigate(&self, oc_sid: String, entry: String) {
        if let Some(local) = &self.ref_.local {
            let l = local.clone();
            tokio::spawn(async move {
                let _ = l.navigate_auto(&oc_sid, &entry).await;
            });
        }
    }

    pub fn next_req(&self) -> ReqId {
        self.req_seq.fetch_add(1, Ordering::Relaxed)
    }


    pub fn connect_session(
        &self,
        req: ReqId,
        dir: PathBuf,
        name: String,
        oc_sid: Option<String>,
        model: Option<ModelRef>,
        history_limit: u32,
    ) {
        let Some(local) = self.ref_.local.clone() else {
            let error = "the harness failed to start".to_string();
            self.ref_.emit(AppEvent::OcCreateFailed { req, error });
            return;
        };
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
                base: "theta".into(),
            });
            m.emit(AppEvent::OcCreated { req, session: session.clone() });
            let cfg = m.cfg();
            let active = cfg.ai.provider.to_ascii_lowercase();
            let providers = crate::ai::discovery::catalog_entries_for(Some(&active));
            let default = Some(model.clone().unwrap_or(ModelRef {
                provider_id: cfg.ai.provider.clone(),
                model_id: cfg.ai.model.clone(),
            }));
            m.emit(AppEvent::ProvidersListed { dir: dir_c.clone(), providers, default });
            m.emit(AppEvent::AgentsListed {
                dir: dir_c.clone(),
                agents: crate::agent::agents::names()
                    .into_iter()
                    .map(|(name, description)| crate::models::AgentInfo { name, description })
                    .collect(),
            });
            let live = crate::ai::discovery::discover_models(&cfg).await;
            m.emit(AppEvent::ProvidersListed {
                dir: dir_c,
                providers: live,
                default: Some(model.clone().unwrap_or(ModelRef {
                    provider_id: cfg.ai.provider.clone(),
                    model_id: cfg.ai.model.clone(),
                })),
            });
            let _ = history_limit;
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
        let Some(local) = self.ref_.local.clone() else {
            self.ref_.emit(AppEvent::SendFailed {
                dir,
                oc_sid,
                error: "the harness failed to start".into(),
            });
            return;
        };
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let session = ProviderSession {
                provider: ProviderKind::Local,
                id: oc_sid.clone(),
                directory: dir.to_string_lossy().to_string(),
            };
            let model_id = model.map(|m| ModelId { provider: m.provider_id, model: m.model_id });
            if let Err(e) = local.send_message(&session, &text, model_id, agent, &attachments).await
            {
                m.emit(AppEvent::SendFailed { dir, oc_sid, error: e.to_string() });
            }
        });
    }

    pub fn abort_session(&self, oc_sid: String) {
        let Some(local) = self.ref_.local.clone() else { return };
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let session = ProviderSession {
                provider: ProviderKind::Local,
                id: oc_sid.clone(),
                directory: String::new(),
            };
            if local.interrupt(&session).await.is_ok() {
                m.emit(AppEvent::Aborted { dir: PathBuf::new(), oc_sid });
            }
        });
    }

    pub fn fork_session(
        &self,
        dir: PathBuf,
        oc_sid: String,
        source: u32,
        at: Option<String>,
    ) {
        let Some(local) = self.ref_.local.clone() else {
            self.ref_.emit(AppEvent::OcForkFailed { source, error: "harness unavailable".into() });
            return;
        };
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let session = ProviderSession {
                provider: ProviderKind::Local,
                id: oc_sid.clone(),
                directory: dir.to_string_lossy().to_string(),
            };
            match local.fork(&session, at.as_deref()).await {
                Ok(new) => m.emit(AppEvent::OcForked { dir, session: new, source }),
                Err(e) => m.emit(AppEvent::OcForkFailed { source, error: format!("fork failed: {e}") }),
            }
        });
    }


    pub fn reply_question(&self, id: String, answers: Vec<Vec<String>>) {
        if let Some(gates) = &self.ref_.local_gates {
            let ok = gates.questions.reply(&id, answers);
            self.result(ok, if ok { "answer sent" } else { "question expired" });
        }
    }

    pub fn reject_question(&self, id: String) {
        if let Some(gates) = &self.ref_.local_gates {
            let ok = gates.questions.reject(&id);
            self.result(ok, if ok { "question rejected" } else { "question expired" });
        }
    }


    pub fn search_files(&self, req: ReqId, dir: PathBuf, query: String) {
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
    }

    pub fn search_pattern(&self, req: ReqId, dir: PathBuf, pattern: String) {
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
    }

    pub fn load_file(&self, req: ReqId, _dir: PathBuf, path: String) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let content = std::fs::read_to_string(&path).ok();
            m.emit(AppEvent::FileLoaded { req, path, content, diff: None });
        });
    }


    pub fn refresh_providers(&self, dir: PathBuf) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let cfg = m.cfg();
            let providers = crate::ai::discovery::discover_models(&cfg).await;
            let default = Some(ModelRef {
                provider_id: cfg.ai.provider.clone(),
                model_id: cfg.ai.model.clone(),
            });
            m.emit(AppEvent::ProvidersListed { dir: dir_c, providers, default });
        });
    }

    pub fn list_server_sessions(&self, req: ReqId, dir: PathBuf) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            let sessions = local_sessions();
            m.emit(AppEvent::ServerSessionsListed { req, dir: dir_c, sessions });
        });
    }

    pub fn preload_dir(&self, target_dir: PathBuf) {
        let m = self.ref_.clone();
        tokio::spawn(async move {
            let dir_c = target_dir.canonicalize().unwrap_or(target_dir);
            m.emit(AppEvent::SessionsPreloaded { dir: dir_c, sessions: local_sessions() });
        });
    }


    pub fn run_command(&self, command: String) {
        self.result(
            false,
            format!("custom /{command} commands are not supported; skills auto-load"),
        );
    }

    pub fn share(&self, want: bool) {
        self.result(
            false,
            if want {
                "there are no share links; use /export instead"
            } else {
                "nothing to unshare"
            },
        );
    }
}

fn local_sessions() -> Vec<OcSession> {
    crate::tree::SessionTree::list_sessions()
        .into_iter()
        .map(|s| OcSession {
            id: s.id,
            title: s.title,
            directory: s.directory.unwrap_or_default(),
            updated_ms: Some(s.updated_ms),
        })
        .collect()
}

fn build_local_provider(
    cfg_shared: &Arc<Mutex<Config>>,
) -> Result<(LocalProvider, Option<LocalGates>), ProviderError> {
    let cfg = cfg_shared.lock().unwrap().clone();
    let gates = local_gates(&cfg, true);
    let mcp_tools: Arc<Vec<Arc<dyn crate::agent::tools::Tool>>> =
        Arc::new(crate::mcp::connect_all(&cfg.mcp));
    if !mcp_tools.is_empty() {
        crate::tlog!("LOCAL loaded {} MCP tool(s)", mcp_tools.len());
    }
    let agent = build_agent_with(
        &cfg,
        true,
        gates.clone(),
        true,
        crate::agent::agents::default_name(),
        &mcp_tools,
    )?;
    let broker = gates.clone();
    let factory_cfg = cfg_shared.clone();
    let factory: crate::providers::local::AgentFactory =
        Arc::new(move |provider, model, agent| {
            let mut c = factory_cfg.lock().unwrap().clone();
            if !provider.is_empty() {
                c.ai.provider = provider.to_string();
                if provider != "custom" {
                    c.ai.base_url = String::new();
                }
            }
            if !model.is_empty() {
                c.ai.model = model.to_string();
            }
            build_agent_with(&c, true, broker.clone(), true, agent, &mcp_tools).map(Arc::new)
        });
    let provider_id = cfg.ai.provider.to_ascii_lowercase();
    let default_model = resolved_model(&cfg, &provider_id);
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

fn resolved_model(cfg: &Config, provider_id: &str) -> String {
    if cfg.ai.model.trim().is_empty() {
        default_model_for(provider_id).to_string()
    } else {
        cfg.ai.model.clone()
    }
}

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

pub fn make_provider(cfg: &Config) -> Result<Box<dyn crate::ai::Provider>, ProviderError> {
    let provider_id = cfg.ai.provider.to_ascii_lowercase();
    let model = resolved_model(cfg, &provider_id);
    make_provider_for_model(cfg, &model)
}

pub fn make_provider_for_model(
    cfg: &Config,
    model: &str,
) -> Result<Box<dyn crate::ai::Provider>, ProviderError> {
    let ai = &cfg.ai;
    let provider_id = ai.provider.to_ascii_lowercase();
    let creds = crate::credentials::Credentials::load();
    let key = creds.resolve(&provider_id, &ai.api_key_env);

    // Refuse here, with something actionable, rather than sending an
    // unauthenticated request: the provider's raw 401 JSON does not tell the
    // user how to fix it. A custom base_url is exempt, since a local or
    // self-hosted endpoint may deliberately need no key.
    if key.is_none() && ai.base_url.trim().is_empty() && crate::ai::is_hosted_preset(&provider_id) {
        return Err(crate::providers::ProviderError::Auth(format!(
            "no API key configured for `{provider_id}`. Run `/login` to add one, or set \
             `${}`, or point `[ai].base_url` at a local endpoint.",
            crate::ai::env_hint(&provider_id)
        )));
    }
    let mut provider = crate::ai::provider_for_model(&provider_id, &ai.base_url, key, model)?;
    provider.set_timeout(ai.timeout_secs);
    Ok(provider)
}

fn local_gates(_cfg: &Config, interactive: bool) -> Option<LocalGates> {
    if interactive {
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
) -> Result<crate::agent::AgentLoop, ProviderError> {
    let def = crate::agent::agents::find(agent_name).unwrap_or_else(|| {
        crate::agent::agents::find(crate::agent::agents::default_name()).unwrap()
    });
    let provider = make_provider(cfg)?;
    let model = resolved_model(cfg, &cfg.ai.provider.to_ascii_lowercase());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let registry =
        crate::extensions::Registry::discover(&crate::extensions::Registry::default_roots(&cwd));
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
    let prune = crate::agent::context::PruneSettings {
        enabled: cfg.compaction.prune,
        protect_tokens: cfg.compaction.prune_protect_tokens,
        minimum_tokens: cfg.compaction.prune_minimum_tokens,
    };
    let compactor = if cfg.compaction.model.trim().is_empty() {
        None
    } else {
        let m = cfg.compaction.model.trim().to_string();
        match make_provider_for_model(cfg, &m) {
            Ok(p) => Some((p, m)),
            Err(e) => {
                crate::tlog!("WARN compaction model {m} unavailable ({e}); using session model");
                None
            }
        }
    };
    let all_tools = crate::agent::tools::default_tools();
    let mut tools = crate::agent::agents::tools_for(&def, all_tools);
    if def.tools == crate::agent::agents::ToolSet::All {
        tools.extend(extra_tools.iter().cloned());
    }
    let mut agent = crate::agent::AgentLoop::new(provider, model)
        .with_tools(tools)
        .with_compaction(settings, cfg.compaction.enabled)
        .with_prune(prune)
        .with_retry(cfg.ai.max_retries, cfg.ai.retry_base_ms)
        .with_max_turns(cfg.ai.max_turns)
        .with_reasoning_effort(cfg.ai.reasoning_effort.clone())
        .with_system_appendix(appendix);
    if let Some((provider, model)) = compactor {
        agent = agent.with_compaction_model(provider, model);
    }
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
        let sub_cfg = cfg.clone();
        let sub_extra = extra_tools.to_vec();
        let builder =
            Arc::new(move |ty: &str| build_agent_with(&sub_cfg, false, None, false, ty, &sub_extra));
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

pub(crate) fn default_model_for(provider: &str) -> &'static str {
    match provider {
        "opencode" | "opencode-zen" | "zen" => "glm-4.7",
        "opencode-go" | "go" => "deepseek-v4.1-flash",
        "anthropic" | "claude" => "claude-3-7-sonnet-20250219",
        "google" | "gemini" => "gemini-2.0-flash",
        "xai" | "grok" => "grok-2-latest",
        "deepseek" => "deepseek-chat",
        "groq" => "llama-3.3-70b-versatile",
        "mistral" => "mistral-large-latest",
        "cerebras" => "llama-3.3-70b",
        "perplexity" => "sonar",
        _ => "gpt-4o",
    }
}

fn spawn_event_pump<P>(provider: P, tx: tokio::sync::mpsc::UnboundedSender<AppEvent>)
where
    P: crate::providers::EventPump + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let sink = wrap_sink(tx);
        loop {
            if provider.pump(sink.clone()).await.is_err() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
}

fn wrap_sink(
    tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
) -> tokio::sync::mpsc::UnboundedSender<crate::providers::RoutedEvent> {
    let (r_tx, mut r_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::providers::RoutedEvent>();
    tokio::spawn(async move {
        while let Some(ev) = r_rx.recv().await {
            let _ = tx.send(AppEvent::Harness {
                dir: PathBuf::new(),
                oc_sid: ev.session_id.unwrap_or_default(),
                event: ev.event,
            });
        }
    });
    r_tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_sessions_always_wire_a_question_broker() {
        for mode in ["allow", "ask", "read-only", "deny"] {
            let mut cfg = Config::default();
            cfg.behavior.local_permissions = mode.into();
            assert!(
                local_gates(&cfg, true).is_some(),
                "TUI must have a question broker in `{mode}` mode"
            );
            assert!(
                local_gates(&cfg, false).is_none(),
                "headless runs have no UI to answer questions"
            );
        }
    }

    #[test]
    fn local_provider_builds_for_preset_and_custom_base() {
        let mut cfg = Config::default();
        cfg.ai.provider = "openai".into();
        // A hosted preset needs credentials; this test is about which base URL
        // is chosen, so satisfy the credential check.
        std::env::set_var("THETA_BASE_TEST_KEY", "sk-test");
        cfg.ai.api_key_env = "THETA_BASE_TEST_KEY".into();
        let shared = Arc::new(Mutex::new(cfg.clone()));
        assert!(build_local_provider(&shared).is_ok(), "preset providers build");

        // An explicit base_url needs no key.
        cfg.ai.provider = "compat".into();
        cfg.ai.base_url = "http://localhost:1234/v1".into();
        cfg.ai.api_key_env = "THETA_DEFINITELY_UNSET_KEY".into();
        let shared = Arc::new(Mutex::new(cfg.clone()));
        assert!(build_local_provider(&shared).is_ok(), "explicit base_url builds");

        cfg.ai.base_url = String::new();
        let shared = Arc::new(Mutex::new(cfg));
        assert!(
            matches!(build_local_provider(&shared), Err(ProviderError::Unsupported(_))),
            "unknown provider without base_url is a clean error"
        );
        std::env::remove_var("THETA_BASE_TEST_KEY");
    }

    #[tokio::test]
    async fn set_ai_updates_live_config() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let m = Manager::new(tx, Config::default());
        m.set_ai("custom", "my-model", "https://example.test/v1");
        let c = m.ref_.cfg();
        assert_eq!(c.ai.provider, "custom");
        assert_eq!(c.ai.model, "my-model");
        assert_eq!(c.ai.base_url, "https://example.test/v1");
        m.set_ai("", "", "");
        let c = m.ref_.cfg();
        assert_eq!(c.ai.provider, "custom");
        assert_eq!(c.ai.model, "my-model");
        assert_eq!(c.ai.base_url, "");
    }

    #[test]
    fn opencode_providers_get_zen_defaults_and_build() {
        assert_eq!(default_model_for("opencode"), "glm-4.7");
        assert_eq!(default_model_for("opencode-go"), "deepseek-v4.1-flash");
        let mut cfg = Config::default();
        cfg.ai.provider = "opencode-go".into();
        cfg.ai.model = String::new();
        assert_eq!(resolved_model(&cfg, "opencode-go"), "deepseek-v4.1-flash");
        // A key is required for any hosted preset, so supply one: this test is
        // about which preset resolves, not about the missing-credential path.
        std::env::set_var("THETA_PRESET_TEST_KEY", "sk-test");
        cfg.ai.api_key_env = "THETA_PRESET_TEST_KEY".into();
        assert!(make_provider(&cfg).is_ok(), "opencode-go builds from its preset");
        cfg.ai.provider = "opencode".into();
        assert!(make_provider(&cfg).is_ok(), "opencode zen builds from its preset");
        std::env::remove_var("THETA_PRESET_TEST_KEY");
    }


    #[test]
    fn a_hosted_provider_without_a_key_is_refused_with_guidance() {
        // Before this check the request went out unauthenticated and the user
        // was shown the provider's raw 401 JSON, which never mentioned /login.
        let mut cfg = Config::default();
        cfg.ai.provider = "openai".into();
        cfg.ai.base_url = String::new();
        cfg.ai.api_key_env = "THETA_DEFINITELY_UNSET_KEY".into();
        std::env::remove_var("THETA_DEFINITELY_UNSET_KEY");

        let msg = match make_provider(&cfg) {
            Err(crate::providers::ProviderError::Auth(m)) => m,
            Err(_) => panic!("expected an Auth error"),
            Ok(_) => panic!("a hosted provider with no key must be refused"),
        };
        assert!(msg.contains("/login"), "must point at /login: {msg}");
        assert!(msg.contains("OPENAI_API_KEY"), "must name the env var: {msg}");
    }

    #[test]
    fn a_custom_base_url_may_omit_the_key() {
        // A local or self-hosted endpoint often needs no credentials, so the
        // check must not fire when base_url is set.
        let mut cfg = Config::default();
        cfg.ai.provider = "openai".into();
        cfg.ai.base_url = "http://localhost:11434/v1".into();
        cfg.ai.api_key_env = "THETA_DEFINITELY_UNSET_KEY".into();
        std::env::remove_var("THETA_DEFINITELY_UNSET_KEY");
        assert!(make_provider(&cfg).is_ok());
    }

    #[test]
    fn a_supplied_key_is_accepted() {
        let mut cfg = Config::default();
        cfg.ai.provider = "openai".into();
        cfg.ai.base_url = String::new();
        cfg.ai.api_key_env = "THETA_TEST_PRESENT_KEY".into();
        std::env::set_var("THETA_TEST_PRESENT_KEY", "sk-test");
        let ok = make_provider(&cfg).is_ok();
        std::env::remove_var("THETA_TEST_PRESENT_KEY");
        assert!(ok);
    }

}
