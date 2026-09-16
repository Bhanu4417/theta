use crate::harness::transcript::Message;
use crate::harness::HarnessEvent;
use crate::models::{AgentInfo, CustomCommand, GrepMatch, ModelEntry, ModelRef, OcSession};
use crate::providers::ProviderSession;
use std::path::PathBuf;

pub type ReqId = u64;

#[derive(Debug)]
#[allow(dead_code)]
#[allow(clippy::large_enum_variant)]
pub enum AppEvent {
    ServerReady { dir: PathBuf, base: String },
    ServerFailed { dir: PathBuf, error: String },
    ServerDied { dir: PathBuf },

    OcCreated { req: ReqId, session: ProviderSession },
    OcCreateFailed { req: ReqId, error: String },

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
    OpResult { ok: bool, message: String },

    ServerSessionsListed {
        req: ReqId,
        dir: PathBuf,
        sessions: Vec<OcSession>,
    },

    SessionsPreloaded {
        dir: PathBuf,
        sessions: Vec<OcSession>,
    },

    OcForked {
        dir: PathBuf,
        session: ProviderSession,
        source: u32,
    },
    OcForkFailed {
        source: u32,
        error: String,
    },
    PushProgress { session: u32, text: String },
    ShellDone {
        session: u32,
        command: String,
        ok: bool,
        output: String,
        send_to_agent: bool,
    },
    TreeLoaded {
        oc_sid: String,
        tree: crate::tree::SessionTree,
    },
    PushDone {
        session: u32,
        ok: bool,
        message: String,
        repo: Option<String>,
        subject: String,
    },
}
