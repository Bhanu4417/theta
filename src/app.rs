//! Application state: sessions, panes, overlays, event routing, key handling.

use crate::config::Config;
use crate::events::{AppEvent, OcEvent, ReqId};
use crate::git::{GitCache, GitInfo};
use crate::keys::Action;
use crate::manager::Manager;
use crate::opencode::{GrepMatch, Message, ModelEntry, ModelRef, OcSession, PartKind, Role, ToolStatus};
use crate::panes::{Dir, PaneGrid};
use crate::persist;
use crate::session::{InputState, PendingPermission, PendingQuestion, SessionState, SessStatus};
use crate::theme::{pal, self};
use crate::ui::conversation;
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Palette,
    NewSession,
    Rename,
    ConfirmQuit,
    BusyChoice,
    FileSearch,
    ProjectSearch,
    ConvSearch,
    Keymap,
    Theme,
    AgyModel,
    Question,
    ModelPicker,
    AgentPicker,
    SessionList,
    LayoutPicker,
    ResumeSession,
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SlashKind {
    Model,
    Agent,
    New,
    Sessions,
    Resume,
    Close,
    Agy,
    AgyModel,
    Keymap,
    Clear,
    Compact,
    Undo,
    Redo,
    Share,
    Unshare,
    Init,
    Help,
    Quit,
    Refresh,
    Push,
    Custom(String),
}

#[derive(Debug, Clone)]
pub struct SlashItem {
    pub name: String,
    pub args: String,
    pub desc: String,
    pub kind: SlashKind,
}

fn builtin_slash_items() -> Vec<SlashItem> {
    vec![
        SlashItem { name: "model".into(), args: "[provider/model]".into(), desc: "Change the model for this session".into(), kind: SlashKind::Model },
        SlashItem { name: "agent".into(), args: "[name]".into(), desc: "Switch agent (build, plan, …)".into(), kind: SlashKind::Agent },
        SlashItem { name: "new".into(), args: "".into(), desc: "Create a new session".into(), kind: SlashKind::New },
        SlashItem { name: "sessions".into(), args: "".into(), desc: "Jump to another open session".into(), kind: SlashKind::Sessions },
        SlashItem { name: "resume".into(), args: "".into(), desc: "Resume a previous session".into(), kind: SlashKind::Resume },
        SlashItem { name: "clear".into(), args: "".into(), desc: "Clear transcript view (history kept)".into(), kind: SlashKind::Clear },
        SlashItem { name: "compact".into(), args: "".into(), desc: "Summarize the conversation".into(), kind: SlashKind::Compact },
        SlashItem { name: "undo".into(), args: "".into(), desc: "Revert the last message".into(), kind: SlashKind::Undo },
        SlashItem { name: "redo".into(), args: "".into(), desc: "Re-apply a reverted message".into(), kind: SlashKind::Redo },
        SlashItem { name: "share".into(), args: "".into(), desc: "Share this session (get URL)".into(), kind: SlashKind::Share },
        SlashItem { name: "unshare".into(), args: "".into(), desc: "Stop sharing this session".into(), kind: SlashKind::Unshare },
        SlashItem { name: "init".into(), args: "[focus]".into(), desc: "Create/update AGENTS.md".into(), kind: SlashKind::Init },
        SlashItem { name: "agy".into(), args: "<prompt>".into(), desc: "Ask via agy CLI (Gemini models)".into(), kind: SlashKind::Agy },
        SlashItem { name: "agymodel".into(), args: "".into(), desc: "Pick the agy model".into(), kind: SlashKind::AgyModel },
        SlashItem { name: "keys".into(), args: "".into(), desc: "View and edit keybindings".into(), kind: SlashKind::Keymap },
        SlashItem { name: "help".into(), args: "".into(), desc: "Overview of keys and commands".into(), kind: SlashKind::Keymap },
        SlashItem { name: "close".into(), args: "".into(), desc: "Close this session".into(), kind: SlashKind::Close },
        SlashItem { name: "quit".into(), args: "".into(), desc: "Quit Theta".into(), kind: SlashKind::Quit },
        SlashItem { name: "refresh".into(), args: "".into(), desc: "Reload the newest build in place".into(), kind: SlashKind::Refresh },
        SlashItem { name: "push".into(), args: "[message]".into(), desc: "Commit and push this project (session only)".into(), kind: SlashKind::Push },
    ]
}

/// All slash commands: builtins plus server-provided custom ones.
pub fn slash_items(app: &App) -> Vec<SlashItem> {
    let mut items = builtin_slash_items();
    for c in &app.custom_commands {
        items.push(SlashItem {
            name: c.name.clone(),
            args: String::new(),
            desc: if c.description.is_empty() {
                format!("{} command", c.source)
            } else {
                let d: String = c.description.chars().take(60).collect();
                d
            },
            kind: SlashKind::Custom(c.name.clone()),
        });
    }
    items
}

/// Commands matching the current input ("/mod" → model). Empty query lists all.
pub fn slash_matches(input: &str, app: &App) -> Vec<SlashItem> {
    let rest = input.trim_start();
    let Some(query) = rest.strip_prefix('/') else {
        return Vec::new();
    };
    let query = query.trim();
    let items = slash_items(app);
    if query.is_empty() {
        return items;
    }
    let ql = query.to_lowercase();
    items
        .into_iter()
        .filter(|i| i.name.to_lowercase().starts_with(&ql))
        .collect()
}

// ---------------------------------------------------------------------------
// Overlay state
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct PaletteState {
    pub input: InputState,
    pub selected: usize,
}

pub struct NewSessionState {
    pub field: usize, // 0 name, 1 dir, 2 resume list
    pub name: InputState,
    pub dir: String,
    pub recent: Vec<OcSession>,
    pub recent_selected: usize,
    pub recent_req: Option<ReqId>,
    pub recent_loaded: bool,
}


pub struct RenameState {
    pub input: InputState,
}

#[derive(Default)]
pub struct ModelPickerState {
    pub input: InputState,
    pub selected: usize,
}

#[derive(Default)]
pub struct AgentPickerState {
    pub input: InputState,
    pub selected: usize,
}

#[derive(Default)]
pub struct SessionListState {
    pub selected: usize,
}

#[derive(Default)]
pub struct LayoutPickerState {
    pub selected: usize,
}

#[derive(Default)]
pub struct ThemeUi {
    pub selected: usize,
    /// Theme to restore if the picker is closed without confirming.
    pub original: String,
}

#[derive(Default)]
pub struct KeymapUi {
    pub selected: usize,
    /// While set, the next key press rebinds this action.
    pub capturing: Option<crate::keys::Action>,
}

#[derive(Default)]
pub struct ResumePickerState {
    pub items: Vec<OcSession>,
    pub selected: usize,
    pub req: Option<ReqId>,
    pub loaded: bool,
}

pub struct FileSearchState {
    pub input: InputState,
    pub results: Vec<String>,
    pub selected: usize,
    pub req: Option<ReqId>,
}

pub struct ProjSearchState {
    pub input: InputState,
    pub results: Vec<GrepMatch>,
    pub selected: usize,
    pub req: Option<ReqId>,
}

pub struct ConvSearchState {
    pub input: InputState,
    pub matches: Vec<ConvMatch>,
    pub selected: usize,
}

pub struct ConvMatch {
    pub block: usize,
    pub label: String,
    pub excerpt: String,
}

#[derive(Clone)]
pub struct ExpItem {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
}

#[derive(Default)]
pub struct ExplorerState {
    pub open: bool,
    pub root: PathBuf,
    pub expanded: std::collections::HashSet<String>,
    pub items: Vec<ExpItem>,
    pub selected: usize,
    pub dirty: bool,
}

pub struct ViewerState {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub raw: String,
    pub scroll: usize,
    pub jump_line: Option<u64>,
}

pub struct DiffState {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: usize,
}

/// A screen-space point (absolute terminal row/column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectPoint {
    pub row: u16,
    pub col: u16,
}

/// In-progress or completed mouse text selection within one pane.
#[derive(Debug, Clone, Copy)]
pub struct SelectState {
    pub sid: u32,
    pub anchor: SelectPoint,
    pub head: SelectPoint,
    pub dragging: bool,
}

#[derive(Clone)]
pub enum FileReq {
    Viewer { line: Option<u64> },
}

pub struct Command {
    pub label: &'static str,
    pub hint: &'static str,
    pub cmd: Cmd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    NewSession,
    CloseSession,
    RenameSession,
    MaximizeRestore,
    Retile,
    FocusNext,
    FocusPrev,
    Interrupt,
    RestartSession,
    ReconnectAll,
    SearchFiles,
    SearchProject,
    SearchConversation,
    ToggleExplorer,
    GitDiff,
    GitLog,
    Help,
    Quit,
    ChangeModel,
    SwitchAgent,
    ChangeTiling,
    ResumeSession,
    SwitchSession,
    Keymap,
    Theme,
    Palette,
    AgyModel,
    Refresh,
    SwapLeft,
    SwapRight,
    SwapUp,
    SwapDown,
}

pub fn all_commands() -> Vec<Command> {
    vec![
        Command { label: "New Session", hint: "^N", cmd: Cmd::NewSession },
        Command { label: "Close Session", hint: "^W", cmd: Cmd::CloseSession },
        Command { label: "Rename Session", hint: "", cmd: Cmd::RenameSession },
        Command { label: "Maximize / Restore Pane", hint: "^Space", cmd: Cmd::MaximizeRestore },
        Command { label: "Retile Layout", hint: "", cmd: Cmd::Retile },
        Command { label: "Focus Next Pane", hint: "Tab", cmd: Cmd::FocusNext },
        Command { label: "Focus Previous Pane", hint: "S-Tab", cmd: Cmd::FocusPrev },
        Command { label: "Interrupt Agent", hint: "^C", cmd: Cmd::Interrupt },
        Command { label: "Restart Session", hint: "", cmd: Cmd::RestartSession },
        Command { label: "Reconnect Sessions", hint: "", cmd: Cmd::ReconnectAll },
        Command { label: "Search Files", hint: "^P", cmd: Cmd::SearchFiles },
        Command { label: "Search Project", hint: "^⇧F", cmd: Cmd::SearchProject },
        Command { label: "Search Conversation", hint: "^F", cmd: Cmd::SearchConversation },
        Command { label: "Toggle Explorer", hint: "^B", cmd: Cmd::ToggleExplorer },
        Command { label: "Git Diff (workspace)", hint: "", cmd: Cmd::GitDiff },
        Command { label: "Git Log", hint: "", cmd: Cmd::GitLog },
        Command { label: "Swap Pane Left", hint: "", cmd: Cmd::SwapLeft },
        Command { label: "Swap Pane Right", hint: "", cmd: Cmd::SwapRight },
        Command { label: "Swap Pane Up", hint: "", cmd: Cmd::SwapUp },
        Command { label: "Swap Pane Down", hint: "", cmd: Cmd::SwapDown },
        Command { label: "Agy model (Gemini)", hint: "/agymodel", cmd: Cmd::AgyModel },
        Command { label: "Refresh (reload build)", hint: "/refresh", cmd: Cmd::Refresh },
        Command { label: "Theme", hint: "/theme", cmd: Cmd::Theme },
        Command { label: "Keybindings", hint: "/keys", cmd: Cmd::Keymap },
        Command { label: "Switch Session", hint: "^O", cmd: Cmd::SwitchSession },
        Command { label: "Resume Session", hint: "^R", cmd: Cmd::ResumeSession },
        Command { label: "Change Tiling", hint: "^T", cmd: Cmd::ChangeTiling },
        Command { label: "Change Model", hint: "/model", cmd: Cmd::ChangeModel },
        Command { label: "Switch Agent", hint: "/agent", cmd: Cmd::SwitchAgent },
        Command { label: "Help", hint: "F1", cmd: Cmd::Help },
        Command { label: "Quit", hint: "^Q", cmd: Cmd::Quit },
    ]
}

// ---------------------------------------------------------------------------

pub struct App {
    pub cfg: Config,
    pub sessions: Vec<SessionState>,
    pub grid: PaneGrid,
    pub focus: u32,
    pub maximized: Option<u32>,
    pub overlay: Overlay,
    pub next_session: u32,
    pub manager: Manager,
    pub initial_dir: PathBuf,
    pub flash: Option<(String, Instant)>,
    /// Guards against a terminal emitting a newline twice per keypress.
    pub last_newline: Option<Instant>,
    pub tick: u64,
    /// Raw frame counter (fast); `tick` advances once per 3 frames.
    pub anim: u64,
    pub dirty: bool,
    pub should_quit: bool,
    /// Set by `/refresh`: after shutdown, re-exec the newest binary.
    pub restart: bool,
    pub git_cache: GitCache,
    pub git_display: Option<GitInfo>,
    pub conv_cache: HashMap<u32, conversation::Cache>,
    pub pending_files: HashMap<ReqId, FileReq>,
    /// Correlate async session creation with the pane that requested it.
    pub pending_create: HashMap<u32, ReqId>,

    pub palette: PaletteState,
    pub newdlg: NewSessionState,
    pub rename: RenameState,
    pub file_search: FileSearchState,
    pub proj_search: ProjSearchState,
    pub conv_search: ConvSearchState,
    pub explorer: ExplorerState,
    pub viewer: Option<ViewerState>,
    pub diff: Option<DiffState>,
    /// Active mouse text selection (conversation transcript).
    pub select: Option<SelectState>,

    pub providers: Vec<ModelEntry>,
    pub default_model: Option<ModelRef>,
    pub agents: Vec<crate::opencode::AgentInfo>,
    pub custom_commands: Vec<crate::opencode::CustomCommand>,
    pub model_picker: ModelPickerState,
    pub agent_picker: AgentPickerState,
    pub session_list: SessionListState,
    pub layout_picker: LayoutPickerState,
    /// Selection in the busy-prompt dialog (0 queue, 1 new workspace).
    pub busy_choice: usize,
    pub resume_picker: ResumePickerState,
    pub keys: crate::keys::Keymap,
    pub keymap_ui: KeymapUi,
    pub theme_ui: ThemeUi,
    /// Sessions per directory, preloaded at boot for instant resume lists.
    pub session_cache: HashMap<PathBuf, Vec<OcSession>>,
    /// Every directory Theta has opened (session lists span all of them).
    pub known_dirs: BTreeSet<PathBuf>,
    /// (new theta session id, prompt) — sent once the forked pane connects.
    pub pending_fork: Vec<(u32, String)>,
    /// (source theta session id, prompt) awaiting the fork to complete.
    pub pending_fork_src: Option<(u32, String)>,
    /// Models offered by the agy CLI (name, description).
    pub agy_models: Vec<(String, String)>,
    /// Agy model picker selection state.
    pub agy_ui: crate::app::LayoutPickerState,

    pub restored: bool,
    #[allow(dead_code)]
    pub started: Instant,
    pub last_save: Instant,
    /// Body area (between header and status bar) from the last render pass.
    pub last_body_area: Rect,
    /// Set during render when the grid cannot fit at minimum sizes.
    pub too_small: bool,
}

impl App {
    pub fn new(cfg: Config, manager: Manager, initial_dir: PathBuf) -> Self {
        let dir_str = initial_dir.to_string_lossy().to_string();
        let key_overrides = cfg.keys.clone();
        Self {
            cfg,
            sessions: Vec::new(),
            grid: PaneGrid::default(),
            focus: 0,
            maximized: None,
            overlay: Overlay::None,
            next_session: 1,
            manager,
            initial_dir,
            flash: None,
            last_newline: None,
            tick: 0,
            anim: 0,
            dirty: true,
            should_quit: false,
            restart: false,
            git_cache: GitCache::new(),
            git_display: None,
            conv_cache: HashMap::new(),
            pending_files: HashMap::new(),
            pending_create: HashMap::new(),
            palette: PaletteState::default(),
            newdlg: NewSessionState {
                field: 0,
                name: InputState::default(),
                dir: dir_str,
                recent: Vec::new(),
                recent_selected: 0,
                recent_req: None,
                recent_loaded: false,
            },
            rename: RenameState {
                input: InputState::default(),
            },
            file_search: FileSearchState {
                input: InputState::default(),
                results: Vec::new(),
                selected: 0,
                req: None,
            },
            proj_search: ProjSearchState {
                input: InputState::default(),
                results: Vec::new(),
                selected: 0,
                req: None,
            },
            conv_search: ConvSearchState {
                input: InputState::default(),
                matches: Vec::new(),
                selected: 0,
            },
            explorer: ExplorerState::default(),
            viewer: None,
            diff: None,
            select: None,
            providers: Vec::new(),
            default_model: None,
            agents: Vec::new(),
            custom_commands: Vec::new(),
            model_picker: ModelPickerState::default(),
            agent_picker: AgentPickerState::default(),
            session_list: SessionListState::default(),
            layout_picker: LayoutPickerState::default(),
            busy_choice: 0,
            resume_picker: ResumePickerState::default(),
            keys: crate::keys::Keymap::load(&key_overrides),
            session_cache: HashMap::new(),
            known_dirs: BTreeSet::new(),
            pending_fork: Vec::new(),
            pending_fork_src: None,
            agy_models: Vec::new(),
            agy_ui: crate::app::LayoutPickerState::default(),
            keymap_ui: KeymapUi::default(),
            theme_ui: ThemeUi::default(),
            restored: false,
            started: Instant::now(),
            last_save: Instant::now(),
            last_body_area: Rect::default(),
            too_small: false,
        }
    }

    // -- session access -------------------------------------------------------

    pub fn session(&self, id: u32) -> Option<&SessionState> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: u32) -> Option<&mut SessionState> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn focused(&self) -> Option<&SessionState> {
        self.session(self.focus)
    }

    pub fn focused_mut(&mut self) -> Option<&mut SessionState> {
        self.session_mut(self.focus)
    }

    pub fn is_busy(&self) -> bool {
        self.sessions
            .iter()
            .any(|s| s.status.is_busy() || s.status == SessStatus::Connecting)
    }

    pub fn working_count(&self) -> usize {
        self.sessions.iter().filter(|s| s.status.is_busy()).count()
    }

    /// Sessions doing anything at all (working, connecting, or waiting on a
    /// permission/question). Drives the status-bar activity slider.
    pub fn active_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| {
                !matches!(s.status, SessStatus::Idle | SessStatus::Error(_))
            })
            .count()
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    // -- session lifecycle ------------------------------------------------------

    pub fn create_session(&mut self, name: &str, dir: PathBuf, model: Option<ModelRef>) {
        let model = model.or_else(|| {
            self.cfg.last_model.as_ref().map(|(p, m)| ModelRef {
                provider_id: p.clone(),
                model_id: m.clone(),
            })
        });
        let id = self.next_session;
        self.next_session += 1;
        let name = if name.trim().is_empty() {
            "session".to_string()
        } else {
            name.trim().to_string()
        };
        let dir = dir.canonicalize().unwrap_or(dir);
        self.remember_dir(&dir);
        self.preload_sessions(dir.clone());
        let mut sess = SessionState::new(id, name.clone(), dir.clone());
        sess.model = model.clone();
        self.sessions.push(sess);
        let area = self.pane_area();
        self.grid.insert_session(id, area.width, area.height);
        self.focus = id;
        self.maximized = None;
        let req = self.manager.next_req();
        self.pending_create.insert(id, req);
        self.manager.connect_session(
            req,
            dir,
            name,
            None,
            model,
            self.cfg.behavior.history_limit,
        );
        self.dirty = true;
        self.save_workspace();
    }

    pub fn close_session(&mut self, id: u32) {
        if let Some(s) = self.session(id) {
            if let (Some(oc_sid), true) = (s.oc_sid.clone(), s.status.is_busy()) {
                self.manager.abort_session(s.dir.clone(), oc_sid);
            }
        }
        self.sessions.retain(|s| s.id != id);
        self.conv_cache.remove(&id);
        self.pending_create.remove(&id);
        let area = self.pane_area();
        self.grid.remove_session(id, area.width, area.height);
        if self.maximized == Some(id) {
            self.maximized = None;
        }
        if self.focus == id {
            self.focus = self.grid.order().first().copied().unwrap_or(0);
        }
        self.dirty = true;
        self.save_workspace();
    }

    pub fn rename_session(&mut self, id: u32, name: &str) {
        if let Some(s) = self.session_mut(id) {
            s.name = name.trim().to_string();
        }
        self.dirty = true;
        self.save_workspace();
    }

    pub fn restart_session(&mut self, id: u32) {
        let Some(s) = self.session(id) else { return };
        if let Some(oc_sid) = s.oc_sid.clone() {
            self.manager.abort_session(s.dir.clone(), oc_sid);
        }
        let (dir, model, name) = (s.dir.clone(), s.model.clone(), s.name.clone());
        if let Some(s) = self.session_mut(id) {
            s.messages.clear();
            s.oc_sid = None;
            s.status = SessStatus::Connecting;
            s.last_error = None;
            s.pending_perm = None;
            s.scroll = 0;
            s.stick_bottom = true;
            s.expanded.clear();
            s.tool_cursor = None;
            s.dirty = true;
        }
        self.conv_cache.remove(&id);
        let req = self.manager.next_req();
        self.pending_create.insert(id, req);
        self.manager.connect_session(
            req,
            dir,
            name,
            None,
            model,
            self.cfg.behavior.history_limit,
        );
        self.flash("session restarted");
        self.dirty = true;
    }

    pub fn reconnect_all(&mut self) {
        let sessions: Vec<(u32, PathBuf, String, Option<String>, Option<ModelRef>)> = self
            .sessions
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.dir.clone(),
                    s.name.clone(),
                    s.oc_sid.clone(),
                    s.model.clone(),
                )
            })
            .collect();
        for (id, dir, name, oc_sid, model) in sessions {
            if let Some(s) = self.session_mut(id) {
                s.status = SessStatus::Connecting;
                s.dirty = true;
            }
            let req = self.manager.next_req();
            self.pending_create.insert(id, req);
            self.manager.connect_session(
                req,
                dir,
                name,
                oc_sid,
                model,
                self.cfg.behavior.history_limit,
            );
        }
        self.flash("reconnecting…");
        self.dirty = true;
    }

    pub fn submit_input(&mut self, id: u32) {
        let limit = self.cfg.ui.history_limit;
        let (text, dir, oc_sid, model, agent, agy) = {
            let Some(s) = self.session_mut(id) else { return };
            if s.oc_sid.is_none() {
                self.flash("session is still connecting…");
                return;
            }
            let text = s.input.take();
            if text.is_empty() {
                return;
            }
            // Guard against double-Enter resends (provider rate limits).
            if let Some((at, last)) = &s.last_send {
                if at.elapsed() < std::time::Duration::from_secs(2) && *last == text {
                    s.input.buf = text;
                    s.input.cursor = s.input.buf.chars().count();
                    self.flash("already sending this prompt…");
                    return;
                }
            }
            // Agent busy: hold the prompt and open the queue / new-workspace
            // choice dialog. Nothing is sent until the user decides.
            if s.status.is_busy() {
                s.pending_send = Some(text.clone());
                s.dirty = true;
                drop(s);
                self.busy_choice = 0;
                self.overlay = Overlay::BusyChoice;
                self.dirty = true;
                return;
            }
            s.last_send = Some((
                std::time::Instant::now(),
                text.clone(),
            ));
            s.input.push_history(&text, limit);
            s.push_local_user(&text);
            s.last_error = None;
            if matches!(s.status, SessStatus::Idle | SessStatus::Error(_)) {
                s.status = SessStatus::Working;
            }
            s.stick_bottom = true;
            s.tool_cursor = None;
            s.dirty = true;
            (
                text,
                s.dir.clone(),
                s.oc_sid.clone().unwrap_or_default(),
                s.model.clone(),
                s.agent.clone(),
                s.agy_model.clone(),
            )
        };
        if let Some(agy_model) = agy {
            self.spawn_agy_run(id, dir, text, agy_model);
        } else {
            self.manager.send_prompt(dir, oc_sid, text, model, agent);
        }
    }

    /// Fire a prompt into a connected session (submit path and queue drain).
    fn send_text_now(&mut self, id: u32, text: &str) {
        let (dir, oc_sid, model, agent, agy) = {
            let Some(s) = self.session_mut(id) else { return };
            s.push_local_user(text);
            s.last_error = None;
            if matches!(s.status, SessStatus::Idle | SessStatus::Error(_)) {
                s.status = SessStatus::Working;
            }
            s.stick_bottom = true;
            s.tool_cursor = None;
            s.last_send = Some((std::time::Instant::now(), text.to_string()));
            s.dirty = true;
            (
                s.dir.clone(),
                s.oc_sid.clone().unwrap_or_default(),
                s.model.clone(),
                s.agent.clone(),
                s.agy_model.clone(),
            )
        };
        if let Some(agy_model) = agy {
            // Gemini via the agy CLI — not the OpenCode provider.
            self.spawn_agy_run(id, dir, text.to_string(), agy_model);
        } else {
            self.manager
                .send_prompt(dir, oc_sid, text.to_string(), model, agent);
        }
    }

    /// Send the next queued prompt when the agent idles.
    fn drain_queue(&mut self, id: u32) {
        let next = {
            let Some(s) = self.session_mut(id) else { return };
            if s.queue.is_empty() || s.oc_sid.is_none() {
                return;
            }
            s.queue.remove(0)
        };
        self.flash("queued prompt sending…");
        self.send_text_now(id, &next);
    }

    /// Confirm the busy-prompt dialog: queue the held prompt on this session,
    /// or fork into a new workspace and send it there.
    pub fn confirm_busy_choice(&mut self, id: u32, choice: usize) {
        let Some(text) = self.session_mut(id).and_then(|s| s.pending_send.take()) else {
            self.overlay = Overlay::None;
            return;
        };
        self.overlay = Overlay::None;
        if choice == 0 {
            let busy = self.session(id).map(|s| s.status.is_busy()).unwrap_or(false);
            if let Some(s) = self.session_mut(id) {
                s.queue.push(text);
                s.dirty = true;
            }
            if busy {
                self.flash("queued — sends when the agent finishes");
            } else {
                // The agent finished while the dialog was open: send now.
                self.drain_queue(id);
            }
        } else {
            self.fork_with_prompt(id, text);
        }
        self.dirty = true;
    }

    /// Fork `id` into a fresh pane sharing its history, then send `text`
    /// there once the fork connects. History is never modified.
    fn fork_with_prompt(&mut self, id: u32, text: String) {
        let (dir, oc_sid, at) = {
            let Some(s) = self.session(id) else { return };
            match &s.oc_sid {
                Some(oc) => (s.dir.clone(), oc.clone(), Self::fork_point(s)),
                None => {
                    // Not connected yet: queue on the current session instead.
                    if let Some(sm) = self.session_mut(id) {
                        sm.queue.push(text);
                        sm.dirty = true;
                    }
                    self.flash("session still connecting — queued instead");
                    return;
                }
            }
        };
        self.pending_fork_src = Some((id, text));
        self.manager.fork_session(dir, oc_sid, id, at);
        self.flash("forking workspace — prompt goes to the new one…");
    }

    /// The message to fork at: the last completed assistant message. This
    /// excludes any in-progress turn *and* the pending user prompt, so the new
    /// workspace starts from a clean, settled state and the source session is
    /// left untouched.
    fn fork_point(s: &SessionState) -> Option<String> {
        for m in s.messages.iter().rev() {
            if !m.id.starts_with("msg") || m.role != Role::Assistant {
                continue;
            }
            let running = m.parts.iter().any(|p| {
                matches!(
                    &p.kind,
                    PartKind::Tool(t)
                        if matches!(t.status, ToolStatus::Pending | ToolStatus::Running)
                )
            });
            if m.completed.is_some() && !running {
                return Some(m.id.clone());
            }
        }
        None
    }

    /// Reload the newest binary in place: save state, then re-exec on exit.
    pub fn request_refresh(&mut self) {
        self.save_workspace();
        self.restart = true;
        self.should_quit = true;
        self.flash("refreshing — reloading the newest build…");
        self.dirty = true;
    }

    /// `/push`: stage, commit and push the session's project. Progress shows in
    /// the workspace activity strip with a small mono animation.
    pub fn start_push(&mut self, id: u32, statement: &str) {
        let Some(dir) = self.session(id).map(|s| s.dir.clone()) else {
            return;
        };
        let statement = statement.trim().to_string();
        let label = if statement.is_empty() {
            "Git commit".to_string()
        } else {
            format!("Git commit \"{statement}\"")
        };
        if let Some(s) = self.session_mut(id) {
            s.activity = Some((label, Instant::now()));
            s.dirty = true;
        }
        crate::tlog!(
            "PUSH session={id} dir={} message={}",
            dir.display(),
            if statement.is_empty() { "<auto>" } else { &statement }
        );
        self.flash("committing…");
        self.dirty = true;
        let tx = self.manager.tx();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::PushProgress {
                session: id,
                text: "Git add".into(),
            });
            let result = crate::git::commit_and_push(&dir, &statement).await;
            let _ = tx.send(AppEvent::PushDone {
                session: id,
                ok: result.is_ok(),
                message: result.as_ref().err().map(|e| e.to_string()).unwrap_or_default(),
                repo: result.ok(),
            });
        });
    }

    /// Send the accumulated answers for a session's pending question.
    pub fn answer_question(&mut self, id: u32) {
        let req = {
            let Some(s) = self.session(id) else { return };
            let Some(pq) = s.pending_question.as_ref() else { return };
            (s.dir.clone(), pq.id.clone(), pq.answers.clone())
        };
        self.manager.reply_question(req.0, req.1, req.2);
        if let Some(s) = self.session_mut(id) {
            s.pending_question = None;
            if s.status == SessStatus::Question {
                s.status = SessStatus::Working;
            }
            s.dirty = true;
        }
        self.open_pending_question();
        self.dirty = true;
    }

    /// Decline a session's pending question.
    pub fn reject_question(&mut self, id: u32) {
        let req = {
            let Some(s) = self.session(id) else { return };
            let Some(pq) = s.pending_question.as_ref() else { return };
            (s.dir.clone(), pq.id.clone())
        };
        self.manager.reject_question(req.0, req.1);
        if let Some(s) = self.session_mut(id) {
            s.pending_question = None;
            if s.status == SessStatus::Question {
                s.status = SessStatus::Working;
            }
            s.dirty = true;
        }
        self.open_pending_question();
        self.dirty = true;
    }

    /// Focus and show the next session waiting on a question, if any.
    pub fn open_pending_question(&mut self) {
        if let Some(sid) = self
            .sessions
            .iter()
            .find(|s| s.pending_question.is_some())
            .map(|s| s.id)
        {
            self.focus = sid;
            self.overlay = Overlay::Question;
            self.dirty = true;
        } else if self.overlay == Overlay::Question {
            self.overlay = Overlay::None;
            self.dirty = true;
        }
    }

    pub fn interrupt(&mut self, id: u32) {
        if let Some(s) = self.session(id) {
            if let Some(oc_sid) = s.oc_sid.clone() {
                self.manager.abort_session(s.dir.clone(), oc_sid);
            }
        }
        if let Some(s) = self.session_mut(id) {
            s.status = SessStatus::Idle;
            s.interrupt_armed = None;
            s.dirty = true;
        }
        self.flash("interrupted");
    }

    pub fn permission_reply(&mut self, id: u32, response: &'static str) {
        let request = {
            let Some(s) = self.session(id) else { return };
            match (&s.pending_perm, &s.oc_sid) {
                (Some(p), Some(oc_sid)) => Some((s.dir.clone(), oc_sid.clone(), p.id.clone())),
                _ => None,
            }
        };
        if let Some((dir, oc_sid, pid)) = request {
            self.manager
                .reply_permission(dir, oc_sid, pid, response.to_string());
        }
        if let Some(s) = self.session_mut(id) {
            s.pending_perm = None;
            s.status = SessStatus::Working;
            s.dirty = true;
        }
        self.flash(match response {
            "reject" => "permission rejected",
            "always" => "allowed (always)",
            _ => "allowed",
        });
    }

    // -- pane ops -----------------------------------------------------------------

    fn pane_area(&self) -> Rect {
        Rect {
            x: 0,
            y: 2,
            width: 120,
            height: 36,
        }
    }

    pub fn focus_next(&mut self) {
        let order = self.grid.order();
        if order.is_empty() {
            return;
        }
        let pos = order.iter().position(|s| *s == self.focus).unwrap_or(0);
        self.focus = order[(pos + 1) % order.len()];
        self.dirty = true;
    }

    pub fn focus_prev(&mut self) {
        let order = self.grid.order();
        if order.is_empty() {
            return;
        }
        let pos = order.iter().position(|s| *s == self.focus).unwrap_or(0);
        self.focus = order[(pos + order.len() - 1) % order.len()];
        self.dirty = true;
    }

    pub fn focus_nth(&mut self, n: usize) {
        let order = self.grid.order();
        if let Some(s) = order.get(n) {
            self.focus = *s;
            self.dirty = true;
        }
    }

    pub fn move_focus(&mut self, dir: Dir, rects: &[(u32, Rect)]) {
        let (cur_id, cur_rect) = match self.focused() {
            Some(s) => (s.id, rects.iter().find(|(sid, _)| *sid == s.id).map(|(_, r)| *r)),
            None => return,
        };
        if let Some(cr) = cur_rect {
            if let Some(target) = geometric_neighbor(cr, dir, rects) {
                self.focus = target;
                self.dirty = true;
                return;
            }
        }
        if let Some(t) = self.grid.neighbor(cur_id, dir, rects) {
            self.focus = t;
            self.dirty = true;
        }
    }

    pub fn resize_pane(&mut self, dir: Dir) {
        if self.grid.resize(self.focus, dir, 0.08) {
            self.dirty = true;
        } else {
            self.flash("no pane in that direction");
        }
    }

    pub fn swap_pane(&mut self, dir: Dir, rects: &[(u32, Rect)]) {
        if self.grid.swap(self.focus, dir, rects) {
            self.dirty = true;
            self.save_workspace();
        } else {
            self.flash("no pane in that direction");
        }
    }

    pub fn toggle_maximize(&mut self) {
        self.maximized = if self.maximized.is_some() {
            None
        } else {
            Some(self.focus)
        };
        self.dirty = true;
    }

    pub fn retile(&mut self) {
        let (w, h) = (self.last_body_area.width, self.last_body_area.height);
        self.grid.retile(w.max(40), h.max(16));
        self.dirty = true;
        self.save_workspace();
    }

    // -- overlays --------------------------------------------------------------------

    pub fn toggle_explorer(&mut self) {
        self.explorer.open = !self.explorer.open;
        if self.explorer.open {
            self.explorer.root = self
                .focused()
                .map(|s| s.dir.clone())
                .unwrap_or_else(|| self.initial_dir.clone());
            self.rebuild_explorer();
        }
        self.dirty = true;
    }

    pub fn rebuild_explorer(&mut self) {
        let root = self.explorer.root.clone();
        let expanded = self.explorer.expanded.clone();
        let mut items = Vec::new();
        walk_tree(Path::new(&root), 0, &expanded, &mut items);
        self.explorer.items = items;
        self.explorer.dirty = false;
        self.dirty = true;
    }

    /// Handle explorer keys; returns true when the key was consumed.
    fn explorer_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                self.explorer.selected = self.explorer.selected.saturating_sub(1);
                self.dirty = true;
                true
            }
            KeyCode::Down => {
                if self.explorer.selected + 1 < self.explorer.items.len() {
                    self.explorer.selected += 1;
                }
                self.dirty = true;
                true
            }
            KeyCode::Right => {
                if let Some(item) = self.explorer.items.get(self.explorer.selected) {
                    if item.is_dir {
                        self.explorer.expanded.insert(item.path.clone());
                        self.rebuild_explorer();
                    }
                }
                true
            }
            KeyCode::Left => {
                if let Some(item) = self.explorer.items.get(self.explorer.selected) {
                    if item.is_dir {
                        self.explorer.expanded.remove(&item.path);
                        self.rebuild_explorer();
                    }
                }
                true
            }
            KeyCode::Enter => {
                let target = self.explorer.items.get(self.explorer.selected).cloned();
                if let Some(item) = target {
                    if item.is_dir {
                        if !self.explorer.expanded.remove(&item.path) {
                            self.explorer.expanded.insert(item.path.clone());
                        }
                        self.rebuild_explorer();
                    } else {
                        self.open_file_viewer(&item.path, None);
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn open_file_viewer(&mut self, path: &str, line: Option<u64>) {        let dir = self
            .focused()
            .map(|s| s.dir.clone())
            .unwrap_or_else(|| self.initial_dir.clone());
        let req = self.manager.next_req();
        self.pending_files.insert(req, FileReq::Viewer { line });
        self.manager.load_file(req, dir, path.to_string());
        self.dirty = true;
    }

    pub async fn open_tool_diff(&mut self, id: u32) {
        let (dir, file, inline_diff) = {
            let Some(s) = self.session(id) else { return };
            let Some(cursor) = s.tool_cursor else { return };
            let Some(tool) = s.tool_at(cursor) else { return };
            (s.dir.clone(), tool.file_path(), tool.diff())
        };
        if let Some(diff) = inline_diff {
            self.diff = Some(DiffState {
                title: format!("diff — {}", file.clone().unwrap_or_else(|| "file".into())),
                lines: crate::highlight::diff_lines(&diff),
                scroll: 0,
            });
            self.dirty = true;
            return;
        }
        if let Some(file) = file {
            let diff = crate::git::file_diff(&dir, &file).await.unwrap_or_default();
            if diff.trim().is_empty() {
                self.flash("no diff available");
                return;
            }
            self.diff = Some(DiffState {
                title: format!("diff — {file}"),
                lines: crate::highlight::diff_lines(&diff),
                scroll: 0,
            });
            self.dirty = true;
        } else {
            self.flash("no diff for this tool");
        }
    }

    pub async fn open_workspace_diff(&mut self) {
        let Some(dir) = self.focused().map(|s| s.dir.clone()) else {
            self.flash("no session");
            return;
        };
        let diff = crate::git::workspace_diff(&dir).await.unwrap_or_default();
        if diff.trim().is_empty() {
            self.flash("working tree clean");
            return;
        }
        self.diff = Some(DiffState {
            title: "git diff — workspace".into(),
            lines: crate::highlight::diff_lines(&diff),
            scroll: 0,
        });
        self.dirty = true;
    }

    pub async fn open_git_log(&mut self) {
        let Some(dir) = self.focused().map(|s| s.dir.clone()) else {
            return;
        };
        let commits = crate::git::recent_commits(&dir, 30).await.unwrap_or_default();
        if commits.is_empty() {
            self.flash("no git history");
            return;
        }
        let lines: Vec<Line<'static>> = commits
            .into_iter()
            .map(|c| {
                let c = c.clone();
                let mut spans = Vec::new();
                if let Some((hash, subject)) = c.split_once(' ') {
                    spans.push(Span::styled(format!("{hash} "), theme::fg(pal().orange)));
                    spans.push(Span::styled(subject.to_string(), theme::fg(pal().fg)));
                } else {
                    spans.push(Span::styled(c, theme::fg(pal().fg)));
                }
                Line::from(spans)
            })
            .collect();
        self.diff = Some(DiffState {
            title: "git log".into(),
            lines,
            scroll: 0,
        });
        self.dirty = true;
    }

    pub fn open_conv_search(&mut self) {
        self.conv_search.input.clear();
        self.conv_search.matches.clear();
        self.conv_search.selected = 0;
        self.overlay = Overlay::ConvSearch;
        self.dirty = true;
    }

    pub fn update_conv_matches(&mut self) {
        let Some(s) = self.focused() else { return };
        let q = self.conv_search.input.text().trim().to_lowercase();
        let mut matches: Vec<ConvMatch> = Vec::new();
        if !q.is_empty() {
            let width = self.last_body_area.width.max(40);
            let tick = self.tick;
            let cache = self
                .conv_cache
                .get(&s.id)
                .filter(|c| c.width == width)
                .cloned();
            let cache = match cache {
                Some(c) => c,
                None => conversation::build_cache(s, width, tick),
            };
            for (bi, b) in cache.blocks.iter().enumerate() {
                let text = b.text.to_lowercase();
                if text.contains(&q) {
                    let pos = text.find(&q).unwrap_or(0);
                    let start = text
                        .char_indices()
                        .nth(text[..pos].chars().count().saturating_sub(10))
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let excerpt: String = text
                        .chars()
                        .skip(text[..start].chars().count())
                        .take(60)
                        .collect();
                    matches.push(ConvMatch {
                        block: bi,
                        label: b.label(),
                        excerpt: excerpt.replace('\n', " "),
                    });
                    if matches.len() >= 100 {
                        break;
                    }
                }
            }
        }
        self.conv_search.matches = matches;
        self.conv_search.selected = 0;
        self.dirty = true;
    }

    pub fn jump_to_conv_match(&mut self) {
        let sid = {
            let Some(s) = self.focused() else { return };
            let Some(m) = self.conv_search.matches.get(self.conv_search.selected) else {
                return;
            };
            (s.id, m.block)
        };
        let start = self
            .conv_cache
            .get(&sid.0)
            .and_then(|cache| cache.blocks.get(sid.1))
            .map(|b| b.start);
        if let Some(start) = start {
            if let Some(s) = self.session_mut(sid.0) {
                s.stick_bottom = false;
                s.scroll = start.saturating_sub(2);
                s.dirty = true;
            }
        }
        self.overlay = Overlay::None;
        self.dirty = true;
    }

    // -- persistence --------------------------------------------------------------------

    pub fn snapshot(&self) -> persist::Workspace {
        let sessions: Vec<persist::SavedSession> = self
            .sessions
            .iter()
            .map(|s| persist::SavedSession {
                name: s.name.clone(),
                dir: s.dir.to_string_lossy().to_string(),
                oc_sid: s.oc_sid.clone(),
                model: s
                    .model
                    .as_ref()
                    .map(|m| (m.provider_id.clone(), m.model_id.clone())),
                agent: s.agent.clone(),
            })
            .collect();
        let idx = |sid: u32| self.sessions.iter().position(|s| s.id == sid).unwrap_or(0);
        let rows = self
            .grid
            .rows
            .iter()
            .map(|r| persist::SavedRow {
                weight: r.weight,
                cells: r
                    .cells
                    .iter()
                    .map(|c| (c.weight, idx(c.session)))
                    .collect(),
            })
            .collect();
        persist::Workspace {
            sessions,
            rows,
            focused: idx(self.focus),
            maximized: self.maximized.map(idx),
            scheme: Some(self.grid.scheme.key().to_string()),
            known_dirs: self
                .known_dirs
                .iter()
                .map(|d| d.to_string_lossy().to_string())
                .collect(),
        }
    }

    pub fn save_workspace(&mut self) {
        self.last_save = Instant::now();
        let snap = self.snapshot();
        let _ = persist::save(&snap);
    }

    pub fn restore_workspace(&mut self) {
        let Some(ws) = persist::load() else { return };
        // Restore the directory set even when no panes are open, and warm the
        // session cache for every one so lists span projects.
        for d in &ws.known_dirs {
            let dir = PathBuf::from(d);
            if !dir.as_os_str().is_empty() {
                self.remember_dir(&dir);
            }
        }
        for saved in &ws.sessions {
            if !saved.dir.trim().is_empty() {
                self.remember_dir(&PathBuf::from(&saved.dir));
            }
        }
        if ws.sessions.is_empty() {
            return;
        }
        let mut new_ids: Vec<u32> = Vec::new();
        for saved in &ws.sessions {
            let id = self.next_session;
            self.next_session += 1;
            let dir = PathBuf::from(&saved.dir);
            self.remember_dir(&dir);
            self.preload_sessions(dir.clone());
            let mut sess = SessionState::new(id, saved.name.clone(), dir.clone());
            sess.oc_sid = saved.oc_sid.clone();
            sess.agent = saved.agent.clone();
            sess.model = saved
                .model
                .as_ref()
                .map(|(p, m)| ModelRef {
                    provider_id: p.clone(),
                    model_id: m.clone(),
                });
            sess.status = SessStatus::Connecting;
            let model = sess.model.clone();
            self.sessions.push(sess);
            new_ids.push(id);
            let req = self.manager.next_req();
            self.pending_create.insert(id, req);
            self.manager.connect_session(
                req,
                dir,
                saved.name.clone(),
                saved.oc_sid.clone(),
                model,
                self.cfg.behavior.history_limit,
            );
        }
        let mut rows = Vec::new();
        for r in &ws.rows {
            let cells: Vec<crate::panes::Cell> = r
                .cells
                .iter()
                .filter_map(|(w, i)| {
                    new_ids.get(*i).map(|sid| crate::panes::Cell {
                        weight: *w,
                        session: *sid,
                    })
                })
                .collect();
            if !cells.is_empty() {
                rows.push(crate::panes::Row {
                    weight: r.weight,
                    cells,
                });
            }
        }
        let scheme = ws
            .scheme
            .as_deref()
            .and_then(crate::panes::Scheme::from_str)
            .unwrap_or_default();
        if rows.is_empty() {
            let area = self.pane_area();
            self.grid = PaneGrid::build(&new_ids, scheme, area.width, area.height);
        } else {
            self.grid = PaneGrid { rows, scheme };
        }
        let clamp = |i: usize| i.min(new_ids.len().saturating_sub(1));
        self.focus = new_ids[clamp(ws.focused)];
        self.maximized = ws.maximized.map(|i| new_ids[clamp(i)]);
        self.restored = true;
        self.dirty = true;
    }

    // -- slash commands -------------------------------------------------------------

    pub fn open_model_picker(&mut self) {
        // Providers may still be loading on a cold start — the picker opens
        // anyway and on_tick refreshes the list until it arrives.
        self.model_picker.input.clear();
        self.model_picker.selected = 0;
        if self.agy_models.is_empty() {
            self.fetch_agy_models();
        }
        self.overlay = Overlay::ModelPicker;
        self.dirty = true;
    }

    pub fn open_agent_picker(&mut self) {
        self.agent_picker.input.clear();
        self.agent_picker.selected = 0;
        self.overlay = Overlay::AgentPicker;
        self.dirty = true;
    }

    /// Live-preview a theme while scrolling the picker (not persisted
    /// until Enter).
    fn preview_theme(&mut self, names: &[&'static str]) {
        if let Some(name) = names.get(self.theme_ui.selected) {
            crate::theme::set_theme(name);
            self.conv_cache.clear();
        }
        self.dirty = true;
    }

    /// Warm the resume cache for a directory (called at boot).
    pub fn preload_sessions(&mut self, dir: PathBuf) {
        let dir = dir.canonicalize().unwrap_or(dir);
        if !dir.is_dir() {
            return;
        }
        if !self.session_cache.contains_key(&dir) {
            self.manager.preload_sessions(dir.clone());
        }
    }

    /// Remember a directory so its sessions stay visible across folders.
    pub fn remember_dir(&mut self, dir: &Path) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.known_dirs.insert(dir);
    }

    /// All sessions from every known directory, newest first (deduped by id).
    pub fn all_cached_sessions(&self) -> Vec<OcSession> {
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<OcSession> = self
            .session_cache
            .values()
            .flat_map(|v| v.iter().cloned())
            .filter(|s| seen.insert(s.id.clone()))
            .collect();
        out.sort_by_key(|s| -s.updated_ms.unwrap_or(0));
        out
    }

    pub fn push_local_user(&mut self, id: u32, text: &str) {
        if let Some(s) = self.session_mut(id) {
            s.push_local_user(text);
        }
    }

    /// Copy text to the system clipboard via OSC 52 (works over SSH too).
    fn copy_clipboard(&mut self, text: &str) {
        let b64 = Self::base64_encode(text.as_bytes());
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", b64);
        let _ = out.flush();
        self.flash(format!("copied {} chars", text.chars().count()));
    }

    /// Spawn an agy CLI turn (Gemini models) in the session folder.
    fn spawn_agy_run(&mut self, id: u32, dir: PathBuf, text: String, model: String) {
        use std::process::Stdio;
        crate::tlog!(
            "AGY dir={} session={} model={} text={}",
            dir.display(),
            id,
            model,
            crate::logging::snippet(&text, 2000)
        );
        self.push_local_user(id, &text);
        if let Some(s) = self.session_mut(id) {
            s.status = SessStatus::Working;
            s.dirty = true;
        }
        let tx = self.manager_tx();
        tokio::spawn(async move {
            let out = tokio::process::Command::new("agy")
                .arg("-p")
                .arg(&text)
                .arg("--model")
                .arg(&model)
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await;
            let ok = out.as_ref().map(|o| o.status.success()).unwrap_or(false);
            let body = out
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            let _ = tx.send(AppEvent::AgyDone {
                session: id,
                ok,
                output: body.trim().to_string(),
                model,
            });
        });
    }

    fn manager_tx(&self) -> tokio::sync::mpsc::UnboundedSender<AppEvent> {
        self.manager.tx()
    }

    /// Run a prompt through the agy CLI (Gemini models) in the session folder.
    pub fn run_agy(&mut self, id: u32, prompt: &str) {
        let model = self
            .session(id)
            .and_then(|s| s.agy_model.clone())
            .unwrap_or_else(|| self.cfg.agy_model.clone());
        let dir = self.session(id).map(|s| s.dir.clone());
        if let Some(dir) = dir {
            self.spawn_agy_run(id, dir, prompt.to_string(), model);
        }
    }

    /// Fetch the model list from the agy CLI.
    pub fn fetch_agy_models(&mut self) {
        let tx = self.manager_tx();
        let _ = std::fs::write("/tmp/agy-start.txt", format!("started, agy_models={}", self.agy_models.len()));
        tokio::spawn(async move {
            let out = tokio::process::Command::new("agy")
                .arg("models")
                .output()
                .await;
            let Ok(out) = out else { return };
            let mut models = Vec::new();
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some((name, desc)) = line.split_once('\t') {
                    models.push((name.trim().to_string(), desc.trim().to_string()));
                }
            }
            if !models.is_empty() {
                let _ = tx.send(AppEvent::AgyModels { models });
            } else {
                let _ = std::fs::write("/tmp/agy-fetch-debug.txt", out.stdout);
            }
        });
    }

    fn base64_encode(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
        }
        out
    }

    pub fn open_agy_model_picker(&mut self) {
        if self.agy_models.is_empty() {
            self.fetch_agy_models();
        }
        self.agy_ui.selected = 0;
        self.overlay = Overlay::AgyModel;
        self.dirty = true;
    }

    pub fn open_theme_picker(&mut self) {
        let names = crate::theme::theme_names();
        let cur = crate::theme::current_name().to_string();
        self.theme_ui.original = cur.clone();
        self.theme_ui.selected = names
            .iter()
            .position(|n| *n == cur)
            .unwrap_or(0);
        self.overlay = Overlay::Theme;
        self.dirty = true;
    }

    pub fn open_keymap(&mut self) {
        self.keymap_ui.selected = 0;
        self.keymap_ui.capturing = None;
        self.overlay = Overlay::Keymap;
        self.dirty = true;
    }

    pub fn open_session_list(&mut self) {
        self.session_list.selected = 0;
        self.overlay = Overlay::SessionList;
        self.dirty = true;
    }

    /// Model picker entries: "default" first, then provider/model labels.
    pub fn picker_models(&self) -> Vec<(String, Option<ModelRef>)> {
        let mut v = vec![(
            "default".to_string(),
            self.default_model.clone(),
        )];
        // agy CLI models (Gemini etc.) first — route through the local agy
        // binary — then the OpenCode providers.
        for (name, desc) in &self.agy_models {
            v.push((
                format!("agy/{name} — {desc}"),
                Some(ModelRef {
                    provider_id: "agy".into(),
                    model_id: name.clone(),
                }),
            ));
        }
        v.extend(self.providers.iter().map(|p| {
            (
                p.label.clone(),
                Some(ModelRef {
                    provider_id: p.provider_id.clone(),
                    model_id: p.model_id.clone(),
                }),
            )
        }));
        v
    }

    pub fn picker_agents(&self) -> Vec<(String, String)> {
        let mut v = vec![("default".to_string(), "agent configured by the server".to_string())];
        v.extend(
            self.agents
                .iter()
                .map(|a| (a.name.clone(), a.description.clone())),
        );
        v
    }

    fn set_model(&mut self, id: u32, model: Option<ModelRef>, label: &str) {
        crate::tlog!("MODEL session={id} -> {label}");
        let is_agy = model.as_ref().map(|m| m.provider_id == "agy").unwrap_or(false);
        if let Some(s) = self.session_mut(id) {
            if is_agy {
                s.model = None;
                s.agy_model = model.as_ref().map(|m| m.model_id.clone());
            } else {
                s.model = model.clone();
                s.agy_model = None;
            }
        }
        // Remember as the default for future sessions.
        if let Some(m) = &model {
            self.cfg.last_model = Some((m.provider_id.clone(), m.model_id.clone()));
            let _ = self.cfg.save();
        }
        self.flash(format!("model: {label}"));
        self.overlay = Overlay::None;
        self.dirty = true;
        self.save_workspace();
    }

    fn set_agent(&mut self, id: u32, agent: Option<String>, label: &str) {
        if let Some(s) = self.session_mut(id) {
            s.agent = agent;
        }
        self.flash(format!("agent: {label}"));
        self.overlay = Overlay::None;
        self.dirty = true;
        self.save_workspace();
    }

    /// Execute a "/command …" line from the input box.
    pub fn execute_slash(&mut self, raw: &str) {
        let raw = raw.trim();
        let Some(rest) = raw.strip_prefix('/') else { return };
        let (name, args) = match rest.split_once(' ') {
            Some((n, a)) => (n.trim(), a.trim()),
            None => (rest.trim(), ""),
        };
        if name.is_empty() {
            return;
        }
        let sid = self.focus;

        match name {
            "model" => {
                if args.is_empty() {
                    self.open_model_picker();
                    return;
                }
                if args.eq_ignore_ascii_case("default") {
                    let label = self
                        .default_model
                        .as_ref()
                        .map(|m| format!("{}/{}", m.provider_id, m.model_id))
                        .unwrap_or_else(|| "default".into());
                    let m = self.default_model.clone();
                    self.set_model(sid, m, &label);
                    return;
                }
                let all = self.providers.clone();
                let found = all.iter().find(|p| {
                    p.label.eq_ignore_ascii_case(args) || p.model_id.eq_ignore_ascii_case(args)
                });
                match found {
                    Some(p) => {
                        let label = p.label.clone();
                        let m = ModelRef {
                            provider_id: p.provider_id.clone(),
                            model_id: p.model_id.clone(),
                        };
                        self.set_model(sid, Some(m), &label);
                    }
                    None => self.flash(format!("unknown model '{args}' — try /model")),
                }
            }
            "agent" => {
                if args.is_empty() {
                    self.open_agent_picker();
                    return;
                }
                if args.eq_ignore_ascii_case("default") {
                    self.set_agent(sid, None, "default");
                    return;
                }
                let found = self
                    .agents
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case(args))
                    .map(|a| a.name.clone());
                match found {
                    Some(n) => self.set_agent(sid, Some(n.clone()), &n),
                    None => self.flash(format!("unknown agent '{args}' — try /agent")),
                }
            }
            "new" => self.open_new_dialog(),
            "sessions" => self.open_session_list(),
            "resume" => self.open_resume_picker(),
            "clear" => {
                if let Some(s) = self.session_mut(sid) {
                    s.messages.clear();
                    s.last_error = None;
                    s.scroll = 0;
                    s.stick_bottom = true;
                    s.dirty = true;
                }
                self.conv_cache.remove(&sid);
                self.flash("view cleared (server history kept)");
            }
            "compact" => {
                let req = {
                    let Some(s) = self.session(sid) else { return };
                    match (s.oc_sid.clone(), s.model.clone().or_else(|| self.default_model.clone())) {
                        (Some(oc), Some(m)) => Some((s.dir.clone(), oc, m)),
                        (Some(_), None) => {
                            self.flash("no model available for compact");
                            None
                        }
                        _ => {
                            self.flash("session is still connecting…");
                            None
                        }
                    }
                };
                if let Some((dir, oc, m)) = req {
                    self.manager.summarize(dir, oc, m);
                    self.flash("compacting…");
                }
            }
            "undo" => {
                let req = {
                    let Some(s) = self.session(sid) else { return };
                    match (s.oc_sid.clone(), s.messages.iter().rev().find(|m| m.role == Role::User)) {
                        (Some(oc), Some(m)) => Some((s.dir.clone(), oc, m.id.clone())),
                        (Some(_), None) => {
                            self.flash("nothing to undo");
                            None
                        }
                        _ => {
                            self.flash("session is still connecting…");
                            None
                        }
                    }
                };
                if let Some((dir, oc, msg)) = req {
                    self.manager.revert(dir, oc, msg);
                }
            }
            "redo" => {
                let req = {
                    let Some(s) = self.session(sid) else { return };
                    s.oc_sid.clone().map(|oc| (s.dir.clone(), oc))
                };
                if let Some((dir, oc)) = req {
                    self.manager.unrevert(dir, oc);
                }
            }
            "share" => {
                let req = {
                    let Some(s) = self.session(sid) else { return };
                    s.oc_sid.clone().map(|oc| (s.dir.clone(), oc))
                };
                if let Some((dir, oc)) = req {
                    self.manager.share(dir, oc, true);
                }
            }
            "unshare" => {
                let req = {
                    let Some(s) = self.session(sid) else { return };
                    s.oc_sid.clone().map(|oc| (s.dir.clone(), oc))
                };
                if let Some((dir, oc)) = req {
                    self.manager.share(dir, oc, false);
                }
            }
            "init" => {
                let req = {
                    let Some(s) = self.session(sid) else { return };
                    s.oc_sid.clone().map(|oc| (s.dir.clone(), oc))
                };
                if let Some((dir, oc)) = req {
                    let args = args.to_string();
                    self.manager.run_command(dir, oc, "init".into(), args);
                    self.flash("running /init…");
                }
            }
            "close" => self.close_session(self.focus),
            "keys" | "help" => self.open_keymap(),
            "agy" => {
                if args.is_empty() {
                    self.open_agy_model_picker();
                } else {
                    self.run_agy(self.focus, args);
                }
            }
            "agymodel" => self.open_agy_model_picker(),
            "refresh" => self.request_refresh(),
            "push" => self.start_push(sid, args),
            "theme" => self.open_theme_picker(),
            "help" => {
                self.open_keymap();
                self.dirty = true;
            }
            "quit" => {
                if self.cfg.behavior.confirm_quit && self.is_busy() {
                    self.overlay = Overlay::ConfirmQuit;
                } else {
                    self.should_quit = true;
                }
            }
            other => {
                let known = self
                    .custom_commands
                    .iter()
                    .any(|c| c.name == other);
                if known {
                    let req = {
                        let Some(s) = self.session(sid) else { return };
                        s.oc_sid.clone().map(|oc| (s.dir.clone(), oc))
                    };
                    if let Some((dir, oc)) = req {
                        let args = args.to_string();
                        let cmd = other.to_string();
                        self.manager.run_command(dir, oc, cmd, args);
                        self.flash(format!("running /{other}…"));
                    }
                } else {
                    self.flash(format!("unknown command /{other} — /help for list"));
                }
            }
        }
        self.dirty = true;
    }

    // -- terminal events ---------------------------------------------------------------

    pub async fn handle_term_event(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::Key(k) => self.handle_key(k).await,
            TermEvent::Mouse(m) => self.handle_mouse(m),
            TermEvent::Resize(_, _) => self.dirty = true,
            TermEvent::Paste(text) => {
                // Bracketed paste arrives as one event; keep newlines intact
                // so nothing is submitted mid-paste.
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if let Some(s) = self.focused_mut() {
                    s.input.insert(&text);
                }
                self.dirty = true;
            }
            TermEvent::FocusGained | TermEvent::FocusLost => {}
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.select = None;
                let (stick, sid) = match self.focused() {
                    Some(s) => (s.stick_bottom, s.id),
                    None => (false, 0),
                };
                let bottom = if stick {
                    let total = self.conv_cache.get(&sid).map(|c| c.lines.len()).unwrap_or(0);
                    total.saturating_sub(self.pane_view_height())
                } else {
                    0
                };
                if let Some(s) = self.focused_mut() {
                    if stick {
                        s.scroll = bottom;
                    }
                    s.stick_bottom = false;
                    s.scroll = s.scroll.saturating_sub(4);
                }
                self.dirty = true;
            }
            MouseEventKind::ScrollDown => {
                self.select = None;
                if let Some(s) = self.focused_mut() {
                    s.scroll = s.scroll.saturating_add(4);
                }
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.overlay == Overlay::None && self.viewer.is_none() && self.diff.is_none() {
                    let area = self.last_body_area;
                    let hit = self.layout_rects(area).and_then(|rects| {
                        rects.into_iter().find(|(_, r)| {
                            r.x <= m.column
                                && m.column < r.x + r.width
                                && r.y <= m.row
                                && m.row < r.y + r.height
                        })
                    });
                    self.select = None;
                    if let Some((sid, _)) = hit {
                        if self.focus != sid {
                            self.focus = sid;
                        }
                        // Start a selection only inside the transcript area.
                        if let Some(ca) = self.pane_conv_area(sid) {
                            if m.column >= ca.x
                                && m.column < ca.right()
                                && m.row >= ca.y
                                && m.row < ca.bottom()
                            {
                                let p = SelectPoint { row: m.row, col: m.column };
                                self.select = Some(SelectState {
                                    sid,
                                    anchor: p,
                                    head: p,
                                    dragging: true,
                                });
                            }
                        }
                    }
                }
                self.dirty = true;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(sel) = self.select {
                    if sel.dragging {
                        if let Some(ca) = self.pane_conv_area(sel.sid) {
                            let row = m.row.clamp(ca.y, ca.bottom().saturating_sub(1));
                            let col = m.column.clamp(ca.x, ca.right().saturating_sub(1));
                            if let Some(s) = self.select.as_mut() {
                                s.head = SelectPoint { row, col };
                            }
                            self.dirty = true;
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let mut copy_text: Option<String> = None;
                if let Some(sel) = self.select {
                    if sel.dragging {
                        if let Some(ca) = self.pane_conv_area(sel.sid) {
                            let row = m.row.clamp(ca.y, ca.bottom().saturating_sub(1));
                            let col = m.column.clamp(ca.x, ca.right().saturating_sub(1));
                            if let Some(s) = self.select.as_mut() {
                                s.head = SelectPoint { row, col };
                                s.dragging = false;
                            }
                        }
                        let sel = self.select.unwrap();
                        if sel.anchor == sel.head {
                            // A plain click clears the selection.
                            self.select = None;
                        } else {
                            copy_text = Some(self.selection_text(&sel));
                        }
                    }
                }
                if let Some(text) = copy_text {
                    if !text.is_empty() {
                        self.copy_clipboard(&text);
                    }
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// The transcript (conversation) rectangle of a pane, matching the layout
    /// used by `ui::pane::render`.
    pub fn pane_conv_area(&self, sid: u32) -> Option<Rect> {
        let rects = self.layout_rects(self.last_body_area)?;
        let (_, pr) = *rects.iter().find(|(s, _)| *s == sid)?;
        let sess = self.session(sid)?;
        if pr.width < 8 || pr.height < 3 {
            return None;
        }
        let inner = Rect {
            x: pr.x + 1,
            y: pr.y + 1,
            width: pr.width.saturating_sub(2),
            height: pr.height.saturating_sub(2),
        };
        if inner.height < 3 || inner.width < 6 {
            return None;
        }
        let input_rows =
            crate::ui::pane::input_height(sess, inner.width as usize, inner.height as usize);
        let conv_h = inner.height.saturating_sub(input_rows);
        Some(Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(2),
            height: conv_h,
        })
    }

    /// Plain text covered by a selection, using the same scroll mapping as the
    /// render pass.
    fn selection_text(&self, sel: &SelectState) -> String {
        let Some(sess) = self.session(sel.sid) else {
            return String::new();
        };
        let Some(cache) = self.conv_cache.get(&sel.sid) else {
            return String::new();
        };
        let Some(area) = self.pane_conv_area(sel.sid) else {
            return String::new();
        };
        let h = area.height as usize;
        if h == 0 {
            return String::new();
        }
        let total = cache.lines.len();
        let max_off = total.saturating_sub(h);
        let offset = if sess.stick_bottom { max_off } else { sess.scroll.min(max_off) };
        let to_abs = |p: SelectPoint| -> (usize, usize) {
            let row = (p.row.saturating_sub(area.y) as usize).min(h.saturating_sub(1));
            let col = p.col.saturating_sub(area.x) as usize;
            (offset + row, col)
        };
        let (ar, ac) = to_abs(sel.anchor);
        let (hr, hc) = to_abs(sel.head);
        let ((r0, c0), (r1, c1)) = if (ar, ac) <= (hr, hc) {
            ((ar, ac), (hr, hc))
        } else {
            ((hr, hc), (ar, ac))
        };
        let mut out: Vec<String> = Vec::new();
        for abs in r0..=r1 {
            let Some(line) = cache.lines.get(abs) else { break };
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let chars: Vec<char> = text.chars().collect();
            let start = if abs == r0 { c0 } else { 0 };
            let end = if abs == r1 { c1 } else { chars.len() };
            let s = start.min(chars.len());
            let e = end.min(chars.len()).max(s);
            let seg: String = chars[s..e].iter().collect();
            out.push(seg.trim_end().to_string());
        }
        out.join("\n").trim_end().to_string()
    }

    pub fn layout_rects(&self, body: Rect) -> Option<Vec<(u32, Rect)>> {
        if let Some(s) = self.maximized {
            return Some(vec![(s, body)]);
        }
        self.grid.rects(body)
    }

    pub async fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != crossterm::event::KeyEventKind::Press {
            return;
        }
        // Opt-in key trace (`THETA_KEYLOG=1`) — helps diagnose terminal key
        // encoding oddities (e.g. Shift+Enter) without affecting normal use.
        if std::env::var_os("THETA_KEYLOG").is_some() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/theta-keys.log")
            {
                let _ = writeln!(f, "{:?} {:?} {:?}", key.code, key.modifiers, key.kind);
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if self.viewer.is_some() {
            self.handle_viewer_key(key);
            return;
        }
        if self.diff.is_some() {
            self.handle_diff_key(key);
            return;
        }
        if self.overlay != Overlay::None {
            self.handle_overlay_key(key).await;
            return;
        }

        // Global bindings from the (user-editable) keymap.
        if let Some(action) = self.keys.action_for(&key) {
            let cmd = match action {
                Action::NewSession => Cmd::NewSession,
                Action::Resume => Cmd::ResumeSession,
                Action::Switch => Cmd::SwitchSession,
                Action::Palette => Cmd::Palette,
                Action::Close => Cmd::CloseSession,
                Action::Quit => Cmd::Quit,
                Action::Maximize => Cmd::MaximizeRestore,
                Action::Tiling => Cmd::ChangeTiling,
                Action::Explorer => Cmd::ToggleExplorer,
                Action::Files => Cmd::SearchFiles,
                Action::Project => Cmd::SearchProject,
                Action::Conversation => Cmd::SearchConversation,
                Action::Model => Cmd::ChangeModel,
                Action::Agent => Cmd::SwitchAgent,
                Action::GitDiff => Cmd::GitDiff,
                Action::GitLog => Cmd::GitLog,
                Action::Interrupt => Cmd::Interrupt,
                Action::FocusNext => Cmd::FocusNext,
                Action::FocusPrev => Cmd::FocusPrev,
                Action::Keymap => Cmd::Keymap,
            };
            self.execute(cmd).await;
            return;
        }

        // Alt+arrows: directional focus. Alt+hjkl: resize. Alt+1..9: focus nth.
        if alt {
            match key.code {
                KeyCode::Left => {
                    let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                    self.move_focus(Dir::Left, &rects);
                    return;
                }
                KeyCode::Right => {
                    let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                    self.move_focus(Dir::Right, &rects);
                    return;
                }
                KeyCode::Up => {
                    let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                    self.move_focus(Dir::Up, &rects);
                    return;
                }
                KeyCode::Down => {
                    let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                    self.move_focus(Dir::Down, &rects);
                    return;
                }
                KeyCode::Char(c) => {
                    // Alt+hjkl resizes; Alt+Shift+hjkl moves (Hyprland-style).
                    // Handles both 'h' and 'H' since terminals differ.
                    let shifted = shift || c.is_ascii_uppercase();
                    match c.to_ascii_lowercase() {
                        'h' => {
                            if shifted {
                                let rects =
                                    self.layout_rects(self.last_body_area).unwrap_or_default();
                                self.swap_pane(Dir::Left, &rects);
                            } else {
                                self.resize_pane(Dir::Left);
                            }
                            return;
                        }
                        'l' => {
                            if shifted {
                                let rects =
                                    self.layout_rects(self.last_body_area).unwrap_or_default();
                                self.swap_pane(Dir::Right, &rects);
                            } else {
                                self.resize_pane(Dir::Right);
                            }
                            return;
                        }
                        'k' => {
                            if shifted {
                                let rects =
                                    self.layout_rects(self.last_body_area).unwrap_or_default();
                                self.swap_pane(Dir::Up, &rects);
                            } else {
                                self.resize_pane(Dir::Up);
                            }
                            return;
                        }
                        'j' => {
                            if shifted {
                                let rects =
                                    self.layout_rects(self.last_body_area).unwrap_or_default();
                                self.swap_pane(Dir::Down, &rects);
                            } else {
                                self.resize_pane(Dir::Down);
                            }
                            return;
                        }
                        '1'..='9' => {
                            self.focus_nth(c as usize - '1' as usize);
                            return;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Explorer navigation when open (and input is empty so typing wins).
        if self.explorer.open
            && self.focused().map(|s| s.input.is_empty()).unwrap_or(true)
        {
            if self.explorer_key(key) {
                return;
            }
        }

        // Session-scoped keys.
        let Some(sess) = self.focused() else { return };
        let sid = sess.id;
        let input_empty = sess.input.is_empty();
        let has_perm = sess.pending_perm.is_some();
        let tool_cursor = sess.tool_cursor;
        let busy = sess.status.is_busy();
        let interrupt_armed = sess.interrupt_armed.is_some();
/* borrow ends here */

        // Esc twice interrupts a busy agent. The first press arms it and the
        // footer shows a hint next to the cost readout.
        if key.code == KeyCode::Esc && busy {
            if interrupt_armed {
                self.interrupt(sid);
                if let Some(s) = self.session_mut(sid) {
                    s.interrupt_armed = None;
                    s.dirty = true;
                }
            } else if let Some(s) = self.session_mut(sid) {
                s.interrupt_armed = Some(Instant::now());
                s.dirty = true;
            }
            return;
        }

        if has_perm {
            match key.code {
                KeyCode::Char('a') => {
                    self.permission_reply(sid, "once");
                    return;
                }
                KeyCode::Char('A') => {
                    self.permission_reply(sid, "always");
                    return;
                }
                KeyCode::Char('r') => {
                    self.permission_reply(sid, "reject");
                    return;
                }
                _ => {}
            }
        }

        // Scrolling works regardless of input content.
        match key.code {
            KeyCode::PageUp => {
                let h = self.pane_view_height();
                let was_stick = self.focused().map(|s| s.stick_bottom).unwrap_or(false);
                let total = if was_stick {
                    self.conv_cache.get(&sid).map(|c| c.lines.len()).unwrap_or(0)
                } else {
                    0
                };
                if let Some(s) = self.session_mut(sid) {
                    if was_stick {
                        s.scroll = total.saturating_sub(h);
                    }
                    s.stick_bottom = false;
                    s.scroll = s.scroll.saturating_sub(h.max(1));
                    s.dirty = true;
                }
                self.dirty = true;
                return;
            }
            KeyCode::PageDown => {
                let h = self.pane_view_height();
                if let Some(s) = self.session_mut(sid) {
                    s.scroll = s.scroll.saturating_add(h.max(1));
                    s.dirty = true;
                }
                self.dirty = true;
                return;
            }
            KeyCode::Home => {
                if let Some(s) = self.session_mut(sid) {
                    s.stick_bottom = false;
                    s.scroll = 0;
                    s.dirty = true;
                }
                self.dirty = true;
                return;
            }
            KeyCode::End => {
                if let Some(s) = self.session_mut(sid) {
                    s.stick_bottom = true;
                    s.dirty = true;
                }
                self.dirty = true;
                return;
            }
            _ => {}
        }

        if input_empty && tool_cursor.is_some() {
            // Interactions with the selected tool entry.
            match key.code {
                KeyCode::Char('y') => {
                    let text = self
                        .session(sid)
                        .and_then(|s| s.tool_at(tool_cursor.unwrap()))
                        .map(|t| {
                            t.output
                                .clone()
                                .or_else(|| t.error.clone())
                                .unwrap_or_else(|| t.display_title())
                        });
                    if let Some(text) = text {
                        self.copy_clipboard(&text);
                    }
                    return;
                }
                KeyCode::Enter => {
                    if let Some(c) = tool_cursor {
                        if let Some(s) = self.session_mut(sid) {
                            s.toggle_expanded(c);
                        }
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Char('d') => {
                    self.open_tool_diff(sid).await;
                    return;
                }
                KeyCode::Char('o') => {
                    let target = self.session(sid).and_then(|s| {
                        s.tool_cursor.and_then(|c| s.tool_at(c)).and_then(|t| {
                            t.file_path().map(|f| {
                                if Path::new(&f).is_absolute() {
                                    f
                                } else {
                                    s.dir.join(&f).to_string_lossy().to_string()
                                }
                            })
                        })
                    });
                    if let Some(path) = target {
                        self.open_file_viewer(&path, None);
                    }
                    return;
                }
                _ => {}
            }
        }

        // Slash-command popup navigation while the input starts with '/'.
        let input_text = self
            .focused()
            .map(|s| s.input.text().to_string())
            .unwrap_or_default();
        if input_text.trim_start().starts_with('/') {
            let matches = slash_matches(&input_text, self);
            match key.code {
                KeyCode::Up => {
                    if let Some(s) = self.session_mut(sid) {
                        s.slash_selected = s.slash_selected.saturating_sub(1);
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Down => {
                    if let Some(s) = self.session_mut(sid) {
                        s.slash_selected = (s.slash_selected + 1).min(matches.len().saturating_sub(1));
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Tab => {
                    if let Some(item) = matches.get(
                        self.focused().map(|s| s.slash_selected).unwrap_or(0)
                            .min(matches.len().saturating_sub(1)),
                    ) {
                        if let Some(s) = self.session_mut(sid) {
                            s.input.clear();
                            s.input.insert(&format!("/{} ", item.name));
                        }
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Enter => {
                    let text = self
                        .focused_mut()
                        .map(|s| s.input.take())
                        .unwrap_or_default();
                    // Resolve the highlighted popup item to its full command,
                    // keeping any arguments typed after the first token.
                    let matches = slash_matches(&text, self);
                    if !matches.is_empty() {
                        let sel = self
                            .focused()
                            .map(|s| s.slash_selected)
                            .unwrap_or(0)
                            .min(matches.len() - 1);
                        let item = matches[sel].name.clone();
                        let args = text
                            .split_once(' ')
                            .map(|(_, a)| a.trim().to_string())
                            .unwrap_or_default();
                        let full = if args.is_empty() {
                            format!("/{}", item)
                        } else {
                            format!("/{} {}", item, args)
                        };
                        self.execute_slash(&full);
                    } else {
                        self.execute_slash(&text);
                    }
                    return;
                }
                KeyCode::Char(c) if !ctrl && !alt => {
                    if let Some(s) = self.session_mut(sid) {
                        s.input.insert(&c.to_string());
                        s.slash_selected = 0;
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Backspace => {
                    if let Some(s) = self.session_mut(sid) {
                        s.input.backspace();
                        s.slash_selected = 0;
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Esc => {
                    if let Some(s) = self.session_mut(sid) {
                        s.input.clear();
                    }
                    self.dirty = true;
                    return;
                }
                _ => {}
            }
        }

        if input_empty {
            match key.code {
                // Copy the last reply — Alt+Y so plain `y` can start a message.
                KeyCode::Char('y') if alt => {
                    let text = self.focused().and_then(|s| {
                        s.messages
                            .iter()
                            .rev()
                            .find(|m| m.role == Role::Assistant)
                            .and_then(|m| {
                                m.parts.iter().find_map(|p| match &p.kind {
                                    PartKind::Text { text, synthetic: false } => Some(text.clone()),
                                    _ => None,
                                })
                            })
                    });
                    match text {
                        Some(t) => self.copy_clipboard(&t),
                        None => self.flash("no reply to copy"),
                    }
                    return;
                }
                KeyCode::Up => {
                    // Terminal-like: with nothing selected, Up recalls the
                    // previous prompt from history (even with empty input).
                    if tool_cursor.is_none() {
                        let has_hist = self
                            .focused()
                            .map(|s| !s.input.history.is_empty())
                            .unwrap_or(false);
                        if has_hist {
                            if let Some(s) = self.session_mut(sid) {
                                s.input.hist_up();
                                s.dirty = true;
                            }
                            self.dirty = true;
                            return;
                        }
                    }
                    let tools = self.focused().map(|s| s.tools()).unwrap_or_default();
                    if tools.is_empty() {
                        return;
                    }
                    let next = match tool_cursor.and_then(|c| tools.iter().position(|t| *t == c)) {
                        Some(i) => i.saturating_sub(1),
                        None => tools.len() - 1,
                    };
                    if let Some(s) = self.session_mut(sid) {
                        s.tool_cursor = Some(tools[next]);
                        s.dirty = true;
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Down => {
                    let tools = self.focused().map(|s| s.tools()).unwrap_or_default();
                    if tools.is_empty() {
                        return;
                    }
                    let next = match tool_cursor.and_then(|c| tools.iter().position(|t| *t == c)) {
                        Some(i) => (i + 1).min(tools.len() - 1),
                        None => 0,
                    };
                    if let Some(s) = self.session_mut(sid) {
                        s.tool_cursor = Some(tools[next]);
                        s.dirty = true;
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Esc => {
                    if let Some(s) = self.session_mut(sid) {
                        s.tool_cursor = None;
                        s.dirty = true;
                    }
                    self.dirty = true;
                    return;
                }
                _ => {}
            }
        }

        self.type_into_input(key).await;
    }

    pub fn open_new_dialog(&mut self) {
        let dir = self
            .focused()
            .map(|s| s.dir.clone())
            .unwrap_or_else(|| self.initial_dir.clone());
        self.newdlg = NewSessionState {
            field: 0,
            name: InputState::default(),
            dir: dir.to_string_lossy().to_string(),
            recent: Vec::new(),
            recent_selected: 0,
            recent_req: None,
            recent_loaded: false,
        };
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        self.remember_dir(&canon);
        // Recent sessions across every project, not just this directory.
        self.newdlg.recent = self.all_cached_sessions();
        self.newdlg.recent_loaded = !self.newdlg.recent.is_empty();
        let req = self.manager.next_req();
        self.newdlg.recent_req = Some(req);
        self.manager.list_server_sessions(req, dir.clone());
        self.overlay = Overlay::NewSession;
        self.dirty = true;
    }

    pub fn open_resume_picker(&mut self) {
        let dir = self
            .focused()
            .map(|s| s.dir.clone())
            .unwrap_or_else(|| self.initial_dir.clone());
        self.remember_dir(&dir);
        self.resume_picker = ResumePickerState::default();
        // Show every known session first; the per-directory fetch below fills
        // in anything not cached yet.
        self.resume_picker.items = self.all_cached_sessions();
        self.resume_picker.loaded = !self.resume_picker.items.is_empty();
        let req = self.manager.next_req();
        self.resume_picker.req = Some(req);
        self.manager.list_server_sessions(req, dir.clone());
        self.overlay = Overlay::ResumeSession;
        self.dirty = true;
    }

    /// Attach a pane to a previous server session (resume).
    pub fn resume_session(&mut self, name: &str, dir: PathBuf, oc_sid: String) {
        let id = self.next_session;
        self.next_session += 1;
        let dir = dir.canonicalize().unwrap_or(dir);
        self.remember_dir(&dir);
        self.preload_sessions(dir.clone());
        let mut sess = SessionState::new(id, name.to_string(), dir.clone());
        sess.oc_sid = Some(oc_sid.clone());
        sess.status = SessStatus::Connecting;
        self.sessions.push(sess);
        let area = self.pane_area();
        self.grid.insert_session(id, area.width, area.height);
        self.focus = id;
        self.maximized = None;
        let req = self.manager.next_req();
        self.pending_create.insert(id, req);
        self.manager.connect_session(
            req,
            dir,
            name.to_string(),
            Some(oc_sid),
            None,
            self.cfg.behavior.history_limit,
        );
        self.dirty = true;
        self.save_workspace();
    }

    pub fn begin_project_search(&mut self) {
        self.proj_search.input.clear();
        self.proj_search.results.clear();
        self.proj_search.selected = 0;
        self.overlay = Overlay::ProjectSearch;
        self.dirty = true;
    }

    async fn type_into_input(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Newline insertions. Some terminals report Shift+Enter both as an
        // `Enter`+SHIFT event and a raw line feed, so de-duplicate identical
        // newlines fired within the same keypress.
        let newline = (key.code == KeyCode::Enter && (shift || alt))
            || (key.code == KeyCode::Char('j') && ctrl)
            || matches!(key.code, KeyCode::Char('\n') | KeyCode::Char('\r'));
        if newline {
            let now = Instant::now();
            let dup = self
                .last_newline
                .map(|t| now.duration_since(t) < Duration::from_millis(150))
                .unwrap_or(false);
            self.last_newline = Some(now);
            if !dup {
                if let Some(s) = self.focused_mut() {
                    s.input.insert("\n");
                    s.dirty = true;
                }
                self.dirty = true;
            }
            return;
        }

        let Some(s) = self.focused_mut() else { return };
        let multiline = s.input.buf.contains('\n');
        match key.code {
            KeyCode::Enter => {
                let sid = s.id;
                // borrow ends here; NLL handles the rest
                self.submit_input(sid);
                self.dirty = true;
                return;
            }
            KeyCode::Backspace => s.input.backspace(),
            KeyCode::Delete => s.input.delete(),
            KeyCode::Left => s.input.left(),
            KeyCode::Right => s.input.right(),
            KeyCode::Home => s.input.home(),
            KeyCode::End => s.input.end(),
            KeyCode::Up => {
                if multiline {
                    s.input.home();
                } else {
                    s.input.hist_up();
                }
            }
            KeyCode::Down => {
                if multiline {
                    s.input.end();
                } else {
                    s.input.hist_down();
                }
            }
            KeyCode::Char('u') if ctrl => s.input.clear(),
            KeyCode::Esc => s.input.clear(),
            KeyCode::Char(c) if !ctrl && !alt && c != '\n' && c != '\r' && c != '\t' => {
                s.input.insert(&c.to_string())
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn handle_viewer_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') => {
                let text = self
                    .viewer
                    .as_ref()
                    .map(|v| {
                        v.lines
                            .iter()
                            .map(|l| {
                                l.spans
                                    .iter()
                                    .map(|s| s.content.clone())
                                    .collect::<String>()
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                self.copy_clipboard(&text);
            }
            KeyCode::Esc | KeyCode::Char('q') => self.viewer = None,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(v) = &mut self.viewer {
                    v.scroll = v.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(v) = &mut self.viewer {
                    v.scroll = v.scroll.saturating_add(1);
                }
            }
            KeyCode::PageUp => {
                if let Some(v) = &mut self.viewer {
                    v.scroll = v.scroll.saturating_sub(30);
                }
            }
            KeyCode::PageDown => {
                if let Some(v) = &mut self.viewer {
                    v.scroll = v.scroll.saturating_add(30);
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                if let Some(v) = &mut self.viewer {
                    v.scroll = 0;
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn handle_diff_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.diff = None,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(d) = &mut self.diff {
                    d.scroll = d.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(d) = &mut self.diff {
                    d.scroll = d.scroll.saturating_add(1);
                }
            }
            KeyCode::PageUp => {
                if let Some(d) = &mut self.diff {
                    d.scroll = d.scroll.saturating_sub(30);
                }
            }
            KeyCode::PageDown => {
                if let Some(d) = &mut self.diff {
                    d.scroll = d.scroll.saturating_add(30);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    async fn handle_overlay_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.overlay {
            Overlay::Palette => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = filtered_commands(self.palette.input.text()).len();
                    if n > 0 {
                        self.palette.selected = if self.palette.selected == 0 {
                            n - 1
                        } else {
                            self.palette.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = filtered_commands(self.palette.input.text()).len();
                    if n > 0 {
                        self.palette.selected = (self.palette.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    let cmds = filtered_commands(self.palette.input.text());
                    if !cmds.is_empty() {
                        let sel = self.palette.selected.min(cmds.len() - 1);
                        let cmd = cmds[sel].cmd;
                        self.overlay = Overlay::None;
                        self.execute(cmd).await;
                    }
                }
                KeyCode::Backspace => self.palette.input.backspace(),
                KeyCode::Char(c) if !ctrl => self.palette.input.insert(&c.to_string()),
                _ => {}
            },
            Overlay::NewSession => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Tab => self.newdlg.field = (self.newdlg.field + 1) % 3,
                KeyCode::Up => {
                    if self.newdlg.field == 2 {
                        let n = self.newdlg.recent.len();
                        if n > 0 {
                            self.newdlg.recent_selected = if self.newdlg.recent_selected == 0 {
                                n - 1
                            } else {
                                self.newdlg.recent_selected - 1
                            };
                        }
                    }
                }
                KeyCode::Down => {
                    if self.newdlg.field == 2 {
                        let n = self.newdlg.recent.len();
                        if n > 0 {
                            self.newdlg.recent_selected = (self.newdlg.recent_selected + 1) % n;
                        }
                    }
                }
                KeyCode::Enter => match self.newdlg.field {
                    2 => {
                        let pick = self.newdlg.recent.get(self.newdlg.recent_selected).map(|s| {
                            (s.title.clone(), s.id.clone(), s.directory.clone())
                        });
                        if let Some((title, oc_sid, dir)) = pick {
                            // Resume in the session's OWN folder so the agent
                            // has access to its project.
                            let dir = if dir.trim().is_empty() {
                                PathBuf::from(self.newdlg.dir.trim())
                            } else {
                                PathBuf::from(dir)
                            };
                            self.overlay = Overlay::None;
                            self.resume_session(&title, dir, oc_sid);
                        }
                    }
                    1 => {
                        // Directory is the last text field: Enter creates.
                        let name = self.newdlg.name.take();
                        let dir = PathBuf::from(self.newdlg.dir.trim());
                        self.overlay = Overlay::None;
                        self.create_session(&name, dir, None);
                    }
                    _ => self.newdlg.field += 1,
                },
                KeyCode::Backspace => match self.newdlg.field {
                    0 => self.newdlg.name.backspace(),
                    1 => {
                        let _ = self.newdlg.dir.pop();
                    }
                    _ => {}
                },
                KeyCode::Char(c) if !ctrl => match self.newdlg.field {
                    0 => self.newdlg.name.insert(&c.to_string()),
                    1 => self.newdlg.dir.push(c),
                    _ => {}
                },
                _ => {}
            },
            Overlay::Rename => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Enter => {
                    let name = self.rename.input.take();
                    let id = self.focus;
                    self.overlay = Overlay::None;
                    if !name.is_empty() {
                        self.rename_session(id, &name);
                    }
                }
                KeyCode::Backspace => self.rename.input.backspace(),
                KeyCode::Char(c) if !ctrl => self.rename.input.insert(&c.to_string()),
                _ => {}
            },
            Overlay::ConfirmQuit => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.should_quit = true,
                _ => self.overlay = Overlay::None,
            },
            Overlay::BusyChoice => {
                let id = self.focus;
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::Left | KeyCode::Char('h') => {
                        self.busy_choice = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Right | KeyCode::Char('l') => {
                        self.busy_choice = 1;
                    }
                    KeyCode::Tab => {
                        self.busy_choice = (self.busy_choice + 1) % 2;
                    }
                    KeyCode::Char('q') if !ctrl => self.confirm_busy_choice(id, 0),
                    KeyCode::Char('f') | KeyCode::Char('n') if !ctrl => {
                        self.confirm_busy_choice(id, 1)
                    }
                    KeyCode::Enter => self.confirm_busy_choice(id, self.busy_choice.min(1)),
                    KeyCode::Esc => {
                        // Abandon the send but keep the text in the input box.
                        if let Some(s) = self.session_mut(id) {
                            if let Some(text) = s.pending_send.take() {
                                s.input.buf = text;
                                s.input.cursor = s.input.buf.chars().count();
                            }
                            s.dirty = true;
                        }
                        self.overlay = Overlay::None;
                    }
                    _ => {}
                }
            }
            Overlay::FileSearch => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = self.file_search.results.len();
                    if n > 0 {
                        self.file_search.selected = if self.file_search.selected == 0 {
                            n - 1
                        } else {
                            self.file_search.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = self.file_search.results.len();
                    if n > 0 {
                        self.file_search.selected = (self.file_search.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    if let Some(path) =
                        self.file_search.results.get(self.file_search.selected).cloned()
                    {
                        self.overlay = Overlay::None;
                        self.open_file_viewer(&path, None);
                    }
                }
                KeyCode::Backspace => {
                    self.file_search.input.backspace();
                    self.run_file_search();
                }
                KeyCode::Char(c) if !ctrl => {
                    self.file_search.input.insert(&c.to_string());
                    self.run_file_search();
                }
                _ => {}
            },
            Overlay::ProjectSearch => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = self.proj_search.results.len();
                    if n > 0 {
                        self.proj_search.selected = if self.proj_search.selected == 0 {
                            n - 1
                        } else {
                            self.proj_search.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = self.proj_search.results.len();
                    if n > 0 {
                        self.proj_search.selected = (self.proj_search.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    if let Some(m) =
                        self.proj_search.results.get(self.proj_search.selected).cloned()
                    {
                        self.overlay = Overlay::None;
                        self.open_file_viewer(&m.path, Some(m.line));
                    }
                }
                KeyCode::Backspace => {
                    self.proj_search.input.backspace();
                    self.run_project_search();
                }
                KeyCode::Char(c) if !ctrl => {
                    self.proj_search.input.insert(&c.to_string());
                    self.run_project_search();
                }
                _ => {}
            },
            Overlay::ConvSearch => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = self.conv_search.matches.len();
                    if n > 0 {
                        self.conv_search.selected = if self.conv_search.selected == 0 {
                            n - 1
                        } else {
                            self.conv_search.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = self.conv_search.matches.len();
                    if n > 0 {
                        self.conv_search.selected = (self.conv_search.selected + 1) % n;
                    }
                }
                KeyCode::Enter => self.jump_to_conv_match(),
                KeyCode::Backspace => {
                    self.conv_search.input.backspace();
                    self.update_conv_matches();
                }
                KeyCode::Char(c) if !ctrl => {
                    self.conv_search.input.insert(&c.to_string());
                    self.update_conv_matches();
                }
                _ => {}
            },
            Overlay::AgyModel => {
                let n = self.agy_models.len();
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.agy_ui.selected = self.agy_ui.selected.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if n > 0 {
                            self.agy_ui.selected = (self.agy_ui.selected + 1) % n;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some((name, _)) = self.agy_models.get(self.agy_ui.selected) {
                            self.cfg.agy_model = name.clone();
                            let _ = self.cfg.save();
                            self.overlay = Overlay::None;
                            self.flash(format!("agy model: {name}"));
                        }
                    }
                    _ => {}
                }
            }
            Overlay::Question => {
                let sid = self.focus;
                let info = self
                    .session(sid)
                    .and_then(|s| s.pending_question.as_ref())
                    .and_then(|pq| {
                        pq.current()
                            .map(|q| (q.multiple, q.custom, q.options.len()))
                    });
                let Some((multiple, custom, n_opts)) = info else {
                    self.overlay = Overlay::None;
                    return;
                };
                match key.code {
                    KeyCode::Esc => self.reject_question(sid),
                    KeyCode::Up | KeyCode::Char('k') if !custom => {
                        if let Some(s) = self.session_mut(sid) {
                            if let Some(pq) = s.pending_question.as_mut() {
                                let cur = pq.selected.get(pq.qi).copied().unwrap_or(0);
                                let next = if cur == 0 {
                                    n_opts.saturating_sub(1)
                                } else {
                                    cur - 1
                                };
                                if let Some(v) = pq.selected.get_mut(pq.qi) {
                                    *v = next;
                                }
                            }
                        }
                        self.dirty = true;
                    }
                    KeyCode::Down | KeyCode::Char('j') if !custom => {
                        if let Some(s) = self.session_mut(sid) {
                            if let Some(pq) = s.pending_question.as_mut() {
                                let cur = pq.selected.get(pq.qi).copied().unwrap_or(0);
                                let next = if n_opts == 0 { 0 } else { (cur + 1) % n_opts };
                                if let Some(v) = pq.selected.get_mut(pq.qi) {
                                    *v = next;
                                }
                            }
                        }
                        self.dirty = true;
                    }
                    KeyCode::Tab => {
                        if let Some(s) = self.session_mut(sid) {
                            if let Some(pq) = s.pending_question.as_mut() {
                                let cur = pq.selected.get(pq.qi).copied().unwrap_or(0);
                                let next = if n_opts == 0 { 0 } else { (cur + 1) % n_opts };
                                if let Some(v) = pq.selected.get_mut(pq.qi) {
                                    *v = next;
                                }
                            }
                        }
                        self.dirty = true;
                    }
                    KeyCode::Char(' ') if multiple && !custom => {
                        if let Some(s) = self.session_mut(sid) {
                            if let Some(pq) = s.pending_question.as_mut() {
                                let qi = pq.qi;
                                let sel = pq.selected.get(qi).copied().unwrap_or(0);
                                if let Some(ch) =
                                    pq.chosen.get_mut(qi).and_then(|v| v.get_mut(sel))
                                {
                                    *ch = !*ch;
                                }
                            }
                        }
                        self.dirty = true;
                    }
                    KeyCode::Backspace if custom => {
                        if let Some(s) = self.session_mut(sid) {
                            if let Some(pq) = s.pending_question.as_mut() {
                                pq.custom.pop();
                            }
                        }
                        self.dirty = true;
                    }
                    KeyCode::Char(c) if custom && !ctrl => {
                        if let Some(s) = self.session_mut(sid) {
                            if let Some(pq) = s.pending_question.as_mut() {
                                pq.custom.push(c);
                            }
                        }
                        self.dirty = true;
                    }
                    KeyCode::Enter => {
                        let done = self
                            .session_mut(sid)
                            .and_then(|s| s.pending_question.as_mut())
                            .map(|pq| pq.commit_current())
                            .unwrap_or(true);
                        if done {
                            self.answer_question(sid);
                        } else {
                            self.dirty = true;
                        }
                    }
                    _ => {}
                }
            }
            Overlay::Theme => {
                let names = crate::theme::theme_names();
                match key.code {
                    KeyCode::Esc => {
                        // Cancel: restore the theme we opened with.
                        let original = self.theme_ui.original.clone();
                        crate::theme::set_theme(&original);
                        self.cfg.theme = original.clone();
                        self.conv_cache.clear();
                        self.overlay = Overlay::None;
                        self.flash(format!("theme: {}", crate::theme::theme_label(&original)));
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let n = names.len();
                        if n > 0 {
                            self.theme_ui.selected = if self.theme_ui.selected == 0 {
                                n - 1
                            } else {
                                self.theme_ui.selected - 1
                            };
                            self.preview_theme(&names);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let n = names.len();
                        if n > 0 {
                            self.theme_ui.selected = (self.theme_ui.selected + 1) % n;
                            self.preview_theme(&names);
                        }
                    }
                    KeyCode::Enter => {
                        let name = names
                            .get(self.theme_ui.selected)
                            .copied()
                            .unwrap_or("theta-night");
                        crate::theme::set_theme(name);
                        self.cfg.theme = name.to_string();
                        let _ = self.cfg.save();
                        self.conv_cache.clear();
                        self.overlay = Overlay::None;
                        self.flash(format!(
                            "theme: {} (saved)",
                            crate::theme::theme_label(name)
                        ));
                    }
                    _ => {}
                }
            }
            Overlay::ModelPicker => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = self.picker_models().len();
                    if n > 0 {
                        self.model_picker.selected = if self.model_picker.selected == 0 {
                            n - 1
                        } else {
                            self.model_picker.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = self.picker_models().len();
                    if n > 0 {
                        self.model_picker.selected = (self.model_picker.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    // Index into the FILTERED list — the one on screen.
                    let entries =
                        self.filtered_picker_models(self.model_picker.input.text());
                    if !entries.is_empty() {
                        let sel = self.model_picker.selected.min(entries.len() - 1);
                        let (label, model) = entries[sel].clone();
                        let sid = self.focus;
                        self.set_model(sid, model, &label);
                    }
                }
                KeyCode::Backspace => self.model_picker.input.backspace(),
                KeyCode::Char(c) if !ctrl => {
                    self.model_picker.input.insert(&c.to_string());
                    self.model_picker.selected = 0;
                }
                _ => {}
            },
            Overlay::AgentPicker => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = self.picker_agents().len();
                    if n > 0 {
                        self.agent_picker.selected = if self.agent_picker.selected == 0 {
                            n - 1
                        } else {
                            self.agent_picker.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = self.picker_agents().len();
                    if n > 0 {
                        self.agent_picker.selected = (self.agent_picker.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    let entries =
                        self.filtered_picker_agents(self.agent_picker.input.text());
                    if !entries.is_empty() {
                        let sel = self.agent_picker.selected.min(entries.len() - 1);
                        let (name, _) = entries[sel].clone();
                        let sid = self.focus;
                        let agent = if name == "default" { None } else { Some(name.clone()) };
                        self.set_agent(sid, agent, &name);
                    }
                }
                KeyCode::Backspace => self.agent_picker.input.backspace(),
                KeyCode::Char(c) if !ctrl => {
                    self.agent_picker.input.insert(&c.to_string());
                    self.agent_picker.selected = 0;
                }
                _ => {}
            },
            Overlay::LayoutPicker => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up | KeyCode::Char('k') => {
                    let n = crate::panes::Scheme::all().len();
                    self.layout_picker.selected = if self.layout_picker.selected == 0 {
                        n - 1
                    } else {
                        self.layout_picker.selected - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = crate::panes::Scheme::all().len();
                    self.layout_picker.selected = (self.layout_picker.selected + 1) % n;
                }
                KeyCode::Enter => {
                    let schemes = crate::panes::Scheme::all();
                    let scheme = schemes[self.layout_picker.selected.min(schemes.len() - 1)];
                    let (w, h) = (self.last_body_area.width, self.last_body_area.height);
                    self.grid.apply_scheme(scheme, w.max(40), h.max(16));
                    self.overlay = Overlay::None;
                    self.flash(format!("tiling: {}", scheme.label()));
                    self.save_workspace();
                }
                _ => {}
            },
            Overlay::ResumeSession => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up | KeyCode::Char('k') => {
                    let n = self.resume_picker.items.len();
                    if n > 0 {
                        self.resume_picker.selected = if self.resume_picker.selected == 0 {
                            n - 1
                        } else {
                            self.resume_picker.selected - 1
                        };
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = self.resume_picker.items.len();
                    if n > 0 {
                        self.resume_picker.selected = (self.resume_picker.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    let pick = self
                        .resume_picker
                        .items
                        .get(self.resume_picker.selected)
                        .map(|s| (s.title.clone(), s.id.clone(), s.directory.clone()));
                    if let Some((title, oc_sid, session_dir)) = pick {
                        // Resume in the session's OWN folder so cross-project
                        // sessions open in the right workspace.
                        let dir = if session_dir.trim().is_empty() {
                            self.focused()
                                .map(|s| s.dir.clone())
                                .unwrap_or_else(|| self.initial_dir.clone())
                        } else {
                            PathBuf::from(session_dir)
                        };
                        self.overlay = Overlay::None;
                        self.resume_session(&title, dir, oc_sid);
                    }
                }
                _ => {}
            },
            Overlay::SessionList => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up => {
                    let n = self.sessions.len();
                    if n > 0 {
                        self.session_list.selected = if self.session_list.selected == 0 {
                            n - 1
                        } else {
                            self.session_list.selected - 1
                        };
                    }
                }
                KeyCode::Down => {
                    let n = self.sessions.len();
                    if n > 0 {
                        self.session_list.selected = (self.session_list.selected + 1) % n;
                    }
                }
                KeyCode::Enter => {
                    let order = self.grid.order();
                    if let Some(target) = order.get(self.session_list.selected) {
                        self.focus = *target;
                    }
                    self.overlay = Overlay::None;
                }
                _ => {}
            },
            Overlay::Keymap => {
                use crate::keys::{Action, KeySpec};
                // Capture mode: the next key press becomes the binding.
                if let Some(action) = self.keymap_ui.capturing {
                    match key.code {
                        KeyCode::Esc => self.keymap_ui.capturing = None,
                        _ => {
                            if let Some(spec_str) = KeySpec::spec_of(&key) {
                                if let Some(spec) = KeySpec::parse(&spec_str) {
                                    self.keys.set(action, spec);
                                    self.cfg.keys.insert(action.name().to_string(), spec_str);
                                    let _ = self.cfg.save();
                                    let shown = self.keys.binding_str(action);
                                    self.keymap_ui.capturing = None;
                                    self.flash(format!(
                                        "{} → {} (saved)",
                                        action.label(),
                                        shown
                                    ));
                                }
                            }
                        }
                    }
                    self.dirty = true;
                    return;
                }
                let actions = Action::ALL;
                let sel = self.keymap_ui.selected.min(actions.len() - 1);
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.keymap_ui.selected = self.keymap_ui.selected.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.keymap_ui.selected = (self.keymap_ui.selected + 1).min(actions.len() - 1);
                    }
                    KeyCode::Enter => {
                        self.keymap_ui.capturing = Some(actions[sel]);
                    }
                    KeyCode::Char('r') => {
                        let action = actions[sel];
                        self.keys.reset(action);
                        self.cfg.keys.remove(action.name());
                        let _ = self.cfg.save();
                        self.flash(format!("{} reset to default", action.label()));
                    }
                    _ => {}
                }
            }
            Overlay::None => {}
        }
        self.dirty = true;
    }

    fn run_file_search(&mut self) {
        let Some(dir) = self.focused().map(|s| s.dir.clone()) else { return };
        let q = self.file_search.input.text().trim().to_string();
        if q.is_empty() {
            self.file_search.results.clear();
            return;
        }
        let req = self.manager.next_req();
        self.file_search.req = Some(req);
        self.manager.search_files(req, dir, q);
    }

    fn run_project_search(&mut self) {
        let Some(dir) = self.focused().map(|s| s.dir.clone()) else { return };
        let q = self.proj_search.input.text().trim().to_string();
        if q.is_empty() {
            self.proj_search.results.clear();
            return;
        }
        let req = self.manager.next_req();
        self.proj_search.req = Some(req);
        self.manager.search_pattern(req, dir, q);
    }

    pub fn filtered_picker_models(&self, query: &str) -> Vec<(String, Option<ModelRef>)> {
        let q = query.trim().to_lowercase();
        self.picker_models()
            .into_iter()
            .filter(|(label, _)| q.is_empty() || label.to_lowercase().contains(&q))
            .collect()
    }

    pub fn filtered_picker_agents(&self, query: &str) -> Vec<(String, String)> {
        let q = query.trim().to_lowercase();
        self.picker_agents()
            .into_iter()
            .filter(|(name, desc)| {
                q.is_empty() || name.to_lowercase().contains(&q) || desc.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn model_choices(&self) -> Vec<String> {
        let mut v = vec!["default".to_string()];
        v.extend(self.providers.iter().map(|p| p.label.clone()));
        v
    }

    pub fn model_choice_at(&self, idx: usize) -> Option<ModelRef> {
        if idx == 0 {
            return self.default_model.clone();
        }
        self.providers.get(idx - 1).map(|p| ModelRef {
            provider_id: p.provider_id.clone(),
            model_id: p.model_id.clone(),
        })
    }

    pub async fn execute(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::NewSession => self.open_new_dialog(),
            Cmd::CloseSession => self.close_session(self.focus),
            Cmd::RenameSession => {
                self.rename.input.clear();
                let snapshot = self.focused().map(|s| (s.name.clone(), s.name.chars().count()));
                if let Some((name, len)) = snapshot {
                    self.rename.input.buf = name;
                    self.rename.input.cursor = len;
                }
                self.overlay = Overlay::Rename;
            }
            Cmd::MaximizeRestore => self.toggle_maximize(),
            Cmd::Retile => self.retile(),
            Cmd::FocusNext => self.focus_next(),
            Cmd::FocusPrev => self.focus_prev(),
            Cmd::Interrupt => self.interrupt(self.focus),
            Cmd::RestartSession => self.restart_session(self.focus),
            Cmd::ReconnectAll => self.reconnect_all(),
            Cmd::SearchFiles => {
                self.file_search.input.clear();
                self.file_search.results.clear();
                self.file_search.selected = 0;
                self.overlay = Overlay::FileSearch;
            }
            Cmd::SearchProject => self.begin_project_search(),
            Cmd::SearchConversation => self.open_conv_search(),
            Cmd::ToggleExplorer => self.toggle_explorer(),
            Cmd::GitDiff => self.open_workspace_diff().await,
            Cmd::GitLog => self.open_git_log().await,
            Cmd::ResumeSession => self.open_resume_picker(),
            Cmd::Keymap => self.open_keymap(),
            Cmd::AgyModel => self.open_agy_model_picker(),
            Cmd::Refresh => self.request_refresh(),
            Cmd::Theme => self.open_theme_picker(),
            // Ctrl+O switches directly to the next session — no picker.
            Cmd::SwitchSession => self.focus_next(),
            Cmd::ChangeTiling => {
                self.layout_picker.selected = 0;
                self.overlay = Overlay::LayoutPicker;
            }
            Cmd::ChangeModel => self.open_model_picker(),
            Cmd::SwitchAgent => self.open_agent_picker(),
            Cmd::Help => self.open_keymap(),
            Cmd::Palette => {
                self.palette.input.clear();
                self.palette.selected = 0;
                self.overlay = Overlay::Palette;
            }
            Cmd::Quit => {
                if self.cfg.behavior.confirm_quit && self.is_busy() {
                    self.overlay = Overlay::ConfirmQuit;
                } else {
                    self.should_quit = true;
                }
            }
            Cmd::SwapLeft => {
                let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                self.swap_pane(Dir::Left, &rects);
            }
            Cmd::SwapRight => {
                let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                self.swap_pane(Dir::Right, &rects);
            }
            Cmd::SwapUp => {
                let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                self.swap_pane(Dir::Up, &rects);
            }
            Cmd::SwapDown => {
                let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                self.swap_pane(Dir::Down, &rects);
            }
        }
        self.dirty = true;
    }

    // -- AppEvent handling -------------------------------------------------------------

    pub async fn handle_event(&mut self, ev: AppEvent) {
        self.dirty = true;
        match ev {
            AppEvent::ServerReady { .. } => {}
            AppEvent::ServerFailed { dir, error } => {
                for s in self.sessions.iter_mut() {
                    if s.dir == dir && s.status == SessStatus::Connecting {
                        s.status = SessStatus::Error(error.clone());
                        s.last_error = Some(error.clone());
                    }
                }
                self.flash(error);
            }
            AppEvent::ServerDied { dir } => {
                for s in self.sessions.iter_mut() {
                    if s.dir == dir && s.oc_sid.is_some() {
                        s.status = SessStatus::Error("opencode server exited".into());
                    }
                }
            }
            AppEvent::OcCreated { req, session } => {
                let target = self
                    .pending_create
                    .iter()
                    .find(|(_, r)| **r == req)
                    .map(|(sid, _)| *sid);
                self.pending_create.retain(|_, r| *r != req);
                if let Some(sid) = target {
                    // Adopt the directory OpenCode reports for this session so
                    // the folder info is always authoritative.
                    let server_dir = PathBuf::from(&session.directory);
                    if let Some(s) = self.session_mut(sid) {
                        s.oc_sid = Some(session.id.clone());
                        if !session.directory.is_empty() && server_dir != s.dir {
                            if let Ok(canon) = server_dir.canonicalize() {
                                s.dir = canon;
                            }
                        }
                        s.status = SessStatus::Idle;
                        s.dirty = true;
                    }
                    // A forked pane just connected — send its prompt now.
                    if let Some(pos) = self.pending_fork.iter().position(|(fid, _)| *fid == sid) {
                        let (_, text) = self.pending_fork.remove(pos);
                        self.send_text_now(sid, &text);
                    }
                }
            }
            AppEvent::OcCreateFailed { req, error } => {
                let target = self
                    .pending_create
                    .iter()
                    .find(|(_, r)| **r == req)
                    .map(|(sid, _)| *sid);
                self.pending_create.retain(|_, r| *r != req);
                if let Some(sid) = target {
                    if let Some(s) = self.session_mut(sid) {
                        s.status = SessStatus::Error(error.clone());
                    }
                }
                self.flash(error);
            }
            AppEvent::OcEvent { dir, ev } => self.route_oc_event(dir, ev),
            AppEvent::HistoryLoaded { dir, oc_sid, msgs } => {
                if let Some(s) = self
                    .sessions
                    .iter_mut()
                    .find(|s| s.dir == dir && s.oc_sid.as_deref() == Some(oc_sid.as_str()))
                {
                    s.replace_history(msgs);
                    if s.status == SessStatus::Connecting {
                        s.status = SessStatus::Idle;
                    }
                    s.dirty = true;
                }
            }
            AppEvent::HistoryFailed { error, .. } => {
                self.flash(format!("history failed: {error}"));
            }
            AppEvent::SendFailed { dir, oc_sid, error } => {
                if let Some(s) = self
                    .sessions
                    .iter_mut()
                    .find(|s| s.dir == dir && s.oc_sid.as_deref() == Some(oc_sid.as_str()))
                {
                    s.last_error = Some(error.clone());
                    s.status = SessStatus::Error(error.clone());
                }
                self.flash(error);
            }
            AppEvent::Aborted { .. } => {}
            AppEvent::ProvidersListed {
                providers, default, ..
            } => {
                self.providers = providers;
                self.default_model = default;
            }
            AppEvent::FilesFound { req, paths } => {
                if self.file_search.req == Some(req) {
                    self.file_search.results = paths;
                    self.file_search.selected = 0;
                }
            }
            AppEvent::MatchesFound { req, matches } => {
                if self.proj_search.req == Some(req) {
                    self.proj_search.results = matches;
                    self.proj_search.selected = 0;
                }
            }
            AppEvent::SearchFailed { req, error } => {
                if self.proj_search.req == Some(req) {
                    self.flash(format!("search: {error}"));
                }
            }
            AppEvent::AgyModels { models } => {
                let n = models.len();
                self.agy_models = models;
                self.flash(format!("{n} agy models loaded"));
            }
            AppEvent::PushProgress { session, text } => {
                if let Some(s) = self.session_mut(session) {
                    s.activity = Some((text, Instant::now()));
                    s.dirty = true;
                }
            }
            AppEvent::PushDone {
                session,
                ok,
                message,
                repo,
            } => {
                let text = if ok {
                    format!("pushed to {}", repo.unwrap_or_else(|| "remote".into()))
                } else {
                    format!("push failed: {message}")
                };
                if let Some(s) = self.session_mut(session) {
                    s.activity = Some((text.clone(), Instant::now()));
                    s.dirty = true;
                }
                self.flash(text);
            }
            AppEvent::QuestionsListed { dir, questions } => {
                for q in questions {
                    if let Some(idx) = self.sessions.iter().position(|s| {
                        s.dir == dir && s.oc_sid.as_deref() == Some(q.session_id.as_str())
                    }) {
                        if self.sessions[idx].pending_question.is_none() {
                            self.sessions[idx].pending_question =
                                Some(PendingQuestion::new(q.id, q.questions));
                            self.sessions[idx].status = SessStatus::Question;
                            self.sessions[idx].dirty = true;
                        }
                    }
                }
                self.open_pending_question();
            }
            AppEvent::PermissionsListed { dir, permissions } => {
                for p in permissions {
                    if let Some(idx) = self.sessions.iter().position(|s| {
                        s.dir == dir && s.oc_sid.as_deref() == Some(p.session_id.as_str())
                    }) {
                        let auto = self.cfg.behavior.auto_approve_permissions;
                        if auto {
                            if let Some(oc) = self.sessions[idx].oc_sid.clone() {
                                self.manager.reply_permission(
                                    dir.clone(),
                                    oc,
                                    p.id.clone(),
                                    "once".into(),
                                );
                            }
                        } else if self.sessions[idx].pending_perm.is_none() {
                            let detail = p.detail();
                            self.sessions[idx].pending_perm = Some(PendingPermission {
                                id: p.id,
                                kind: p.permission,
                                detail,
                            });
                            self.sessions[idx].status = SessStatus::Permission;
                            self.sessions[idx].dirty = true;
                        }
                    }
                }
            }
            AppEvent::AgyDone {
                session: id,
                ok,
                output,
                model,
            } => {
                let mut text = if output.is_empty() {
                    "(no output)".to_string()
                } else {
                    output
                };
                let header = format!("▌ agy · {}\n\n", model);
                if let Some(s) = self.session_mut(id) {
                    let msg = crate::opencode::Message {
                        id: format!("agy-{}", s.optimistic_seq()),
                        role: crate::opencode::Role::Assistant,
                        error: if ok { None } else { Some("agy failed".into()) },
                        completed: Some(1),
                        created: None,
                        cost: None,
                        tokens: None,
                        parts: vec![crate::opencode::Part {
                            id: format!("agy-{}-out", s.optimistic_seq()),
                            message_id: format!("agy-{}", s.optimistic_seq()),
                            kind: crate::opencode::PartKind::Text {
                                text: format!("{header}{}", if ok { text.clone() } else { String::new() }),
                                synthetic: false,
                            },
                        }],
                    };
                    if !ok {
                        // show stderr/output in the error block
                        s.last_error = Some(if text.is_empty() {
                            "agy failed".into()
                        } else {
                            format!("agy failed: {text}")
                        });
                    }
                    s.messages.push(msg);
                    s.status = SessStatus::Idle;
                    s.dirty = true;
                }
                let _ = &mut text;
            }
            AppEvent::OcForked { dir, session, source } => {
                // New pane sharing the forked history; inherits the source's
                // model/agent and carries the pending prompt.
                let id = self.next_session;
                self.next_session += 1;
                let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
                self.remember_dir(&dir_c);
                self.preload_sessions(dir_c.clone());
                let base = self
                    .sessions
                    .iter()
                    .find(|s| s.id == source)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "session".into());
                // Name forks `base(fork#N)`, numbering across all forks.
                let base = match base.find("(fork#") {
                    Some(i) => base[..i].trim_end().to_string(),
                    None => base,
                };
                let prefix = format!("{base}(fork#");
                let mut max_fork = 0usize;
                for s in &self.sessions {
                    if let Some(rest) = s.name.strip_prefix(&prefix) {
                        if let Some(n) = rest
                            .strip_suffix(')')
                            .and_then(|n| n.parse::<usize>().ok())
                        {
                            max_fork = max_fork.max(n);
                        }
                    }
                }
                let name = format!("{base}(fork#{})", max_fork + 1);
                let (model, agent, agy_model) = self
                    .sessions
                    .iter()
                    .find(|s| s.id == source)
                    .map(|s| (s.model.clone(), s.agent.clone(), s.agy_model.clone()))
                    .unwrap_or((None, None, None));
                let mut sess = SessionState::new(id, name.clone(), dir_c.clone());
                sess.oc_sid = Some(session.id.clone());
                sess.model = model;
                sess.agent = agent;
                sess.agy_model = agy_model;
                sess.status = SessStatus::Connecting;
                if let Some((_, text)) = self.pending_fork_src.take() {
                    self.pending_fork.push((id, text));
                }
                self.sessions.push(sess);
                let area = self.pane_area();
                self.grid.insert_session(id, area.width, area.height);
                self.focus = id;
                self.maximized = None;
                let req = self.manager.next_req();
                self.pending_create.insert(id, req);
                self.manager.connect_session(
                    req,
                    dir_c,
                    name,
                    Some(session.id.clone()),
                    None,
                    self.cfg.behavior.history_limit,
                );
                self.dirty = true;
                self.save_workspace();
            }
            AppEvent::SessionsPreloaded { dir, sessions } => {
                self.session_cache.insert(dir.clone(), sessions.clone());
                // Keep any open picker in sync with newly arrived directories.
                if self.overlay == Overlay::ResumeSession {
                    self.resume_picker.items = self.all_cached_sessions();
                    let n = self.resume_picker.items.len();
                    if self.resume_picker.selected >= n {
                        self.resume_picker.selected = n.saturating_sub(1);
                    }
                    self.resume_picker.loaded = true;
                }
                if self.overlay == Overlay::NewSession {
                    self.newdlg.recent = self.all_cached_sessions();
                    let n = self.newdlg.recent.len();
                    if self.newdlg.recent_selected >= n {
                        self.newdlg.recent_selected = n.saturating_sub(1);
                    }
                    self.newdlg.recent_loaded = true;
                }
                // Fresh boot with no saved workspace but older sessions on
                // the server: open the resume picker so they're one key away.
                if self.sessions.is_empty()
                    && !self.restored
                    && !sessions.is_empty()
                    && self.overlay == Overlay::None
                {
                    self.resume_picker.items = self.all_cached_sessions();
                    self.resume_picker.selected = 0;
                    self.resume_picker.loaded = true;
                    self.resume_picker.req = None;
                    self.overlay = Overlay::ResumeSession;
                }
            }
            AppEvent::ServerSessionsListed { req, dir, sessions } => {
                self.session_cache.insert(dir, sessions);
                if self.newdlg.recent_req == Some(req) {
                    self.newdlg.recent = self.all_cached_sessions();
                    self.newdlg.recent_selected = 0;
                    self.newdlg.recent_loaded = true;
                }
                if self.resume_picker.req == Some(req) {
                    self.resume_picker.items = self.all_cached_sessions();
                    self.resume_picker.selected = 0;
                    self.resume_picker.loaded = true;
                }
            }
            AppEvent::AgentsListed { agents, .. } => {
                self.agents = agents;
            }
            AppEvent::CommandsListed { commands, .. } => {
                self.custom_commands = commands;
            }
            AppEvent::OpResult { ok, message } => {
                self.flash(message);
                let _ = ok;
            }
            AppEvent::FileLoaded {
                req,
                path,
                content,
                diff,
            } => {
                if let Some(FileReq::Viewer { line }) = self.pending_files.remove(&req) {
                    match content {
                        Some(text) => {
                            let guard = crate::highlight::get();
                            let hl = guard.as_ref().map(|(_, h)| h).expect("highlighter");
                            let mut lines = hl
                                .highlight_file(&path, &text)
                                .into_iter()
                                .into_iter()
                                .map(Line::from)
                                .collect::<Vec<_>>();
                            if let Some(d) = diff {
                                lines.push(Line::from(""));
                                lines.extend(crate::highlight::diff_lines(&d));
                            }
                            self.viewer = Some(ViewerState {
                                title: theme::abbreviate_path(&path),
                                lines,
                                raw: text.clone(),
                                scroll: 0,
                                jump_line: line,
                            });
                        }
                        None => {
                            self.flash(format!("cannot display {path} (binary or missing)"));
                        }
                    }
                }
            }
        }
    }

    pub fn route_oc_event(&mut self, dir: PathBuf, ev: OcEvent) {
        let Some(sid) = ev.session_id() else { return };
        let Some(sess_idx) = self
            .sessions
            .iter()
            .position(|s| s.oc_sid.as_deref() == Some(sid.as_str()) && s.dir == dir)
        else {
            return;
        };
        let sess_id = self.sessions[sess_idx].id;

        match ev.typ.as_str() {
            "message.updated" => {
                if let Some(info) = ev.properties.get("info") {
                    if let Some(msg) = crate::opencode::parse_message(info) {
                        let aborted = msg
                            .error
                            .as_ref()
                            .map(|e| e.contains("Aborted"))
                            .unwrap_or(false);
                        let s = &mut self.sessions[sess_idx];
                        s.upsert_message_meta(&msg);
                        if msg.role == Role::User {
                            // message.updated carries no parts, so match by
                            // FIFO against optimistic sends.
                            s.adopt_oldest(&msg);
                        }
                        if let Some(err) = msg.error {
                            if aborted {
                                // User-initiated interrupt: not an error state.
                                s.status = SessStatus::Idle;
                            } else {
                                s.last_error = Some(err.clone());
                                s.status = SessStatus::Error(err);
                            }
                        }
                        s.dirty = true;
                    }
                }
            }
            "message.part.updated" => {
                if let Some(pv) = ev.properties.get("part") {
                    if let Some(part) = crate::opencode::parse_part(pv) {
                        let msg_id = part.message_id.clone();
                        let existing_role = self.sessions[sess_idx]
                            .messages
                            .iter()
                            .find(|m| m.id == msg_id)
                            .map(|m| m.role);
                        let meta = Message {
                            id: msg_id,
                            role: existing_role.unwrap_or(Role::Assistant),
                            error: None,
                            completed: None,
                            created: None,
                            cost: None,
                            tokens: None,
                            parts: vec![part.clone()],
                        };
                        let s = &mut self.sessions[sess_idx];
                        s.upsert_part(&meta, part.clone());
                        match &part.kind {
                            PartKind::Reasoning { running: true, .. } => {
                                if s.status != SessStatus::Permission {
                                    s.status = SessStatus::Thinking;
                                }
                            }
                            PartKind::Tool(t)
                                if matches!(
                                    t.status,
                                    ToolStatus::Pending | ToolStatus::Running
                                ) =>
                            {
                                if s.status != SessStatus::Permission {
                                    s.status = SessStatus::Working;
                                }
                            }
                            _ => {}
                        }
                        s.dirty = true;
                    }
                }
            }
            "message.part.removed" => {
                let msg_id = ev
                    .properties
                    .get("messageID")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let part_id = ev
                    .properties
                    .get("partID")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.sessions[sess_idx].remove_part(&msg_id, &part_id);
            }
            "session.error" => {
                let err = ev.properties.get("error");
                let name = err
                    .and_then(|e| e.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string();
                if name.contains("Aborted") {
                    // User-initiated interrupt.
                    let s = &mut self.sessions[sess_idx];
                    s.status = SessStatus::Idle;
                    s.dirty = true;
                    return;
                }
                let msg = err
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        err.and_then(|e| e.get("data"))
                            .and_then(|d| d.get("message"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                let text = match msg {
                    Some(m) if !m.is_empty() => format!("{name}: {m}"),
                    _ => name,
                };
                let s = &mut self.sessions[sess_idx];
                s.last_error = Some(text.clone());
                s.status = SessStatus::Error(text);
                s.dirty = true;
            }
            "session.idle" => {
                let has_queue = !self.sessions[sess_idx].queue.is_empty();
                let sid = self.sessions[sess_idx].id;
                {
                    let s = &mut self.sessions[sess_idx];
                    if !matches!(s.status, SessStatus::Error(_) | SessStatus::Permission) {
                        s.status = SessStatus::Idle;
                    }
                    s.interrupt_armed = None;
                    s.dirty = true;
                }
                if has_queue {
                    self.drain_queue(sid);
                }
            }
            "session.status" => {
                if let Some(st) = ev.properties.get("status") {
                    let typ = st.get("type").and_then(|v| v.as_str()).unwrap_or("idle");
                    let s = &mut self.sessions[sess_idx];
                    match typ {
                        "busy" => {
                            if s.status != SessStatus::Permission {
                                s.status = SessStatus::Working;
                            }
                        }
                        "retry" => {
                            let msg = st
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("retrying")
                                .to_string();
                            s.status = SessStatus::Retrying(msg);
                        }
                        _ => {
                            if !matches!(s.status, SessStatus::Error(_) | SessStatus::Permission) {
                                s.status = SessStatus::Idle;
                            }
                        }
                    }
                    s.dirty = true;
                }
            }
            "permission.asked" => {
                let pid = ev
                    .properties
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let kind = ev
                    .properties
                    .get("permission")
                    .and_then(|v| v.as_str())
                    .unwrap_or("permission")
                    .to_string();
                let mut details: Vec<String> = Vec::new();
                if let Some(meta) = ev.properties.get("metadata") {
                    for key in ["command", "filePath", "file", "path", "url", "description"] {
                        if let Some(v) = meta.get(key).and_then(|v| v.as_str()) {
                            details.push(v.to_string());
                            break;
                        }
                    }
                }
                if details.is_empty() {
                    if let Some(pats) = ev.properties.get("patterns").and_then(|p| p.as_array()) {
                        for p in pats.iter().take(2) {
                            if let Some(ps) = p.as_str() {
                                details.push(ps.to_string());
                            }
                        }
                    }
                }
                let detail = details.join(" ");
                let auto = self.cfg.behavior.auto_approve_permissions;
                let (oc_sid, dir2) = {
                    let s = &self.sessions[sess_idx];
                    (s.oc_sid.clone(), s.dir.clone())
                };
                if auto {
                    if let Some(oc_sid) = oc_sid {
                        self.manager
                            .reply_permission(dir2, oc_sid, pid.clone(), "once".into());
                    }
                }
                let s = &mut self.sessions[sess_idx];
                s.pending_perm = if auto {
                    None
                } else {
                    Some(PendingPermission { id: pid, kind, detail })
                };
                s.status = if auto {
                    SessStatus::Working
                } else {
                    SessStatus::Permission
                };
                s.dirty = true;
            }
            "permission.replied" => {
                let s = &mut self.sessions[sess_idx];
                if s.pending_perm.is_some() {
                    s.pending_perm = None;
                    s.status = SessStatus::Working;
                    s.dirty = true;
                }
            }
            "question.asked" | "question.v2.asked" => {
                if let Some(q) = crate::opencode::parse_question_request(&ev.properties) {
                    let sid = self.sessions[sess_idx].id;
                    self.sessions[sess_idx].pending_question =
                        Some(PendingQuestion::new(q.id, q.questions));
                    self.sessions[sess_idx].status = SessStatus::Question;
                    self.sessions[sess_idx].dirty = true;
                    if self.overlay == Overlay::None || self.overlay == Overlay::Question {
                        self.focus = sid;
                        self.overlay = Overlay::Question;
                    }
                }
            }
            "question.replied" | "question.rejected" | "question.v2.replied"
            | "question.v2.rejected" => {
                let s = &mut self.sessions[sess_idx];
                s.pending_question = None;
                if s.status == SessStatus::Question {
                    s.status = SessStatus::Working;
                }
                s.dirty = true;
                self.open_pending_question();
            }
            "file.watcher.updated" | "file.edited" => {
                self.explorer.dirty = true;
                self.git_cache.invalidate(&dir);
            }
            "vcs.branch.updated" => {
                self.git_cache.invalidate(&dir);
            }
            _ => {}
        }
        let _ = sess_id;
    }

    pub async fn on_tick(&mut self) {
        // The loop runs at 40ms (smooth status scanner); `tick` keeps the
        // original 120ms cadence for the other animations/timers.
        self.anim = self.anim.wrapping_add(1);
        if self.anim % 3 == 0 {
            self.tick = self.tick.wrapping_add(1);
        }

        if let Some((_, at)) = self.flash {
            if at.elapsed() > Duration::from_secs(3) {
                self.flash = None;
                self.dirty = true;
            }
        }

        // Expire an armed Esc-interrupt so a single stray press is harmless.
        for s in self.sessions.iter_mut() {
            if let Some(at) = s.interrupt_armed {
                if at.elapsed() > Duration::from_millis(2500) {
                    s.interrupt_armed = None;
                    s.dirty = true;
                }
            }
            // Let a `/push` status linger, then clear it.
            if let Some((_, at)) = &s.activity {
                if at.elapsed() > Duration::from_secs(10) {
                    s.activity = None;
                    s.dirty = true;
                }
            }
        }

        // Surface a pending agent question as soon as no other modal is open.
        if self.overlay == Overlay::None
            && self.sessions.iter().any(|s| {
                s.pending_question
                    .as_ref()
                    .map(|pq| pq.current().is_some())
                    .unwrap_or(false)
            })
        {
            self.open_pending_question();
        }

        if self.tick % 15 == 0 {
            if let Some(s) = self.focused() {
                let dir = s.dir.clone();
                let info = self.git_cache.get(&dir).await;
                if self.git_display != info {
                    self.git_display = info;
                    self.dirty = true;
                }
            }
        }

        if self.explorer.open && self.explorer.dirty {
            self.rebuild_explorer();
        }

        // Retry provider fetch while the model picker is open and empty.
        if self.overlay == Overlay::ModelPicker
            && self.providers.is_empty()
            && self.tick % 15 == 0
        {
            if let Some(dir) = self.focused().map(|s| s.dir.clone()) {
                self.manager.refresh_providers(dir);
            }
        }

        if self.last_save.elapsed() > Duration::from_secs(30) {
            self.save_workspace();
        }

        // Keep animating while any workspace is active (so the slider runs).
        if self.active_count() > 0 {
            self.dirty = true;
        }
    }

    pub fn pane_view_height(&self) -> usize {
        (self.last_body_area.height.saturating_sub(8)).max(5) as usize
    }
}

pub fn filtered_commands(query: &str) -> Vec<&'static Command> {
    static CMDS: std::sync::OnceLock<Vec<Command>> = std::sync::OnceLock::new();
    let cmds = CMDS.get_or_init(all_commands);
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return cmds.iter().collect();
    }
    let mut scored: Vec<(usize, &Command)> = cmds
        .iter()
        .filter_map(|c| {
            let label = c.label.to_lowercase();
            let pos = label.find(&q)?;
            Some((pos, c))
        })
        .collect();
    scored.sort_by_key(|(pos, _)| *pos);
    scored.into_iter().map(|(_, c)| c).collect()
}

fn geometric_neighbor(cur: Rect, dir: Dir, rects: &[(u32, Rect)]) -> Option<u32> {
    let cx = (cur.x + cur.width / 2) as i32;
    let cy = (cur.y + cur.height / 2) as i32;
    let mut best: Option<(u64, u32)> = None;
    for (sid, r) in rects {
        if *r == cur {
            continue;
        }
        let tx = (r.x + r.width / 2) as i32;
        let ty = (r.y + r.height / 2) as i32;
        let ok = match dir {
            Dir::Left => tx < cx && (ty - cy).abs() < (r.height.max(cur.height) as i32),
            Dir::Right => tx > cx && (ty - cy).abs() < (r.height.max(cur.height) as i32),
            Dir::Up => ty < cy && (tx - cx).abs() < (r.width.max(cur.width) as i32),
            Dir::Down => ty > cy && (tx - cx).abs() < (r.width.max(cur.width) as i32),
            _ => false,
        };
        if ok {
            let d = (tx - cx).abs() as u64 + (ty - cy).abs() as u64;
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, *sid));
            }
        }
    }
    best.map(|(_, sid)| sid)
}

fn walk_tree(dir: &Path, depth: usize, expanded: &std::collections::HashSet<String>, items: &mut Vec<ExpItem>) {
    if depth > 12 || items.len() > 3000 {
        return;
    }
    for e in crate::fsx::list_dir(dir) {
        items.push(ExpItem {
            path: e.path.clone(),
            name: e.name.clone(),
            depth,
            is_dir: e.is_dir,
        });
        if e.is_dir && expanded.contains(&e.path) {
            walk_tree(Path::new(&e.path), depth + 1, expanded, items);
        }
    }
}
