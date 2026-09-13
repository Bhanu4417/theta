//! Events flowing from background tasks into the UI loop.

use crate::opencode::{
    AgentInfo, CustomCommand, GrepMatch, Message, ModelEntry, ModelRef, OcSession,
    PermissionRequest, QuestionRequest,
};
use std::path::PathBuf;

/// A transient key used to correlate async replies with UI requests.
pub type ReqId = u64;

#[derive(Debug)]
#[allow(dead_code)]
pub enum AppEvent {
    ServerReady { dir: PathBuf, base: String },
    ServerFailed { dir: PathBuf, error: String },
    ServerDied { dir: PathBuf },

    OcCreated { req: ReqId, session: OcSession },
    OcCreateFailed { req: ReqId, error: String },

    /// Raw SSE event from a server; routed by sessionID inside the app.
    OcEvent { dir: PathBuf, ev: OcEvent },

    HistoryLoaded {
        dir: PathBuf,
        oc_sid: String,
        msgs: Vec<Message>,
    },
    HistoryFailed {
        dir: PathBuf,
        oc_sid: String,
        error: String,
    },

    SendFailed {
        dir: PathBuf,
        oc_sid: String,
        error: String,
    },
    Aborted { dir: PathBuf, oc_sid: String },

    ProvidersListed {
        dir: PathBuf,
        providers: Vec<ModelEntry>,
        default: Option<ModelRef>,
    },

    FilesFound {
        req: ReqId,
        paths: Vec<String>,
    },
    SearchFailed {
        req: ReqId,
        error: String,
    },
    MatchesFound {
        req: ReqId,
        matches: Vec<GrepMatch>,
    },
    FileLoaded {
        req: ReqId,
        path: String,
        content: Option<String>,
        diff: Option<String>,
    },

    AgentsListed { dir: PathBuf, agents: Vec<AgentInfo> },
    CommandsListed {
        dir: PathBuf,
        commands: Vec<CustomCommand>,
    },
    /// Generic result of an async op (share, compact, revert, command…).
    OpResult { ok: bool, message: String },

    ServerSessionsListed {
        req: ReqId,
        dir: PathBuf,
        sessions: Vec<OcSession>,
    },

    /// Boot-time preload of a directory's session list.
    SessionsPreloaded {
        dir: PathBuf,
        sessions: Vec<OcSession>,
    },

    /// Completed output of an agy CLI run.
    AgyDone {
        session: u32,
        ok: bool,
        output: String,
        model: String,
    },
    /// Available models from the agy CLI.
    AgyModels { models: Vec<(String, String)> },
    /// A session was forked; the new one shares the history.
    OcForked {
        dir: PathBuf,
        session: OcSession,
        source: u32,
    },
    /// Pending agent questions fetched when (re)connecting.
    QuestionsListed {
        dir: PathBuf,
        questions: Vec<QuestionRequest>,
    },
    /// Pending permission requests fetched when (re)connecting.
    PermissionsListed {
        dir: PathBuf,
        permissions: Vec<PermissionRequest>,
    },
    /// Progress of a `/push` (commit + push) run.
    PushProgress { session: u32, text: String },
    /// A `/push` finished; `ok=false` carries the error message.
    PushDone {
        session: u32,
        ok: bool,
        message: String,
        repo: Option<String>,
    },
}

/// A raw OpenCode bus event: `{ id, type, properties }`.
#[derive(Debug, Clone)]
pub struct OcEvent {
    pub typ: String,
    pub properties: serde_json::Value,
}

impl OcEvent {
    pub fn parse(v: serde_json::Value) -> Option<Self> {
        let typ = v.get("type")?.as_str()?.to_string();
        Some(Self {
            typ,
            properties: v.get("properties").cloned().unwrap_or_default(),
        })
    }

    pub fn session_id(&self) -> Option<String> {
        self.properties
            .get("sessionID")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    }
}
