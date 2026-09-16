//! Events flowing from background tasks into the UI loop.

use crate::harness::transcript::Message;
use crate::harness::HarnessEvent;
use crate::models::{AgentInfo, CustomCommand, GrepMatch, ModelEntry, ModelRef, OcSession};
use crate::providers::ProviderSession;
use std::path::PathBuf;

/// A transient key used to correlate async replies with UI requests.
pub type ReqId = u64;

#[derive(Debug)]
#[allow(dead_code)]
pub enum AppEvent {
    ServerReady { dir: PathBuf, base: String },
    ServerFailed { dir: PathBuf, error: String },
    ServerDied { dir: PathBuf },

    OcCreated { req: ReqId, session: ProviderSession },
    OcCreateFailed { req: ReqId, error: String },

    /// Provider-neutral harness event, routed by native session id.
    Harness {
        dir: PathBuf,
        oc_sid: String,
        event: HarnessEvent,
    },

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

    /// A session was forked; the new one shares the history.
    OcForked {
        dir: PathBuf,
        session: ProviderSession,
        source: u32,
    },
    /// A fork failed; the placeholder pane for `source` should show the error.
    OcForkFailed {
        source: u32,
        error: String,
    },
    /// Progress of a `/push` (commit + push) run.
    PushProgress { session: u32, text: String },
    /// A `!`/`!!` shell escape finished.
    ShellDone {
        session: u32,
        command: String,
        ok: bool,
        output: String,
        send_to_agent: bool,
    },
    /// A local session's history tree (for the `/tree` overlay).
    TreeLoaded {
        oc_sid: String,
        tree: crate::tree::SessionTree,
    },
    /// A `/push` finished; `ok=false` carries the error message.
    PushDone {
        session: u32,
        ok: bool,
        message: String,
        repo: Option<String>,
        subject: String,
    },
}
