//! Application state: sessions, panes, overlays, event routing, key handling.

use crate::config::Config;
use crate::events::{AppEvent, ReqId};
use crate::git::{GitCache, GitInfo};
use crate::harness::transcript::{Message, PartKind, Role};
use crate::harness::{HarnessEvent, NotificationPolicy, Task, TaskStatus, TranscriptUpdate};
use crate::keys::Action;
use crate::manager::Manager;
use crate::models::{GrepMatch, ModelEntry, ModelRef, OcSession};
use crate::panes::{Dir, PaneGrid};
use crate::persist;
use crate::session::{Activity, InputState, PendingPermission, PendingQuestion, SessionState, SessStatus};
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
    Question,
    ModelPicker,
    AgentPicker,
    SessionList,
    LayoutPicker,
    ResumeSession,
    Tree,
    /// agy-style `/undo` rewind picker.
    Rewind,
    /// Interactive provider login.
    Login,
    /// Live tail of the debug log (`/logs`).
    Logs,
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
    Delete,
    Rename,
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
    Fork,
    Tree,
    Editor,
    Export,
    Login,
    Logs,
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
        SlashItem { name: "undo".into(), args: "".into(), desc: "Rewind to an earlier message (pick from a list)".into(), kind: SlashKind::Undo },
        SlashItem { name: "redo".into(), args: "".into(), desc: "Re-apply the last rewind".into(), kind: SlashKind::Redo },
        SlashItem { name: "share".into(), args: "".into(), desc: "Share this session (get URL)".into(), kind: SlashKind::Share },
        SlashItem { name: "unshare".into(), args: "".into(), desc: "Stop sharing this session".into(), kind: SlashKind::Unshare },
        SlashItem { name: "init".into(), args: "[focus]".into(), desc: "Create/update AGENTS.md".into(), kind: SlashKind::Init },
        SlashItem { name: "keys".into(), args: "".into(), desc: "View and edit keybindings".into(), kind: SlashKind::Keymap },
        SlashItem { name: "help".into(), args: "".into(), desc: "Overview of keys and commands".into(), kind: SlashKind::Keymap },
        SlashItem { name: "close".into(), args: "".into(), desc: "Close this session".into(), kind: SlashKind::Close },
        SlashItem { name: "delete".into(), args: "".into(), desc: "Delete this session from the workspace (server history kept)".into(), kind: SlashKind::Delete },
        SlashItem { name: "rename".into(), args: "[name]".into(), desc: "Rename this session".into(), kind: SlashKind::Rename },
        SlashItem { name: "quit".into(), args: "".into(), desc: "Quit Theta".into(), kind: SlashKind::Quit },
        SlashItem { name: "refresh".into(), args: "".into(), desc: "Reload the newest build in place".into(), kind: SlashKind::Refresh },
        SlashItem { name: "tree".into(), args: "".into(), desc: "Jump to an earlier point (local backend)".into(), kind: SlashKind::Tree },
        SlashItem { name: "editor".into(), args: "".into(), desc: "Compose the prompt in $EDITOR".into(), kind: SlashKind::Editor },
        SlashItem { name: "export".into(), args: "[file]".into(), desc: "Export this session (Markdown/JSONL)".into(), kind: SlashKind::Export },
        SlashItem { name: "login".into(), args: "[provider]".into(), desc: "Log in to a provider (API key)".into(), kind: SlashKind::Login },
        SlashItem { name: "logs".into(), args: "".into(), desc: "Tail the debug log (requests → model)".into(), kind: SlashKind::Logs },
        SlashItem { name: "push".into(), args: "[message]".into(), desc: "Commit and push this project (session only)".into(), kind: SlashKind::Push },
        SlashItem { name: "fork".into(), args: "".into(), desc: "Fork this session into a new pane (instant, session only)".into(), kind: SlashKind::Fork },
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

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum LoginStage {
    #[default]
    Choose,
    /// Custom endpoint: enter a base URL first.
    Url,
    Key,
}

#[derive(Default)]
pub struct LoginState {
    pub stage: LoginStage,
    /// `(provider id, display label, env var, already configured)`.
    pub providers: Vec<(String, String, String, bool)>,
    pub selected: usize,
    pub input: InputState,
    /// Chosen provider (Key stage).
    pub provider: String,
    /// Display label of the chosen provider.
    pub provider_label: String,
    /// Env var that holds the chosen provider's key.
    pub env: String,
    /// Base URL for the custom endpoint.
    pub url: String,
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

/// One row of the `/tree` history navigator.
#[derive(Clone)]
pub struct TreeRow {
    pub id: String,
    pub depth: usize,
    pub label: String,
    pub kind: crate::tree::EntryKind,
    pub active: bool,
}

#[derive(Default)]
pub struct TreeUi {
    pub items: Vec<TreeRow>,
    pub selected: usize,
}

/// Live debug-log tail (`/logs`).
#[derive(Default)]
pub struct LogView {
    pub lines: Vec<String>,
    pub scroll: usize,
    /// Keep pinned to the newest line as the log grows.
    pub follow: bool,
    pub path: Option<PathBuf>,
}

/// One user turn offered by the `/undo` rewind picker.
#[derive(Clone)]
pub struct RewindRow {
    /// Index into the session transcript where this user message starts.
    pub index: usize,
    pub text: String,
    /// Local backend: the tree entry id to rewind before.
    pub entry: Option<String>,
    /// OpenCode backend: the message id to revert.
    pub msg_id: Option<String>,
    pub adds: u32,
    pub dels: u32,
    pub files: usize,
}

#[derive(Default)]
pub struct RewindState {
    pub rows: Vec<RewindRow>,
    pub selected: usize,
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
    Refresh,
    Tree,
    Editor,
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
        Command { label: "Refresh (reload build)", hint: "/refresh", cmd: Cmd::Refresh },
        Command { label: "History tree", hint: "/tree", cmd: Cmd::Tree },
        Command { label: "Edit in $EDITOR", hint: "Ctrl+G", cmd: Cmd::Editor },
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
    /// Monotonic id source for harness tasks.
    pub next_task: u64,
    pub manager: Manager,
    pub initial_dir: PathBuf,
    pub flash: Option<(String, Instant)>,
    /// Guards against a terminal emitting a newline twice per keypress.
    pub last_newline: Option<Instant>,
    /// Harness-level notification debounce/grouping.
    pub notifications: NotificationPolicy,
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
    pub agents: Vec<crate::models::AgentInfo>,
    pub custom_commands: Vec<crate::models::CustomCommand>,
    pub model_picker: ModelPickerState,
    pub agent_picker: AgentPickerState,
    pub session_list: SessionListState,
    pub layout_picker: LayoutPickerState,
    /// Selection in the busy-prompt dialog (0 queue, 1 new workspace).
    pub busy_choice: usize,
    pub resume_picker: ResumePickerState,
    /// History tree navigator state (`/tree`).
    pub tree_ui: TreeUi,
    /// `/undo` rewind picker state.
    pub rewind_ui: RewindState,
    /// Interactive `/login` state.
    pub login_ui: LoginState,
    /// `/logs` live tail state.
    pub log_view: LogView,
    /// Set when the TUI should suspend and open `$EDITOR` for a prompt.
    pub pending_editor: Option<(u32, String)>,
    pub keys: crate::keys::Keymap,
    pub keymap_ui: KeymapUi,
    pub theme_ui: ThemeUi,
    /// Sessions per directory, preloaded at boot for instant resume lists.
    pub session_cache: HashMap<PathBuf, Vec<OcSession>>,
    /// Every directory Theta has opened (session lists span all of them).
    pub known_dirs: BTreeSet<PathBuf>,
    /// (new theta session id, prompt) — sent once the forked pane connects.
    pub pending_fork: Vec<(u32, String)>,
    /// (source theta session id, placeholder theta session id) whose panes
    /// were created instantly and are waiting for the provider fork to land.
    pub fork_wait: Vec<(u32, u32)>,

    pub restored: bool,
    #[allow(dead_code)]
    pub started: Instant,
    pub last_save: Instant,
    /// Per-session scroll signature at the last scroll save, so scrolling is
    /// persisted as it happens — not only on a clean exit.
    scroll_sig: Vec<(u32, usize, bool)>,
    /// Timestamp of the last lightweight scroll save (throttles writes).
    last_scroll_save: Option<Instant>,
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
            next_task: 1,
            manager,
            initial_dir,
            flash: None,
            last_newline: None,
            notifications: NotificationPolicy::new(8000),
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
            tree_ui: TreeUi::default(),
            rewind_ui: RewindState::default(),
            login_ui: LoginState::default(),
            log_view: LogView::default(),
            pending_editor: None,
            keys: crate::keys::Keymap::load(&key_overrides),
            session_cache: HashMap::new(),
            known_dirs: BTreeSet::new(),
            pending_fork: Vec::new(),
            fork_wait: Vec::new(),
            keymap_ui: KeymapUi::default(),
            theme_ui: ThemeUi::default(),
            restored: false,
            started: Instant::now(),
            last_save: Instant::now(),
            scroll_sig: Vec::new(),
            last_scroll_save: None,
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

    /// Sessions currently summarizing their context.
    pub fn compacting_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.status == SessStatus::Compacting)
            .count()
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

    /// Open a harness task for a freshly submitted prompt. It is finished when
    /// the harness sees the session idle/error/interrupted.
    fn start_task(&mut self, id: u32, title: &str) {
        let task_id = self.next_task;
        self.next_task += 1;
        if let Some(s) = self.session_mut(id) {
            let mut t = Task::new(task_id, title, s.dir.clone(), s.session_id);
            t.start();
            s.task = Some(t);
        }
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
        // Cancel the harness task so it does not linger as running.
        if let Some(s) = self.session_mut(id) {
            if let Some(t) = s.task.as_mut() {
                t.finish(TaskStatus::Cancelled);
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
        // `!cmd` runs a shell command and sends its output to the agent;
        // `!!cmd` runs it without sending. Works even before connecting.
        if let Some(s) = self.session(id) {
            if let Some((send, cmd)) = parse_shell_line(s.input.text()) {
                let limit = self.cfg.ui.history_limit;
                let entry = format!("{}{}", if send { "!" } else { "!!" }, cmd);
                if let Some(sm) = self.session_mut(id) {
                    sm.input.take();
                    sm.input.push_history(&entry, limit);
                }
                self.run_shell(id, cmd, send);
                return;
            }
        }
        let limit = self.cfg.ui.history_limit;
        let (text, dir, oc_sid, model, agent, expanded, attachments) = {
            let Some(s) = self.session_mut(id) else { return };
            if s.oc_sid.is_none() {
                self.flash("session is still connecting…");
                return;
            }
            let text = s.input.take();
            s.mention_results.clear();
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
            // Expand collapsed pastes and gather @file + pasted attachments.
            let expanded = crate::paste::expand(&text, &s.paste_parts);
            let mut attachments = Self::attachments_for(&s.dir, &expanded);
            attachments.extend(crate::paste::attachments(&text, &s.paste_parts));
            s.paste_parts.clear();
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
                expanded,
                attachments,
            )
        };
        self.start_task(id, &text);
        self.manager
            .send_prompt(dir, oc_sid, expanded, model, agent, attachments);
    }

    /// Run a `!`/`!!` shell escape in the session's directory.
    fn run_shell(&mut self, id: u32, command: String, send_to_agent: bool) {
        let Some(dir) = self.session(id).map(|s| s.dir.clone()) else {
            return;
        };
        let tx = self.manager_tx();
        let cmd = command.clone();
        // Show the command immediately; output arrives via `ShellDone`.
        if let Some(s) = self.session_mut(id) {
            s.push_local_user(&format!("$ {cmd}"));
            s.status = SessStatus::Working;
            s.stick_bottom = true;
            s.dirty = true;
        }
        tokio::spawn(async move {
            let out = tokio::process::Command::new("bash")
                .arg("-lc")
                .arg(&cmd)
                .current_dir(&dir)
                .output()
                .await;
            let (ok, body) = match out {
                Ok(o) => {
                    let mut b = String::from_utf8_lossy(&o.stdout).to_string();
                    let e = String::from_utf8_lossy(&o.stderr);
                    if !e.trim().is_empty() {
                        b.push_str("\n[stderr]\n");
                        b.push_str(&e);
                    }
                    (o.status.success(), b)
                }
                Err(e) => (false, format!("spawn failed: {e}")),
            };
            let mut body = body;
            if body.len() > 100 * 1024 {
                body.truncate(100 * 1024);
                body.push_str("\n… [truncated]");
            }
            let _ = tx.send(AppEvent::ShellDone {
                session: id,
                command: cmd,
                ok,
                output: body,
                send_to_agent,
            });
        });
    }

    /// Resolve `@file` mentions in `text` against `dir`.
    pub fn attachments_for(dir: &Path, text: &str) -> Vec<crate::mentions::Attachment> {
        crate::mentions::to_attachments(&crate::mentions::extract(text, dir))
    }

    /// Insert clipboard text into the active modal field and refresh any
    /// search driven by that field. Returns true when a modal consumed the
    /// paste, even if the modal has no text field.
    fn apply_overlay_paste(&mut self, raw: &str) -> bool {
        if !self.insert_overlay_text(raw) {
            return false;
        }
        match self.overlay {
            Overlay::FileSearch => self.run_file_search(),
            Overlay::ProjectSearch => self.run_project_search(),
            Overlay::ConvSearch => self.update_conv_matches(),
            _ => {}
        }
        self.dirty = true;
        true
    }

    /// Insert bracketed-paste text into the active modal field, if any.
    /// Returns true when the paste was consumed (including when a modal has no
    /// text field, so background chat never receives modal-window pastes).
    fn insert_overlay_text(&mut self, raw: &str) -> bool {
        let text = raw.replace("\r\n", "\n").replace('\r', "\n");
        match self.overlay {
            Overlay::None => false,
            Overlay::Palette => {
                self.palette.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::Rename => {
                self.rename.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::ModelPicker => {
                self.model_picker.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::AgentPicker => {
                self.agent_picker.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::FileSearch => {
                self.file_search.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::ProjectSearch => {
                self.proj_search.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::ConvSearch => {
                self.conv_search.input.insert(&text.replace('\n', " "));
                true
            }
            Overlay::NewSession => match self.newdlg.field {
                0 => {
                    self.newdlg.name.insert(&text.replace('\n', " "));
                    true
                }
                1 => {
                    let path = text.replace('\n', "").trim().to_string();
                    if !path.is_empty() {
                        self.newdlg.dir.push_str(&path);
                    }
                    true
                }
                _ => true,
            },
            Overlay::Question => {
                let sid = self.focus;
                if let Some(s) = self.session_mut(sid) {
                    if let Some(pq) = s.pending_question.as_mut() {
                        let custom = pq.current().map(|q| q.custom).unwrap_or(false);
                        if custom {
                            pq.custom.push_str(&text);
                            s.dirty = true;
                        }
                    }
                }
                true
            }
            Overlay::Login => {
                // The provider list has no text field; only the custom-endpoint
                // URL and API-key stages accept a paste. Newlines are stripped
                // so keys/URLs from a multi-line clipboard stay intact.
                if !matches!(self.login_ui.stage, LoginStage::Choose) {
                    self.login_ui.input.insert(&text.replace(['\n', '\r'], ""));
                    self.dirty = true;
                }
                true
            }
            _ => true,
        }
    }

    /// Insert pasted text, collapsing long pastes into a placeholder.
    fn add_paste_text(&mut self, id: u32, text: &str) {
        let Some(s) = self.session_mut(id) else { return };
        if crate::paste::is_long(text) {
            let ph = crate::paste::text_placeholder(text);
            s.paste_parts.push(crate::paste::PastePart {
                placeholder: ph.clone(),
                content: crate::paste::PasteContent::Text(text.to_string()),
            });
            s.input.insert(&ph);
        } else {
            s.input.insert(text);
        }
        s.dirty = true;
        self.dirty = true;
    }

    /// Insert a pasted image/file as an attachment placeholder.
    fn add_paste_image(&mut self, id: u32, mime: String, bytes: Vec<u8>) {
        let Some(s) = self.session_mut(id) else { return };
        s.paste_seq += 1;
        let n = s.paste_seq;
        let ph = crate::paste::file_placeholder(n as usize, &mime);
        let ext = mime.rsplit('/').next().unwrap_or("bin").to_string();
        let filename = format!("clipboard-{n}.{ext}");
        let url = crate::paste::data_url(&mime, &bytes);
        s.paste_parts.push(crate::paste::PastePart {
            placeholder: ph.clone(),
            content: crate::paste::PasteContent::File { mime, filename, url },
        });
        s.input.insert(&format!("{ph} "));
        s.dirty = true;
        self.dirty = true;
    }

    /// Read the system clipboard (Ctrl+V) and paste text or an image.
    /// Modal dialogs take precedence over the background chat box. Image/file
    /// clipboard content is only supported in an open session prompt.
    async fn paste_from_clipboard(&mut self) {
        if self.viewer.is_some() || self.diff.is_some() {
            self.flash("Close the viewer to paste");
            self.dirty = true;
            return;
        }
        let data = tokio::task::spawn_blocking(crate::paste::read_clipboard)
            .await
            .ok()
            .flatten();
        match data {
            Some(crate::paste::Clipboard::Text(t)) => {
                if self.apply_overlay_paste(&t) {
                    return;
                }
                let Some(id) = self.focused().map(|s| s.id) else {
                    return;
                };
                self.add_paste_text(id, &t);
            }
            Some(crate::paste::Clipboard::Image { mime, bytes }) => {
                if self.overlay != Overlay::None {
                    self.flash("Images can only be pasted into an open chat prompt");
                    self.dirty = true;
                    return;
                }
                let Some(id) = self.focused().map(|s| s.id) else {
                    return;
                };
                self.add_paste_image(id, mime, bytes);
            }
            None => self.flash("clipboard empty or unavailable"),
        }
    }

    /// Refresh the `@file` suggestion list for the focused session.
    async fn refresh_mentions(&mut self, id: u32) {
        let (dir, text) = match self.session(id) {
            Some(s) => (s.dir.clone(), s.input.buf.clone()),
            None => return,
        };
        let query = crate::mentions::active_query(&text).map(|(_, q)| q);
        let results = match query {
            Some(q) if !q.is_empty() => {
                let dir2 = dir.clone();
                tokio::task::spawn_blocking(move || crate::fsx::find_files(&dir2, &q, 30))
                    .await
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        if let Some(s) = self.session_mut(id) {
            s.mention_results = results;
            s.mention_selected = 0;
            s.dirty = true;
        }
        self.dirty = true;
    }

    /// Fire a prompt into a connected session (submit path and queue drain).
    fn send_text_now(&mut self, id: u32, text: &str) {
        let (dir, oc_sid, model, agent) = {
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
            )
        };
        self.start_task(id, text);
        let attachments = Self::attachments_for(&dir, text);
        self.manager
            .send_prompt(dir, oc_sid, text.to_string(), model, agent, attachments);
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
            // Respect provider capabilities: if native forking isn't offered,
            // fall back to queueing rather than silently doing the wrong thing.
            let can_fork = self
                .session(id)
                .map(|s| s.provider.capabilities().native_fork)
                .unwrap_or(false);
            if can_fork {
                self.fork_with_prompt(id, text);
            } else {
                if let Some(s) = self.session_mut(id) {
                    s.queue.push(text);
                    s.dirty = true;
                }
                self.flash("provider can't fork — queued instead");
            }
        }
        self.dirty = true;
    }

    /// Fork `id` into a fresh pane sharing its history, then send `text`
    /// there once the fork connects. History is never modified.
    fn fork_with_prompt(&mut self, id: u32, text: String) {
        // Pinned to the last settled point so the busy dialog doesn't inherit
        // the source's in-progress turn.
        self.begin_fork(id, Some(text), false);
    }

    /// `/fork`: duplicate the focused session with its complete history (up to
    /// the current tip, including the latest output). No prompt is sent.
    pub fn fork_active(&mut self) {
        let id = self.focus;
        self.begin_fork(id, None, true);
    }

    /// Create the forked pane immediately (seeded with the source transcript so
    /// it renders instantly), then ask the provider to fork in the background.
    /// `full` forks the entire session; otherwise it forks at the last settled
    /// message (`fork_point`).
    fn begin_fork(&mut self, source: u32, text: Option<String>, full: bool) {
        let can_fork = self
            .session(source)
            .map(|s| s.provider.capabilities().native_fork)
            .unwrap_or(false);
        if !can_fork {
            self.flash("provider can't fork this session");
            return;
        }
        if self.fork_wait.iter().any(|(s, _)| *s == source) {
            self.flash("a fork is already in progress…");
            return;
        }
        let (dir, oc_sid, at, seed, model, agent, base_name) = {
            let Some(s) = self.session(source) else {
                return;
            };
            let Some(oc) = s.oc_sid.clone() else {
                self.flash("session is still connecting…");
                return;
            };
            (
                s.dir.clone(),
                oc,
                if full { None } else { Self::fork_point(s) },
                s.messages.clone(),
                s.model.clone(),
                s.agent.clone(),
                s.name.clone(),
            )
        };

        let id = self.next_session;
        self.next_session += 1;
        let name = self.next_fork_name(&base_name);
        let mut sess = SessionState::new(id, name.clone(), dir.clone());
        sess.model = model;
        sess.agent = agent;
        // Seed with the source transcript so the pane is readable immediately;
        // the provider's forked history replaces it once it arrives.
        if !seed.is_empty() {
            sess.messages = seed;
            sess.recompute_metrics();
            sess.stick_bottom = true;
        }
        sess.status = SessStatus::Connecting;
        sess.dirty = true;
        self.sessions.push(sess);
        let area = self.pane_area();
        self.grid.insert_session(id, area.width, area.height);
        self.focus = id;
        self.maximized = None;
        if let Some(t) = text {
            if !t.trim().is_empty() {
                self.pending_fork.push((id, t));
            }
        }
        self.fork_wait.push((source, id));
        self.manager.fork_session(dir, oc_sid, source, at);
        self.flash("forking…");
        self.dirty = true;
        self.save_workspace();
    }

    /// `base(fork#N)` numbering across all existing forks.
    fn next_fork_name(&self, base: &str) -> String {
        let base = match base.find("(fork#") {
            Some(i) => base[..i].trim_end().to_string(),
            None => base.to_string(),
        };
        let prefix = format!("{base}(fork#");
        let mut max_fork = 0usize;
        for s in &self.sessions {
            if let Some(rest) = s.name.strip_prefix(&prefix) {
                if let Some(n) = rest.strip_suffix(')').and_then(|n| n.parse::<usize>().ok()) {
                    max_fork = max_fork.max(n);
                }
            }
        }
        format!("{base}(fork#{})", max_fork + 1)
    }

    /// The message to fork at: the newest real message, so the fork carries
    /// the entire conversation through to the end — including the latest
    /// assistant output and any answered agent question.
    fn fork_point(s: &SessionState) -> Option<String> {
        s.messages
            .iter()
            .rev()
            .find(|m| m.id.starts_with("msg"))
            .map(|m| m.id.clone())
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
            "Git push".to_string()
        } else {
            format!("Git push \"{statement}\"")
        };
        if let Some(s) = self.session_mut(id) {
            s.activity = Some(Activity {
                text: label.clone(),
                started: Instant::now(),
                done: false,
            });
            s.dirty = true;
        }
        crate::tlog!(
            "PUSH session={id} dir={} message={}",
            dir.display(),
            if statement.is_empty() { "<auto>" } else { &statement }
        );
        self.dirty = true;
        let tx = self.manager.tx();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::PushProgress {
                session: id,
                text: label,
            });
            let started = Instant::now();
            let result = crate::git::commit_and_push(&dir, &statement).await;
            // Keep the push animation on screen for a beat even when the
            // operation is near-instant, so it doesn't flash by unseen.
            let elapsed = started.elapsed();
            if elapsed < Duration::from_millis(1100) {
                tokio::time::sleep(Duration::from_millis(1100) - elapsed).await;
            }
            let (ok, message, repo, subject) = match result {
                Ok(o) => (true, String::new(), Some(o.repo), o.subject),
                Err(e) => (false, e.to_string(), None, String::new()),
            };
            let _ = tx.send(AppEvent::PushDone {
                session: id,
                ok,
                message,
                repo,
                subject,
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
            // Clear every blocking state so aborting always returns the pane
            // to a usable input box.
            s.status = SessStatus::Idle;
            s.interrupt_armed = None;
            s.pending_perm = None;
            s.pending_question = None;
            s.dirty = true;
        }
        if self.overlay == Overlay::Question {
            self.open_pending_question();
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
            if self.manager.is_local() {
                self.manager
                    .local_permission_reply(oc_sid, pid, response.to_string());
            } else {
                self.manager
                    .reply_permission(dir, oc_sid, pid, response.to_string());
            }
        }
        if let Some(s) = self.session_mut(id) {
            s.pending_perm = None;
            s.status = SessStatus::Working;
            s.dirty = true;
        }
        // "always" stops the per-tool prompts for good (persisted).
        if response == "always" && !self.cfg.behavior.auto_approve_permissions {
            self.cfg.behavior.auto_approve_permissions = true;
            let _ = self.cfg.save();
        }
        self.flash(match response {
            "reject" => "permission rejected",
            "always" => "allowed — no more access prompts",
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
        if let Some(target) = self.grid.focus_step(cur_id, dir) {
            self.focus = target;
            self.dirty = true;
            return;
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
            .map(|s| {
                let scroll = if s.stick_bottom {
                    let total = self.conv_cache.get(&s.id).map(|c| c.lines.len()).unwrap_or(0);
                    let h = self.pane_view_height();
                    total.saturating_sub(h) as u64
                } else {
                    s.scroll as u64
                };
                persist::SavedSession {
                    name: s.name.clone(),
                    dir: s.dir.to_string_lossy().to_string(),
                    oc_sid: s.oc_sid.clone(),
                    model: s
                        .model
                        .as_ref()
                        .map(|m| (m.provider_id.clone(), m.model_id.clone())),
                    agent: s.agent.clone(),
                    provider: Some(crate::providers::ProviderKind::Local.id().to_string()),
                    scroll,
                    stick_bottom: s.stick_bottom,
                }
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
        // Display-only transcript cache so the next launch renders instantly.
        let mut cache: HashMap<String, Vec<Message>> = HashMap::new();
        for s in &self.sessions {
            if let Some(key) = Self::transcript_key(s) {
                if !s.messages.is_empty() {
                    let start = s.messages.len().saturating_sub(persist::TRANSCRIPT_CACHE_LIMIT);
                    cache.insert(key, s.messages[start..].to_vec());
                }
            }
        }
        let _ = persist::save_transcripts(&cache);
        crate::tlog!("TRANSCRIPT cache saved {} sessions", cache.len());
    }

    /// Persist only the workspace layout + scroll positions. Cheaper than
    /// [`Self::save_workspace`], which also rewrites the transcript cache and
    /// is far too heavy to run on every scroll event.
    fn save_scroll_state(&mut self) {
        let _ = persist::save(&self.snapshot());
        self.last_scroll_save = Some(Instant::now());
    }

    /// Save scroll positions as they change, so a `/refresh`, a killed
    /// terminal or a crash doesn't lose the user's place.
    fn persist_scroll_if_changed(&mut self) {
        let sig: Vec<(u32, usize, bool)> = self
            .sessions
            .iter()
            .map(|s| (s.id, s.scroll, s.stick_bottom))
            .collect();
        if sig == self.scroll_sig {
            return;
        }
        self.scroll_sig = sig;
        // Throttle the write during a fast wheel scroll; the periodic full save
        // and the exit save both catch anything skipped here.
        let due = self
            .last_scroll_save
            .map(|t| t.elapsed() >= Duration::from_millis(200))
            .unwrap_or(true);
        if due {
            self.save_scroll_state();
        }
    }

    fn transcript_key(s: &SessionState) -> Option<String> {
        s.oc_sid
            .as_ref()
            .map(|id| format!("{}|{}", s.dir.display(), id))
    }

    pub fn restore_workspace(&mut self) {
        let Some(ws) = persist::load() else { return };
        let transcript_cache = persist::load_transcripts();
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
            sess.provider = saved
                .provider
                .as_deref()
                .and_then(crate::providers::ProviderKind::from_id)
                .unwrap_or_default();
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
            // Hydrate from the session tree or display cache so the pane is readable instantly
            // and positioned correctly from frame 1 without needing to replay turns.
            if let Some(oc) = &sess.oc_sid {
                let tree_msgs = crate::tree::SessionTree::sidecar_path(oc)
                    .and_then(|p| crate::tree::SessionTree::load(&p))
                    .map(|tree| tree.to_messages(oc))
                    .filter(|m| !m.is_empty());
                let msgs = tree_msgs.or_else(|| {
                    let key = format!("{}|{}", dir.display(), oc);
                    transcript_cache.get(&key).cloned()
                });
                if let Some(msgs) = msgs {
                    crate::tlog!("TRANSCRIPT loaded {} msgs for {}", msgs.len(), oc);
                    sess.messages = msgs;
                    sess.recompute_metrics();
                    sess.dirty = true;
                }
            }
            // Reopen where the user stopped scrolling instead of snapping to
            // the bottom: the saved offset is clamped to the cache length when
            // the pane renders, so a changed window size is safe.
            sess.scroll = saved.scroll as usize;
            sess.stick_bottom = saved.stick_bottom;
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
        // anyway and on_tick refreshes the list until it arrives. Refresh now
        // so models from every logged-in provider are fetched.
        self.model_picker.input.clear();
        self.model_picker.selected = 0;
        if let Some(dir) = self.focused().map(|s| s.dir.clone()) {
            self.manager.refresh_providers(dir);
        }
        self.overlay = Overlay::ModelPicker;
        self.dirty = true;
    }

    /// Known providers for the `/login` picker: `(id, label, env var)`.
    /// Shared with model discovery so login and `/model` agree.
    const LOGIN_PROVIDERS: &'static [(&'static str, &'static str, &'static str)] =
        crate::ai::discovery::PROVIDERS;

    /// Open the interactive provider login.
    /// True when the configured provider (or the last-used model's provider)
    /// has a usable credential.
    pub fn provider_configured(&self) -> bool {
        let creds = crate::credentials::Credentials::load();
        let mut ids = vec![self.cfg.ai.provider.to_ascii_lowercase()];
        if let Some((p, _)) = &self.cfg.last_model {
            ids.push(p.to_ascii_lowercase());
        }
        ids.iter().any(|id| {
            creds
                .resolve(id, &self.cfg.ai.api_key_env)
                .is_some()
                || !self.cfg.ai.base_url.trim().is_empty()
                || id == "ollama"
        })
    }

    /// `/logs`: live-tail the debug log inside the TUI (no second terminal).
    pub fn open_logs(&mut self) {
        if !crate::logging::enabled() {
            self.flash("logging is off — restart with `theta --log --run`");
            return;
        }
        self.log_view.follow = true;
        self.refresh_logs();
        self.overlay = Overlay::Logs;
        self.dirty = true;
    }

    /// Re-read the newest log file; keep pinned to the bottom when following.
    fn refresh_logs(&mut self) {
        let dir = crate::logging::logs_dir();
        let Some(path) = crate::logging::newest_log(&dir) else { return };
        self.log_view.lines = crate::logging::tail(&path, 2000);
        self.log_view.path = Some(path);
        if self.log_view.follow {
            self.log_view.scroll = self.log_view.lines.len().saturating_sub(1);
        }
    }

    pub fn open_login(&mut self) {
        let creds = crate::credentials::Credentials::load();
        self.login_ui = LoginState::default();
        self.login_ui.providers = Self::LOGIN_PROVIDERS
            .iter()
            .map(|(id, label, env)| {
                let configured = creds.resolve(id, env).is_some();
                ((*id).to_string(), (*label).to_string(), (*env).to_string(), configured)
            })
            .collect();
        let custom_configured = !self.cfg.ai.base_url.trim().is_empty();
        self.login_ui.providers.insert(
            0,
            (
                "custom".to_string(),
                "custom (OpenAI-compatible URL)".to_string(),
                String::new(),
                custom_configured,
            ),
        );
        self.login_ui.stage = LoginStage::Choose;
        self.overlay = Overlay::Login;
        self.dirty = true;
    }

    /// Move from the provider list to key entry.
    fn login_choose(&mut self) {
        let sel = self.login_ui.selected.min(self.login_ui.providers.len().saturating_sub(1));
        if let Some((id, label, env, _)) = self.login_ui.providers.get(sel) {
            self.login_ui.provider = id.clone();
            self.login_ui.provider_label = label.clone();
            self.login_ui.env = env.clone();
            self.login_ui.input.clear();
            self.login_ui.stage = if id == "custom" { LoginStage::Url } else { LoginStage::Key };
            self.dirty = true;
        }
    }

    /// Env var for a login provider id, or `None` if it is not a preset.
    fn login_env_for(provider: &str) -> Option<&'static str> {
        Self::LOGIN_PROVIDERS
            .iter()
            .find(|(id, _, _)| *id == provider)
            .map(|(_, _, env)| *env)
    }

    /// Point the live config at `provider`'s preset (endpoint + default model)
    /// so a `/login` takes effect without a restart. Returns the chosen model.
    fn activate_provider(&mut self, provider: &str, env: &str) -> String {
        let model = crate::manager::default_model_for(provider).to_string();
        self.cfg.ai.provider = provider.to_string();
        self.cfg.ai.base_url = String::new();
        self.cfg.ai.api_key_env = env.to_string();
        self.cfg.ai.model = model.clone();
        self.cfg.last_model = Some((provider.to_string(), model.clone()));
        let _ = self.cfg.save();
        self.manager.set_ai(provider, &model, "");
        self.manager.reload_credentials();
        // New key → re-fetch this provider's models exactly once; `/model`
        // then reads the persisted cache.
        crate::ai::discovery::invalidate(provider);
        if let Some(dir) = self.focused().map(|s| s.dir.clone()) {
            self.manager.refresh_providers(dir);
        }
        model
    }

    /// Persist the entered key (and endpoint) and reload providers.
    fn login_save(&mut self) {
        let provider = self.login_ui.provider.clone();
        let label = self.login_ui.provider_label.clone();
        let env = self.login_ui.env.clone();
        let key = self.login_ui.input.text().trim().to_string();
        let mut creds = crate::credentials::Credentials::load();
        let res = if key.is_empty() {
            creds.remove(&provider).map(|_| format!("cleared key for {provider}"))
        } else {
            creds.set(&provider, &key).map(|_| format!("saved key for {provider}"))
        };
        match res {
            Ok(msg) => {
                if provider == "custom" {
                    let url = self.login_ui.url.trim().to_string();
                    self.cfg.ai.provider = "custom".into();
                    self.cfg.ai.base_url = url.clone();
                    let _ = self.cfg.save();
                    let model = self.cfg.ai.model.clone();
                    self.manager.set_ai("custom", &model, &url);
                    crate::ai::discovery::invalidate("custom");
                    if let Some(dir) = self.focused().map(|s| s.dir.clone()) {
                        self.manager.refresh_providers(dir);
                    }
                    self.flash(format!("{msg}, endpoint {url}"));
                } else {
                    // Activate the provider (like the OpenCode CLI does after
                    // `/connect`) so the next prompt uses it without a restart.
                    let model = self.activate_provider(&provider, &env);
                    self.flash(format!("{msg} · {label} active ({model})"));
                }
                self.overlay = Overlay::None;
            }
            Err(e) => self.flash(format!("could not save key: {e}")),
        }
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

    /// Warm the resume cache for a directory (called at boot). Uses an
    /// existing server with a directory override, so listing many folders
    /// never spawns extra servers.
    pub fn preload_sessions(&mut self, dir: PathBuf) {
        let dir = dir.canonicalize().unwrap_or(dir);
        if !dir.is_dir() {
            return;
        }
        if !self.session_cache.contains_key(&dir) {
            let server = self.primary_server_dir();
            self.manager.preload_dir(server, dir);
        }
    }

    /// Re-request the session list for every known directory through the
    /// primary server. Results replace each directory's cache entry, so an
    /// open picker updates live without flicker.
    pub fn preload_known_dirs(&mut self) {
        let dirs: Vec<PathBuf> = self.known_dirs.iter().cloned().collect();
        let server = self.primary_server_dir();
        for d in dirs {
            if d.is_dir() {
                self.manager.preload_dir(server.clone(), d);
            }
        }
    }

    /// The directory whose server we use to enumerate other folders.
    fn primary_server_dir(&self) -> PathBuf {
        self.focused()
            .map(|s| s.dir.clone())
            .unwrap_or_else(|| self.initial_dir.clone())
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

    fn manager_tx(&self) -> tokio::sync::mpsc::UnboundedSender<AppEvent> {
        self.manager.tx()
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
        if let Some(s) = self.session_mut(id) {
            s.model = model.clone();
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
                if self.manager.is_local() {
                    if let Some(oc) = self.session(sid).and_then(|s| s.oc_sid.clone()) {
                        self.manager.local_compact(oc);
                        self.flash("compacting…");
                    } else {
                        self.flash("session is still connecting…");
                    }
                    return;
                }
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
            "undo" => self.open_rewind(sid),
            "redo" => self.redo(sid),
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
            "delete" | "remove" => {
                self.close_session(self.focus);
                self.flash("session deleted from workspace (server history kept)");
            }
            "rename" => {
                if args.is_empty() {
                    // Open the rename prompt pre-filled with the current name.
                    self.rename.input.clear();
                    if let Some((name, len)) =
                        self.focused().map(|s| (s.name.clone(), s.name.chars().count()))
                    {
                        self.rename.input.buf = name;
                        self.rename.input.cursor = len;
                    }
                    self.overlay = Overlay::Rename;
                } else {
                    let name = args.to_string();
                    self.rename_session(sid, &name);
                    self.flash(format!("renamed to {name}"));
                }
            }
            "keys" | "help" => self.open_keymap(),
            "refresh" => self.request_refresh(),
            "tree" => self.open_tree(),
            "editor" => self.open_editor(),
            "export" => self.export_session(if args.is_empty() { None } else { Some(args) }),
            "login" => {
                if args.is_empty() {
                    self.open_login();
                } else {
                    self.login(args);
                }
            }
            "logs" => self.open_logs(),
            "push" => self.start_push(sid, args),
            "fork" => self.fork_active(),
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
            TermEvent::Paste(raw) => {
                if self.viewer.is_some() || self.diff.is_some() {
                    // A full-screen viewer is on top; do not paste into the
                    // session underneath it.
                    self.dirty = true;
                    return;
                }
                if self.apply_overlay_paste(&raw) {
                    return;
                }
                // Keep newlines intact so nothing is submitted mid-paste, and
                // collapse long pastes.
                let text = raw.replace("\r\n", "\n").replace('\r', "\n");
                if let Some(id) = self.focused().map(|s| s.id) {
                    self.add_paste_text(id, &text);
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
                let h = self.pane_view_height();
                let sid = self.focused().map(|s| s.id).unwrap_or(0);
                let total = self.conv_cache.get(&sid).map(|c| c.lines.len()).unwrap_or(0);
                let bottom = total.saturating_sub(h);
                if let Some(s) = self.focused_mut() {
                    s.scroll = s.scroll.saturating_add(4);
                    if s.scroll >= bottom {
                        s.scroll = bottom;
                        s.stick_bottom = true;
                    }
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

        // Escape hatches that must work from ANY state (including a modal that
        // is waiting on a reply) so the UI can never trap the user.
        if ctrl && key.code == KeyCode::Char('c') {
            self.interrupt(self.focus);
            return;
        }
        if ctrl && key.code == KeyCode::Char('q') {
            self.execute(Cmd::Quit).await;
            return;
        }
        if ctrl && key.code == KeyCode::Char('v') {
            // Clipboard paste belongs to the active surface: modal field,
            // viewer guard, or focused chat box.
            self.paste_from_clipboard().await;
            return;
        }

        if self.overlay != Overlay::None {
            self.handle_overlay_key(key).await;
            return;
        }

        // Global bindings from the (user-editable) keymap.
        if let Some(action) = self.keys.action_for(&key) {
            // Directional focus (Ctrl+arrows) moves to the neighbouring pane.
            // Directional pane ops: Alt+arrows focus, Alt+Shift+arrows move,
            // Alt+Ctrl+arrows resize.
            #[derive(Clone, Copy)]
            enum PaneOp {
                Focus,
                Move,
                Resize,
            }
            let op = match action {
                Action::FocusLeft => Some((PaneOp::Focus, Dir::Left)),
                Action::FocusRight => Some((PaneOp::Focus, Dir::Right)),
                Action::FocusUp => Some((PaneOp::Focus, Dir::Up)),
                Action::FocusDown => Some((PaneOp::Focus, Dir::Down)),
                Action::MoveLeft => Some((PaneOp::Move, Dir::Left)),
                Action::MoveRight => Some((PaneOp::Move, Dir::Right)),
                Action::MoveUp => Some((PaneOp::Move, Dir::Up)),
                Action::MoveDown => Some((PaneOp::Move, Dir::Down)),
                Action::ResizeLeft => Some((PaneOp::Resize, Dir::Left)),
                Action::ResizeRight => Some((PaneOp::Resize, Dir::Right)),
                Action::ResizeUp => Some((PaneOp::Resize, Dir::Up)),
                Action::ResizeDown => Some((PaneOp::Resize, Dir::Down)),
                _ => None,
            };
            if let Some((op, dir)) = op {
                let rects = self.layout_rects(self.last_body_area).unwrap_or_default();
                match op {
                    PaneOp::Focus => self.move_focus(dir, &rects),
                    PaneOp::Move => self.swap_pane(dir, &rects),
                    PaneOp::Resize => self.resize_pane(dir),
                }
                return;
            }
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
                Action::Editor => Cmd::Editor,
                Action::FocusLeft
                | Action::FocusRight
                | Action::FocusUp
                | Action::FocusDown
                | Action::MoveLeft
                | Action::MoveRight
                | Action::MoveUp
                | Action::MoveDown
                | Action::ResizeLeft
                | Action::ResizeRight
                | Action::ResizeUp
                | Action::ResizeDown => unreachable!(),
            };
            self.execute(cmd).await;
            return;
        }

        // Arrow family (bound actions): Alt+arrows focus, Alt+Shift+arrows
        // move, Alt+Ctrl+arrows resize. This block keeps the vim-style aliases:
        // Alt+hjkl resize, Alt+Shift+hjkl move, Alt+1..9 focus nth.
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

        // Ctrl+V pastes from the system clipboard (text or an image).
        if ctrl && key.code == KeyCode::Char('v') {
            self.paste_from_clipboard().await;
            return;
        }

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
            // The input box is replaced by the permission prompt, so swallow
            // everything else (no invisible typing) and let Esc reject.
            match key.code {
                KeyCode::Char('a') => {
                    self.permission_reply(sid, "once");
                }
                KeyCode::Char('A') => {
                    self.permission_reply(sid, "always");
                }
                KeyCode::Char('r') | KeyCode::Esc => {
                    self.permission_reply(sid, "reject");
                }
                _ => {}
            }
            return;
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
                let total = self.conv_cache.get(&sid).map(|c| c.lines.len()).unwrap_or(0);
                let bottom = total.saturating_sub(h);
                if let Some(s) = self.session_mut(sid) {
                    s.scroll = s.scroll.saturating_add(h.max(1));
                    if s.scroll >= bottom {
                        s.scroll = bottom;
                        s.stick_bottom = true;
                    }
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
                // Ctrl+Y (not bare `y`, which must stay typable).
                KeyCode::Char('y') if ctrl => {
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

        // `@file` mention popup: navigate, complete, or dismiss.
        let mention_active = self
            .focused()
            .map(|s| !s.mention_results.is_empty())
            .unwrap_or(false);
        if mention_active {
            match key.code {
                KeyCode::Up => {
                    if let Some(s) = self.session_mut(sid) {
                        s.mention_selected = s.mention_selected.saturating_sub(1);
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Down => {
                    if let Some(s) = self.session_mut(sid) {
                        let n = s.mention_results.len();
                        if n > 0 {
                            s.mention_selected = (s.mention_selected + 1).min(n - 1);
                        }
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    if let Some(s) = self.session_mut(sid) {
                        if let Some(label) = s.mention_results.get(s.mention_selected).cloned() {
                            let completed = crate::mentions::complete(&s.input.buf, &label);
                            s.input.buf = completed;
                            s.input.cursor = s.input.buf.chars().count();
                            s.mention_results.clear();
                            s.dirty = true;
                        }
                    }
                    self.dirty = true;
                    return;
                }
                KeyCode::Esc => {
                    if let Some(s) = self.session_mut(sid) {
                        s.mention_results.clear();
                        s.dirty = true;
                    }
                    self.dirty = true;
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
                // Copy the last reply — Ctrl+Y so plain `y` can start a message.
                KeyCode::Char('y') if ctrl => {
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
        // Snapshot what we already have, then refresh every known folder.
        self.newdlg.recent = self.all_cached_sessions();
        self.newdlg.recent_loaded = true;
        self.preload_known_dirs();
        let req = self.manager.next_req();
        self.newdlg.recent_req = Some(req);
        self.manager.list_server_sessions(req, dir.clone());
        self.overlay = Overlay::NewSession;
        self.dirty = true;
    }

    /// Request composing the focused prompt in `$EDITOR` (handled by main).
    pub fn open_editor(&mut self) {
        let Some(s) = self.focused() else { return };
        let text = s.input.buf.clone();
        self.pending_editor = Some((s.id, text));
    }

    /// Take a pending editor request (called by the event loop).
    pub fn take_pending_editor(&mut self) -> Option<(u32, String)> {
        self.pending_editor.take()
    }

    /// Replace a session's input with `text` (after the editor returns).
    pub fn set_input(&mut self, id: u32, text: String) {
        if let Some(s) = self.session_mut(id) {
            s.input.buf = text;
            s.input.cursor = s.input.buf.chars().count();
            s.dirty = true;
        }
        self.dirty = true;
    }

    /// `/login <provider> <key>` stores an API key; `/login <provider>` checks.
    pub fn login(&mut self, args: &str) {
        let mut parts = args.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(provider), Some(key)) => {
                let mut creds = crate::credentials::Credentials::load();
                match creds.set(provider, key) {
                    Ok(()) => match Self::login_env_for(provider) {
                        Some(env) => {
                            let model = self.activate_provider(provider, env);
                            self.flash(format!("saved API key for {provider} · active ({model})"));
                        }
                        None => self.flash(format!("saved API key for {provider}")),
                    },
                    Err(e) => self.flash(format!("could not save key: {e}")),
                }
            }
            (Some(provider), None) => {
                let creds = crate::credentials::Credentials::load();
                let env = Self::login_env_for(provider).unwrap_or("");
                let configured = creds.resolve(provider, env).is_some();
                self.flash(if configured {
                    format!("{provider}: key configured")
                } else {
                    format!("usage: /login {provider} <api-key>")
                });
            }
            _ => self.flash("usage: /login <provider> <api-key>"),
        }
    }

    /// Export the focused session transcript to `path` (Markdown, or JSONL when
    /// the path ends in `.jsonl`).
    pub fn export_session(&mut self, path: Option<&str>) {
        let Some(s) = self.focused() else {
            self.flash("no session");
            return;
        };
        let name = s.name.clone();
        let msgs = s.messages.clone();
        let dir = s.dir.clone();
        if msgs.is_empty() {
            self.flash("nothing to export");
            return;
        }
        let jsonl = path.map(|p| p.ends_with(".jsonl")).unwrap_or(false);
        let target = match path {
            Some(p) if p.trim().is_empty() => dir.join(crate::export::default_filename(&name, jsonl)),
            Some(p) => std::path::PathBuf::from(p),
            None => dir.join(crate::export::default_filename(&name, false)),
        };
        let body = if jsonl {
            crate::export::jsonl(&msgs)
        } else {
            crate::export::markdown(&msgs)
        };
        match std::fs::write(&target, body) {
            Ok(()) => self.flash(format!("exported → {}", target.display())),
            Err(e) => self.flash(format!("export failed: {e}")),
        }
    }

    /// Open the history-tree navigator for the focused (local) session.
    pub fn open_tree(&mut self) {
        let Some(s) = self.focused() else { return };
        let Some(oc) = s.oc_sid.clone() else {
            self.flash("session is still connecting…");
            return;
        };
        self.manager.local_tree(oc);
    }

    /// Build rewind rows (user turns + per-turn diff stats) from a transcript.
    /// `tree_users` are `(entry_id, text)` for the local history tree.
    fn build_rewind_rows(
        msgs: &[Message],
        tree_users: &[(String, String)],
        local: bool,
    ) -> Vec<RewindRow> {
        let user_idx: Vec<usize> = msgs
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::User)
            .map(|(i, _)| i)
            .collect();
        let mut rows = Vec::new();
        for (k, &i) in user_idx.iter().enumerate() {
            let text = msgs[i]
                .parts
                .iter()
                .filter_map(|p| match &p.kind {
                    PartKind::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let end = user_idx.get(k + 1).copied().unwrap_or(msgs.len());
            let (mut adds, mut dels) = (0u32, 0u32);
            let mut files = std::collections::HashSet::new();
            for m in &msgs[i + 1..end] {
                for p in &m.parts {
                    if let PartKind::Tool(t) = &p.kind {
                        if let Some(path) = t.file_path() {
                            files.insert(path);
                        }
                        if let Some(d) = t.diff() {
                            for line in d.lines() {
                                if line.starts_with('+') && !line.starts_with("+++") {
                                    adds += 1;
                                } else if line.starts_with('-') && !line.starts_with("---") {
                                    dels += 1;
                                }
                            }
                        }
                    }
                }
            }
            let (entry, msg_id) = if local {
                (tree_users.get(k).map(|(e, _)| e.clone()), None)
            } else {
                (None, Some(msgs[i].id.clone()))
            };
            rows.push(RewindRow {
                index: i,
                text,
                entry,
                msg_id,
                adds,
                dels,
                files: files.len(),
            });
        }
        rows
    }

    /// Open the agy-style rewind picker for session `sid`.
    pub fn open_rewind(&mut self, sid: u32) {
        let Some((oc, local)) = self
            .session(sid)
            .map(|s| (s.oc_sid.clone(), self.manager.is_local()))
        else {
            return;
        };
        let Some(oc) = oc else {
            self.flash("session is still connecting…");
            return;
        };
        // Local sessions: map each user turn to its history-tree entry (the
        // active path preserves transcript order, so index alignment holds).
        let tree_users: Vec<(String, String)> = if local {
            self.manager
                .local_tree_snapshot(&oc)
                .map(|t| {
                    t.active_path()
                        .into_iter()
                        .filter(|e| e.kind == crate::tree::EntryKind::User)
                        .map(|e| (e.id.clone(), e.text.clone()))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let msgs = self
            .session(sid)
            .map(|s| s.messages.clone())
            .unwrap_or_default();
        // Entry ids only map 1:1 when the tree and transcript agree.
        let aligned: Vec<(String, String)> = if local
            && tree_users.len()
                != msgs.iter().filter(|m| m.role == Role::User).count()
        {
            Vec::new()
        } else {
            tree_users
        };
        let rows = Self::build_rewind_rows(&msgs, &aligned, local);
        if rows.is_empty() {
            self.flash("nothing to rewind");
            return;
        }
        let selected = rows.len() - 1;
        self.rewind_ui = RewindState { rows, selected };
        self.overlay = Overlay::Rewind;
        self.dirty = true;
    }

    /// Apply the highlighted rewind: drop the turn's output, put the prompt
    /// back in the chatbox, and move the local history leaf before it.
    pub fn rewind_selected(&mut self) {
        let sid = self.focus;
        let Some(row) = self.rewind_ui.rows.get(self.rewind_ui.selected).cloned() else {
            return;
        };
        let Some((dir, oc, local)) = self
            .session(sid)
            .map(|s| (s.dir.clone(), s.oc_sid.clone(), self.manager.is_local()))
        else {
            return;
        };
        let Some(oc) = oc else { return };
        if local {
            let Some(entry) = &row.entry else {
                self.flash("history tree out of sync — use /tree");
                return;
            };
            self.manager.local_rewind(oc, entry.clone());
        } else if let Some(mid) = &row.msg_id {
            self.manager.revert(dir, oc, mid.clone());
        }
        if let Some(s) = self.session_mut(sid) {
            let removed: Vec<Message> = s.messages.split_off(row.index.min(s.messages.len()));
            s.redo_snapshot = Some(removed);
            s.input.clear();
            s.input.buf = row.text.clone();
            s.input.cursor = s.input.buf.chars().count();
            s.status = SessStatus::Idle;
            s.stick_bottom = true;
            s.dirty = true;
        }
        self.overlay = Overlay::None;
        let first: String = row.text.lines().next().unwrap_or("").chars().take(48).collect();
        self.flash(format!("rewound: {first}"));
        self.dirty = true;
    }

    /// `/redo`: restore the turn dropped by the last local rewind.
    pub fn redo(&mut self, sid: u32) {
        if !self.manager.is_local() {
            let req = {
                let Some(s) = self.session(sid) else { return };
                s.oc_sid.clone().map(|oc| (s.dir.clone(), oc))
            };
            if let Some((dir, oc)) = req {
                self.manager.unrevert(dir, oc);
            }
            return;
        }
        let Some((oc, removed)) = self
            .session(sid)
            .map(|s| (s.oc_sid.clone(), s.redo_snapshot.clone()))
        else {
            return;
        };
        let (Some(oc), Some(removed)) = (oc, removed) else {
            self.flash("nothing to redo");
            return;
        };
        if self.manager.local_redo(oc) {
            if let Some(s) = self.session_mut(sid) {
                s.messages.extend(removed);
                s.redo_snapshot = None;
                s.recompute_metrics();
                s.stick_bottom = true;
                s.dirty = true;
            }
            self.flash("redone");
            self.dirty = true;
        } else {
            self.flash("nothing to redo");
        }
    }

    /// Flatten a session tree into indented rows (depth-first).
    fn tree_rows(tree: &crate::tree::SessionTree) -> Vec<TreeRow> {
        use std::collections::HashSet;
        let active: HashSet<String> = tree.active_path().iter().map(|e| e.id.clone()).collect();
        fn label(e: &crate::tree::Entry) -> String {
            let t: String = e.text.lines().next().unwrap_or("").chars().take(48).collect();
            match e.kind {
                crate::tree::EntryKind::User => format!("you: {t}"),
                crate::tree::EntryKind::Assistant => format!("ai: {t}"),
                crate::tree::EntryKind::Tool => format!("tool: {t}"),
                crate::tree::EntryKind::System => format!("sys: {t}"),
                crate::tree::EntryKind::Compaction => format!("⊟ summary ({t})"),
                crate::tree::EntryKind::BranchSummary => format!("↳ branch ({t})"),
            }
        }
        let mut rows = Vec::new();
        fn walk(
            tree: &crate::tree::SessionTree,
            parent: &str,
            depth: usize,
            active: &HashSet<String>,
            rows: &mut Vec<TreeRow>,
        ) {
            for e in tree.children(parent) {
                rows.push(TreeRow {
                    id: e.id.clone(),
                    depth,
                    label: label(e),
                    kind: e.kind,
                    active: active.contains(&e.id),
                });
                walk(tree, &e.id, depth + 1, active, rows);
            }
        }
        for r in tree.roots() {
            rows.push(TreeRow {
                id: r.id.clone(),
                depth: 0,
                label: label(r),
                kind: r.kind,
                active: active.contains(&r.id),
            });
            walk(tree, &r.id, 1, &active, &mut rows);
        }
        rows
    }

    pub fn open_resume_picker(&mut self) {
        let dir = self
            .focused()
            .map(|s| s.dir.clone())
            .unwrap_or_else(|| self.initial_dir.clone());
        self.remember_dir(&dir);
        self.resume_picker = ResumePickerState::default();
        // The local backend keeps sessions as on-disk tree sidecars.
        if self.manager.is_local() {
            let dir_s = dir.to_string_lossy().to_string();
            self.resume_picker.items = crate::tree::SessionTree::list_sessions()
                .into_iter()
                .map(|s| crate::models::OcSession {
                    id: s.id,
                    title: s.title,
                    directory: s.directory.unwrap_or_else(|| dir_s.clone()),
                    updated_ms: Some(s.updated_ms),
                })
                .collect();
            self.resume_picker.loaded = true;
            self.overlay = Overlay::ResumeSession;
            self.dirty = true;
            return;
        }
        // Show every known session first; the refresh below fills in the rest.
        self.resume_picker.items = self.all_cached_sessions();
        self.resume_picker.loaded = true;
        self.preload_known_dirs();
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
        if let Some(msgs) = crate::tree::SessionTree::sidecar_path(&oc_sid)
            .and_then(|p| crate::tree::SessionTree::load(&p))
            .map(|tree| tree.to_messages(&oc_sid))
            .filter(|m| !m.is_empty())
        {
            sess.messages = msgs;
            sess.recompute_metrics();
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
        let sid = s.id;
        self.dirty = true;
        // Keep the `@file` suggestion list in sync with the input word.
        self.refresh_mentions(sid).await;
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
            Overlay::Tree => {
                let n = self.tree_ui.items.len();
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.tree_ui.selected = self.tree_ui.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if n > 0 {
                            self.tree_ui.selected = (self.tree_ui.selected + 1).min(n - 1);
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(row) = self.tree_ui.items.get(self.tree_ui.selected).cloned() {
                            if let Some(oc) = self.focused().and_then(|s| s.oc_sid.clone()) {
                                self.manager.local_navigate(oc, row.id);
                            }
                        }
                        self.overlay = Overlay::None;
                    }
                    _ => {}
                }
            }
            Overlay::Rewind => match key.code {
                KeyCode::Esc => self.overlay = Overlay::None,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.rewind_ui.selected = self.rewind_ui.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = self.rewind_ui.rows.len();
                    if n > 0 {
                        self.rewind_ui.selected = (self.rewind_ui.selected + 1).min(n - 1);
                    }
                }
                KeyCode::PageUp => {
                    self.rewind_ui.selected = self.rewind_ui.selected.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    let n = self.rewind_ui.rows.len();
                    if n > 0 {
                        self.rewind_ui.selected = (self.rewind_ui.selected + 10).min(n - 1);
                    }
                }
                KeyCode::Enter => self.rewind_selected(),
                _ => {}
            },
            Overlay::Login => match self.login_ui.stage {
                LoginStage::Choose => match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        let n = self.login_ui.providers.len();
                        if n > 0 {
                            self.login_ui.selected =
                                if self.login_ui.selected == 0 { n - 1 } else { self.login_ui.selected - 1 };
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let n = self.login_ui.providers.len();
                        if n > 0 {
                            self.login_ui.selected = (self.login_ui.selected + 1) % n;
                        }
                    }
                    KeyCode::Enter => self.login_choose(),
                    _ => {}
                },
                LoginStage::Url => match key.code {
                    KeyCode::Esc => {
                        self.login_ui.stage = LoginStage::Choose;
                        self.login_ui.input.clear();
                    }
                    KeyCode::Enter => {
                        let url = self.login_ui.input.text().trim().to_string();
                        if url.is_empty() {
                            self.flash("enter a base URL like https://host/v1");
                        } else {
                            self.login_ui.url = url;
                            self.login_ui.input.clear();
                            self.login_ui.stage = LoginStage::Key;
                        }
                    }
                    KeyCode::Backspace => self.login_ui.input.backspace(),
                    KeyCode::Left => self.login_ui.input.left(),
                    KeyCode::Right => self.login_ui.input.right(),
                    KeyCode::Home => self.login_ui.input.home(),
                    KeyCode::End => self.login_ui.input.end(),
                    KeyCode::Char(c) if !ctrl => {
                        self.login_ui.input.insert(&c.to_string());
                    }
                    _ => {}
                },
                LoginStage::Key => match key.code {
                    KeyCode::Esc => {
                        self.login_ui.stage = LoginStage::Choose;
                        self.login_ui.input.clear();
                    }
                    KeyCode::Enter => self.login_save(),
                    KeyCode::Backspace => self.login_ui.input.backspace(),
                    KeyCode::Left => self.login_ui.input.left(),
                    KeyCode::Right => self.login_ui.input.right(),
                    KeyCode::Home => self.login_ui.input.home(),
                    KeyCode::End => self.login_ui.input.end(),
                    KeyCode::Char(c) if !ctrl => {
                        self.login_ui.input.insert(&c.to_string());
                    }
                    _ => {}
                },
            },
            Overlay::Logs => {
                let max = self.log_view.lines.len().saturating_sub(1);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.log_view.follow = false;
                        self.log_view.scroll = self.log_view.scroll.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.log_view.scroll = (self.log_view.scroll + 1).min(max);
                        if self.log_view.scroll == max {
                            self.log_view.follow = true;
                        }
                    }
                    KeyCode::PageUp => {
                        self.log_view.follow = false;
                        self.log_view.scroll = self.log_view.scroll.saturating_sub(20);
                    }
                    KeyCode::PageDown => {
                        self.log_view.scroll = (self.log_view.scroll + 20).min(max);
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.log_view.follow = true;
                        self.log_view.scroll = max;
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.log_view.follow = false;
                        self.log_view.scroll = 0;
                    }
                    KeyCode::Char('f') => {
                        self.log_view.follow = !self.log_view.follow;
                        if self.log_view.follow {
                            self.log_view.scroll = max;
                        }
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
            Cmd::Refresh => self.request_refresh(),
            Cmd::Tree => self.open_tree(),
            Cmd::Editor => self.open_editor(),
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
                let targets: Vec<String> = self
                    .sessions
                    .iter()
                    .filter(|s| s.dir == dir && s.oc_sid.is_some())
                    .filter_map(|s| s.oc_sid.clone())
                    .collect();
                for oc in targets {
                    self.handle_harness_event(
                        dir.clone(),
                        oc,
                        HarnessEvent::ProviderDisconnected,
                    );
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
            AppEvent::Harness { dir, oc_sid, event } => {
                self.handle_harness_event(dir, oc_sid, event)
            }
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
                self.flash(error.clone());
                self.handle_harness_event(dir, oc_sid, HarnessEvent::ProviderError(error));
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
            AppEvent::PushProgress { session, text } => {
                if let Some(s) = self.session_mut(session) {
                    s.activity = Some(Activity {
                        text,
                        started: Instant::now(),
                        done: false,
                    });
                    s.dirty = true;
                }
            }
            AppEvent::PushDone {
                session,
                ok,
                message,
                repo,
                subject,
            } => {
                let text = if ok {
                    let repo = repo.unwrap_or_else(|| "remote".into());
                    if subject.trim().is_empty() {
                        format!("Git pushed {repo}")
                    } else {
                        format!("Git pushed \"{subject}\" {repo}")
                    }
                } else {
                    format!("push failed: {message}")
                };
                if let Some(s) = self.session_mut(session) {
                    s.activity = Some(Activity {
                        text,
                        started: Instant::now(),
                        done: true,
                    });
                    s.dirty = true;
                }
                // No flash: the activity strip is the single place push status
                // is shown, so it isn't duplicated in the status bar.
            }
            AppEvent::OcForked { dir, session, source } => {
                // The pane was already created instantly in `begin_fork`; just
                // wire the provider session id and connect to load history.
                let Some(pos) = self.fork_wait.iter().position(|(s, _)| *s == source) else {
                    return;
                };
                let new_id = self.fork_wait.remove(pos).1;
                let dir_c = dir.canonicalize().unwrap_or_else(|_| dir.clone());
                self.remember_dir(&dir_c);
                let (name, target_dir) = {
                    let Some(s) = self.session_mut(new_id) else {
                        return;
                    };
                    s.oc_sid = Some(session.id.clone());
                    s.status = SessStatus::Connecting;
                    let server_dir = PathBuf::from(&session.directory);
                    if !session.directory.is_empty() && server_dir != s.dir {
                        if let Ok(c) = server_dir.canonicalize() {
                            s.dir = c;
                        }
                    }
                    s.dirty = true;
                    (s.name.clone(), s.dir.clone())
                };
                let req = self.manager.next_req();
                self.pending_create.insert(new_id, req);
                self.manager.connect_session(
                    req,
                    target_dir,
                    name,
                    Some(session.id.clone()),
                    None,
                    self.cfg.behavior.history_limit,
                );
                self.focus = new_id;
                self.dirty = true;
                self.save_workspace();
            }
            AppEvent::OcForkFailed { source, error } => {
                if let Some(pos) = self.fork_wait.iter().position(|(s, _)| *s == source) {
                    let (_, new_id) = self.fork_wait.remove(pos);
                    if let Some(s) = self.session_mut(new_id) {
                        s.last_error = Some(error.clone());
                        s.status = SessStatus::Error(error.clone());
                        s.dirty = true;
                    }
                }
                self.flash(error);
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
            AppEvent::ShellDone {
                session: id,
                command,
                ok,
                output,
                send_to_agent,
            } => {
                if let Some(s) = self.session_mut(id) {
                    let header = format!("▌ shell · $ {command}\n\n");
                    let msg = crate::harness::transcript::Message {
                        id: format!("shell-{}", s.optimistic_seq()),
                        role: crate::harness::transcript::Role::Assistant,
                        error: if ok { None } else { Some("command failed".into()) },
                        completed: Some(1),
                        created: None,
                        cost: None,
                        tokens: None,
                        parts: vec![crate::harness::transcript::Part {
                            id: format!("shell-{}-out", s.optimistic_seq()),
                            message_id: format!("shell-{command}"),
                            kind: crate::harness::transcript::PartKind::Text {
                                text: format!("{header}{output}"),
                                synthetic: false,
                            },
                        }],
                    };
                    s.messages.push(msg);
                    s.status = SessStatus::Idle;
                    s.dirty = true;
                } else {
                    return;
                }
                if send_to_agent && !output.trim().is_empty() {
                    self.send_text_now(id, &output);
                }
                self.dirty = true;
            }
            AppEvent::TreeLoaded { oc_sid, tree } => {
                let _ = oc_sid;
                let rows = Self::tree_rows(&tree);
                if rows.is_empty() {
                    self.flash("no history recorded yet");
                } else {
                    let active = rows.iter().position(|r| r.active).unwrap_or(0);
                    self.tree_ui.items = rows;
                    self.tree_ui.selected = active;
                    self.overlay = Overlay::Tree;
                }
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
                            // Never syntax-highlight an unbounded file on the
                            // event loop — large generated/lock files would
                            // freeze the whole UI for a long time.
                            const MAX_VIEW_BYTES: usize = 256 * 1024;
                            const MAX_VIEW_LINES: usize = 4000;
                            let total_lines = text.lines().count();
                            let truncated =
                                text.len() > MAX_VIEW_BYTES || total_lines > MAX_VIEW_LINES;
                            let display = if truncated {
                                let mut end = text.len().min(MAX_VIEW_BYTES);
                                while end > 0 && !text.is_char_boundary(end) {
                                    end -= 1;
                                }
                                text[..end]
                                    .lines()
                                    .take(MAX_VIEW_LINES)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            } else {
                                text.clone()
                            };
                            let guard = crate::highlight::get();
                            let hl = guard.as_ref().map(|(_, h)| h).expect("highlighter");
                            let mut lines = hl
                                .highlight_file(&path, &display)
                                .into_iter()
                                .map(Line::from)
                                .collect::<Vec<_>>();
                            if truncated {
                                lines.push(Line::from(Span::styled(
                                    format!(
                                        "… truncated for display (first {} of {} lines)",
                                        lines.len().min(MAX_VIEW_LINES),
                                        total_lines
                                    ),
                                    theme::dim(),
                                )));
                            }
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

    /// Handle a provider-neutral event. Session lifecycle, status,
    /// permissions and questions live here; the transcript stays on the raw
    /// OpenCode event path until the model types are moved into the harness.
    fn handle_harness_event(&mut self, dir: PathBuf, oc_sid: String, event: HarnessEvent) {
        // Directory-scoped events that don't belong to a single session.
        match event {
            HarnessEvent::FilesChanged => {
                self.explorer.dirty = true;
                self.git_cache.invalidate(&dir);
                self.dirty = true;
                return;
            }
            HarnessEvent::BranchChanged => {
                self.git_cache.invalidate(&dir);
                self.dirty = true;
                return;
            }
            _ => {}
        }
        if oc_sid.is_empty() {
            return;
        }
        let Some(idx) = Self::route_session(&self.sessions, &dir, &oc_sid) else {
            crate::tlog!("APP no route oc_sid={oc_sid} dir={}", dir.display());
            return;
        };
        let sid = self.sessions[idx].id;

        match event {
            HarnessEvent::FilesChanged | HarnessEvent::BranchChanged => {}
            HarnessEvent::Transcript(update) => {
                let s = &mut self.sessions[idx];
                let mut dirty = true;
                match update {
                    TranscriptUpdate::MessageMeta(msg) => {
                        if msg.role == Role::User {
                            // message.updated carries no parts; adopt by FIFO
                            // against optimistic sends *before* inserting, so
                            // the adopted row is updated rather than duplicated.
                            s.adopt_oldest(&msg);
                        }
                        s.upsert_message_meta(&msg);
                    }
                    TranscriptUpdate::Part(part) => {
                        let msg_id = part.message_id.clone();
                        let existing_role = s
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
                        s.upsert_part(&meta, part);
                    }
                    TranscriptUpdate::PartRemoved {
                        message_id,
                        part_id,
                    } => {
                        s.remove_part(&message_id, &part_id);
                    }
                    TranscriptUpdate::Reset => {
                        s.messages.clear();
                        s.recompute_metrics();
                    }
                    TranscriptUpdate::ReplaceAll(msgs) => {
                        if s.messages != msgs {
                            s.replace_history(msgs);
                            if s.status == SessStatus::Connecting {
                                s.status = SessStatus::Idle;
                            }
                        } else {
                            if s.status == SessStatus::Connecting {
                                s.status = SessStatus::Idle;
                            }
                            dirty = false;
                        }
                    }
                }
                if dirty {
                    s.dirty = true;
                }
            }
            HarnessEvent::SessionIdle => {
                let has_queue = !self.sessions[idx].queue.is_empty();
                {
                    let s = &mut self.sessions[idx];
                    if !matches!(
                        s.status,
                        SessStatus::Error(_) | SessStatus::Permission | SessStatus::Question
                    ) {
                        s.status = SessStatus::Idle;
                    }
                    s.interrupt_armed = None;
                    s.dirty = true;
                }
                self.finish_task(idx, TaskStatus::Completed);
                if has_queue {
                    self.drain_queue(sid);
                }
            }
            HarnessEvent::SessionWorking => {
                let s = &mut self.sessions[idx];
                if !matches!(s.status, SessStatus::Permission | SessStatus::Question) {
                    s.status = SessStatus::Working;
                }
                s.dirty = true;
            }
            HarnessEvent::SessionThinking => {
                let s = &mut self.sessions[idx];
                if !matches!(s.status, SessStatus::Permission | SessStatus::Question) {
                    s.status = SessStatus::Thinking;
                }
                s.dirty = true;
            }
            HarnessEvent::SessionRetrying(msg) => {
                self.sessions[idx].status = SessStatus::Retrying(msg);
                self.sessions[idx].dirty = true;
            }
            HarnessEvent::SessionError(msg) => {
                let s = &mut self.sessions[idx];
                s.last_error = Some(msg.clone());
                s.status = SessStatus::Error(msg);
                s.dirty = true;
                self.finish_task(idx, TaskStatus::Failed);
            }
            HarnessEvent::SessionInterrupted => {
                let s = &mut self.sessions[idx];
                s.status = SessStatus::Idle;
                s.interrupt_armed = None;
                s.dirty = true;
                self.finish_task(idx, TaskStatus::Interrupted);
            }
            HarnessEvent::PermissionAsked { id, kind, detail } => {
                if let Some(t) = self.sessions[idx].task.as_mut() {
                    t.wait();
                }
                let auto = self.cfg.behavior.auto_approve_permissions;
                let dir = self.sessions[idx].dir.clone();
                let oc = self.sessions[idx].oc_sid.clone();
                if auto {
                    if let Some(oc) = oc {
                        if self.manager.is_local() {
                            self.manager.local_permission_reply(oc, id.clone(), "once".into());
                        } else {
                            self.manager.reply_permission(dir, oc, id.clone(), "once".into());
                        }
                    }
                }
                let s = &mut self.sessions[idx];
                s.pending_perm = if auto {
                    None
                } else {
                    Some(PendingPermission { id, kind, detail })
                };
                s.status = if auto {
                    SessStatus::Working
                } else {
                    SessStatus::Permission
                };
                s.dirty = true;
            }
            HarnessEvent::PermissionReplied => {
                let s = &mut self.sessions[idx];
                if s.pending_perm.is_some() {
                    s.pending_perm = None;
                    s.status = SessStatus::Working;
                    s.dirty = true;
                }
            }
            HarnessEvent::QuestionAsked(prompt) => {
                if let Some(t) = self.sessions[idx].task.as_mut() {
                    t.wait();
                }
                self.sessions[idx].pending_question =
                    Some(PendingQuestion::new(prompt.id, prompt.questions));
                self.sessions[idx].status = SessStatus::Question;
                self.sessions[idx].dirty = true;
                if self.overlay == Overlay::None || self.overlay == Overlay::Question {
                    self.focus = sid;
                    self.overlay = Overlay::Question;
                }
            }
            HarnessEvent::QuestionReplied => {
                let s = &mut self.sessions[idx];
                s.pending_question = None;
                if s.status == SessStatus::Question {
                    s.status = SessStatus::Working;
                }
                s.dirty = true;
                self.open_pending_question();
            }
            HarnessEvent::AssistantFinished => {
                // Notify (debounced) when the finishing session is unfocused.
                if self.focus != sid {
                    if self.notifications.should_notify() {
                        let name = self.sessions[idx].name.clone();
                        self.flash(format!("{name}: agent finished"));
                        if self.cfg.behavior.notify {
                            notify_desktop("Theta", &format!("{name}: agent finished"));
                        }
                    } else {
                        crate::tlog!(
                            "notification debounced ({} grouped)",
                            self.notifications.suppressed()
                        );
                    }
                }
            }
            HarnessEvent::CompactionStarted => {
                let s = &mut self.sessions[idx];
                s.status = SessStatus::Compacting;
                s.dirty = true;
            }
            HarnessEvent::CompactionFinished { .. } => {
                let s = &mut self.sessions[idx];
                if s.status == SessStatus::Compacting {
                    s.status = SessStatus::Working;
                }
                s.dirty = true;
            }
            HarnessEvent::ProviderError(msg) => {
                let s = &mut self.sessions[idx];
                s.last_error = Some(msg.clone());
                s.status = SessStatus::Error(msg);
                s.dirty = true;
            }
            HarnessEvent::ProviderDisconnected => {
                let s = &mut self.sessions[idx];
                s.status = SessStatus::Error("provider disconnected".into());
                s.dirty = true;
            }
            // Activity is derived from the transcript cache; unknown/provider
            // specific events are intentionally ignored here.
            HarnessEvent::ToolStarted { .. }
            | HarnessEvent::ToolFinished { .. }
            | HarnessEvent::ProviderSpecific { .. } => {}
        }
    }

    /// Find the Theta session a provider event belongs to, keyed by both the
    /// working directory and the provider session id, so events never leak
    /// between projects or panes.
    fn route_session(sessions: &[SessionState], dir: &Path, oc_sid: &str) -> Option<usize> {
        // An empty directory acts as a wildcard: in-process backends (the local
        // loop) emit events without a workspace, and the session id is enough.
        let wildcard = dir.as_os_str().is_empty();
        sessions.iter().position(|s| {
            (wildcard || s.dir == dir) && s.oc_sid.as_deref() == Some(oc_sid)
        })
    }

    fn finish_task(&mut self, idx: usize, status: TaskStatus) {
        if let Some(t) = self.sessions[idx].task.as_mut() {
            if t.finish(status) {
                crate::tlog!(
                    "TASK {} '{}' provider={} workspace={} session={} created={:?} -> {:?}",
                    t.id,
                    t.title,
                    t.provider.label(),
                    t.workspace.display(),
                    t.session.get(),
                    t.created,
                    status
                );
            }
        }
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
            // Let a finished `/push` status linger briefly, then clear it.
            if let Some(a) = &s.activity {
                if a.done && a.started.elapsed() > Duration::from_secs(6) {
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

        // Keep the `/logs` tail live.
        if self.overlay == Overlay::Logs && self.tick % 10 == 0 {
            self.refresh_logs();
        }

        if self.last_save.elapsed() > Duration::from_secs(30) {
            self.save_workspace();
        }
        self.persist_scroll_if_changed();

        // Keep animating while any workspace is active (slider) or a push is
        // in flight (its progress bar needs continuous redraws).
        let pushing = self
            .sessions
            .iter()
            .any(|s| s.activity.as_ref().map(|a| !a.done).unwrap_or(false));
        if self.active_count() > 0 || pushing {
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

#[cfg(test)]
mod harness_tests {
    use super::*;
    use crate::harness::transcript::{Message, Role};

    fn msg(id: &str, role: Role, completed: Option<i64>) -> Message {
        Message {
            id: id.into(),
            role,
            error: None,
            completed,
            created: None,
            cost: None,
            tokens: None,
            parts: Vec::new(),
        }
    }

    fn text_msg(id: &str, role: Role, completed: Option<i64>, body: &str) -> Message {
        let mut m = msg(id, role, completed);
        m.parts.push(crate::harness::transcript::Part {
            id: format!("{id}-p"),
            message_id: id.into(),
            kind: crate::harness::transcript::PartKind::Text {
                text: body.into(),
                synthetic: false,
            },
        });
        m
    }

    #[test]
    fn rewind_rows_carry_diff_stats_and_entry_mapping() {
        use crate::harness::transcript::{Part, PartKind, ToolInfo, ToolStatus};
        let mut u1 = text_msg("msg_u1", Role::User, Some(1), "first request");
        u1.parts.clear();
        u1.parts.push(Part {
            id: "u1p".into(),
            message_id: "msg_u1".into(),
            kind: PartKind::Text { text: "first request".into(), synthetic: false },
        });
        let mut a1 = text_msg("msg_a1", Role::Assistant, Some(2), "did it");
        a1.parts.push(Part {
            id: "t1".into(),
            message_id: "msg_a1".into(),
            kind: PartKind::Tool(ToolInfo {
                tool: "edit".into(),
                call_id: "c1".into(),
                status: ToolStatus::Completed,
                title: Some("edit".into()),
                input: serde_json::json!({"path": "src/lib.rs"}),
                output: Some("ok".into()),
                error: None,
                metadata: serde_json::json!({
                    "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n+added one\n+added two\n-removed one\n",
                    "path": "src/lib.rs"
                }),
                start_ms: None,
            }),
        });
        let u2 = text_msg("msg_u2", Role::User, Some(3), "second request");
        let msgs = vec![u1, a1, u2];

        let tree_users = vec![("e2".to_string(), "first request".to_string())];
        let rows = App::build_rewind_rows(&msgs, &tree_users, true);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "first request");
        assert_eq!(rows[0].adds, 2);
        assert_eq!(rows[0].dels, 1);
        assert_eq!(rows[0].files, 1);
        assert_eq!(rows[0].entry.as_deref(), Some("e2"));
        assert_eq!(rows[0].index, 0);
        assert_eq!(rows[1].text, "second request");
        assert_eq!(rows[1].adds, 0);
        assert_eq!(rows[1].index, 2);

        // Non-local rows expose the message id for OpenCode revert.
        let rows = App::build_rewind_rows(&msgs, &[], false);
        assert_eq!(rows[0].msg_id.as_deref(), Some("msg_u1"));
        assert!(rows[0].entry.is_none());
    }

    #[test]
    fn fork_point_includes_the_newest_message() {
        let mut s = SessionState::new(1, "x".into(), std::path::PathBuf::from("/tmp"));
        s.messages.push(text_msg("msg_u1", Role::User, None, "hi"));
        s.messages.push(text_msg("msg_a1", Role::Assistant, Some(10), "the answer"));
        // trailing in-progress turn: still the end of the conversation
        s.messages.push(msg("msg_a2", Role::Assistant, None));
        assert_eq!(App::fork_point(&s).as_deref(), Some("msg_a2"));
    }

    #[test]
    fn fork_point_includes_latest_output_even_mid_turn() {
        // A question answer (tool output) lives in the last assistant message,
        // which may still be streaming; the fork must include it.
        let mut s = SessionState::new(1, "x".into(), std::path::PathBuf::from("/tmp"));
        s.messages.push(text_msg("msg_u1", Role::User, None, "ask me"));
        let mut tool_msg = msg("msg_a1", Role::Assistant, None);
        tool_msg
            .parts
            .push(crate::harness::transcript::Part {
                id: "p-tool".into(),
                message_id: "msg_a1".into(),
                kind: crate::harness::transcript::PartKind::Tool(
                    crate::harness::transcript::ToolInfo {
                        tool: "ask".into(),
                        call_id: "c".into(),
                        status: crate::harness::transcript::ToolStatus::Completed,
                        title: Some("Ask".into()),
                        input: serde_json::json!({}),
                        output: Some("A".into()),
                        error: None,
                        metadata: serde_json::json!({}),
                        start_ms: None,
                    },
                ),
            });
        s.messages.push(tool_msg);
        assert_eq!(App::fork_point(&s).as_deref(), Some("msg_a1"));
    }

    #[test]
    fn fork_point_none_for_only_optimistic_messages() {
        let mut s = SessionState::new(1, "x".into(), std::path::PathBuf::from("/tmp"));
        s.messages.push(msg("local-1", Role::User, None));
        assert_eq!(App::fork_point(&s), None);
    }

    #[test]
    fn fork_point_skips_optimistic_local_ids() {
        let mut s = SessionState::new(1, "x".into(), std::path::PathBuf::from("/tmp"));
        s.messages.push(msg("local-1", Role::User, None));
        s.messages.push(text_msg("msg_a1", Role::Assistant, Some(1), "done"));
        s.messages.push(msg("local-2", Role::User, None));
        assert_eq!(App::fork_point(&s).as_deref(), Some("msg_a1"));
    }

    #[test]
    fn events_route_by_dir_and_session_id() {
        let mk = |id: u32, dir: &str, oc: &str| {
            let mut s = SessionState::new(id, format!("s{id}"), PathBuf::from(dir));
            s.oc_sid = Some(oc.to_string());
            s
        };
        let sessions = vec![
            mk(1, "/projA", "a1"),
            mk(2, "/projA", "a2"),
            mk(3, "/projB", "b1"),
            mk(4, "/projB", "b2"),
        ];
        assert_eq!(App::route_session(&sessions, Path::new("/projA"), "a2"), Some(1));
        assert_eq!(App::route_session(&sessions, Path::new("/projB"), "b1"), Some(2));
        assert_eq!(App::route_session(&sessions, Path::new("/projA"), "b1"), None);
        assert_eq!(App::route_session(&sessions, Path::new("/projC"), "a1"), None);
    }

    fn paste_test_app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = Config::default();
        let manager = Manager::new(tx, cfg.clone());
        App::new(cfg, manager, PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn terminal_paste_targets_new_session_directory() {
        let mut app = paste_test_app();
        app.overlay = Overlay::NewSession;
        app.newdlg.field = 1;
        app.newdlg.dir = String::from("/home/bhanu/");

        app.handle_term_event(TermEvent::Paste("/tmp/pasted path/\n".into()))
            .await;

        assert_eq!(app.newdlg.dir, "/home/bhanu//tmp/pasted path/");
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn terminal_paste_collapses_new_session_name_line_breaks() {
        let mut app = paste_test_app();
        app.overlay = Overlay::NewSession;
        app.newdlg.field = 0;

        app.handle_term_event(TermEvent::Paste("New\nSession".into()))
            .await;

        assert_eq!(app.newdlg.name.text(), "New Session");
        assert!(app.sessions.is_empty());
    }

    #[tokio::test]
    async fn terminal_paste_does_not_leak_into_background_chat() {
        let mut app = paste_test_app();
        app.sessions.push(SessionState::new(
            10,
            "background".into(),
            PathBuf::from("/tmp"),
        ));
        app.focus = 10;
        app.overlay = Overlay::BusyChoice;

        app.handle_term_event(TermEvent::Paste("leftover".into())).await;

        assert_eq!(app.session(10).expect("session").input.text(), "");
    }

    #[tokio::test]
    async fn replace_all_transcript_update_is_atomic_and_idempotent() {
        let mut app = paste_test_app();
        let mut sess = SessionState::new(1, "s".into(), PathBuf::from("/tmp"));
        sess.oc_sid = Some("oc-1".into());
        app.sessions.push(sess);

        let m1 = text_msg("m1", Role::User, Some(10), "hello");
        let m2 = text_msg("m2", Role::Assistant, Some(20), "world");
        let msgs = vec![m1, m2];

        // Initial ReplaceAll populates messages and marks session dirty.
        app.handle_event(AppEvent::Harness {
            dir: PathBuf::from("/tmp"),
            oc_sid: "oc-1".into(),
            event: HarnessEvent::Transcript(TranscriptUpdate::ReplaceAll(msgs.clone())),
        }).await;

        let s = app.session(1).unwrap();
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].id, "m1");
        assert_eq!(s.messages[1].id, "m2");

        // Clear dirty flag (as render would do)
        app.session_mut(1).unwrap().dirty = false;

        // An identical ReplaceAll must be a zero-cost no-op: session stays NOT dirty, no redraw.
        app.handle_event(AppEvent::Harness {
            dir: PathBuf::from("/tmp"),
            oc_sid: "oc-1".into(),
            event: HarnessEvent::Transcript(TranscriptUpdate::ReplaceAll(msgs)),
        }).await;

        let s = app.session(1).unwrap();
        assert!(!s.dirty, "identical ReplaceAll must not set dirty flag (no flicker)");
    }

    #[tokio::test]
    async fn scrolling_down_to_bottom_restores_stick_bottom() {
        let mut app = paste_test_app();
        let mut sess = SessionState::new(1, "s".into(), PathBuf::from("/tmp"));
        sess.stick_bottom = false;
        sess.scroll = 20;
        app.sessions.push(sess);
        app.focus = 1;

        let cache = crate::ui::conversation::Cache {
            width: 80,
            lines: (0..50).map(|_| ratatui::text::Line::from("line")).collect(),
            blocks: Vec::new(),
            animating: false,
            built_at_tick: 0,
        };
        app.conv_cache.insert(1, cache);
        app.last_body_area = ratatui::layout::Rect::new(0, 0, 80, 20);

        let h = app.pane_view_height();
        let bottom = 50usize.saturating_sub(h);

        // Scroll down past bottom
        app.session_mut(1).unwrap().scroll = bottom + 10;
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 10,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::empty(),
        });

        let s = app.session(1).unwrap();
        assert!(s.stick_bottom, "stick_bottom must be restored when scrolling to bottom");
        assert_eq!(s.scroll, bottom, "scroll must be clamped to bottom");
    }

    #[test]
    fn session_tree_loads_and_converts_to_messages_for_restore() {
        let temp_dir = std::env::temp_dir().join(format!("theta-test-restore-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let path = temp_dir.join("ses_test.jsonl");

        let mut tree = crate::tree::SessionTree::new();
        for i in 0..10 {
            tree.append(&crate::ai::ChatMessage::user(format!("prompt {i}")));
            tree.append(&crate::ai::ChatMessage::assistant(format!("response {i}"), vec![]));
        }
        std::fs::write(&path, tree.to_jsonl()).unwrap();

        let loaded = crate::tree::SessionTree::load(&path).unwrap();
        let msgs = loaded.to_messages("ses_test");
        assert_eq!(msgs.len(), 20);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

/// Parse a `!cmd` / `!!cmd` shell escape line. Returns `(send_to_agent, cmd)`.
pub fn parse_shell_line(text: &str) -> Option<(bool, String)> {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("!!") {
        let cmd = rest.trim();
        return (!cmd.is_empty()).then(|| (false, cmd.to_string()));
    }
    if let Some(rest) = t.strip_prefix('!') {
        let cmd = rest.trim();
        return (!cmd.is_empty()).then(|| (true, cmd.to_string()));
    }
    None
}

#[cfg(test)]
mod shell_tests {
    use super::parse_shell_line;

    #[test]
    fn parses_bang_and_double_bang() {
        assert_eq!(parse_shell_line("!ls -la"), Some((true, "ls -la".into())));
        assert_eq!(parse_shell_line("!!git status"), Some((false, "git status".into())));
        assert_eq!(parse_shell_line("  !pwd"), Some((true, "pwd".into())));
        assert_eq!(parse_shell_line("hello"), None);
        assert_eq!(parse_shell_line("!"), None);
        assert_eq!(parse_shell_line("!!"), None);
    }
}

#[cfg(test)]
mod login_tests {
    use super::*;

    fn test_app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = Config::default();
        let manager = Manager::new(tx, cfg.clone());
        App::new(cfg, manager, PathBuf::from("/tmp"))
    }

    #[test]
    fn logs_slash_command_is_registered() {
        assert!(builtin_slash_items().iter().any(|i| i.name == "logs"));
    }

    #[test]
    fn login_lists_opencode_zen_and_go() {
        let ids: Vec<&str> = App::LOGIN_PROVIDERS.iter().map(|(id, _, _)| *id).collect();
        assert!(ids.contains(&"opencode"), "OpenCode Zen is offered");
        assert!(ids.contains(&"opencode-go"), "OpenCode Go is offered");
        // Both OpenCode gateways authenticate with the same env var.
        for (id, _, env) in App::LOGIN_PROVIDERS {
            if id.starts_with("opencode") {
                assert_eq!(*env, "OPENCODE_API_KEY", "{id} env");
            }
        }
    }

    #[tokio::test]
    async fn pasted_key_lands_in_the_login_field() {
        let mut app = test_app();
        app.overlay = Overlay::Login;
        app.login_ui.stage = LoginStage::Key;
        app.login_ui.provider = "opencode-go".into();

        app.handle_term_event(TermEvent::Paste("sk-new-key\n".into())).await;

        assert_eq!(app.login_ui.input.text(), "sk-new-key");
    }

    #[tokio::test]
    async fn pasted_url_lands_in_the_login_field() {
        let mut app = test_app();
        app.overlay = Overlay::Login;
        app.login_ui.stage = LoginStage::Url;

        app.handle_term_event(TermEvent::Paste("https://host/v1\n".into())).await;

        assert_eq!(app.login_ui.input.text(), "https://host/v1");
    }

    #[tokio::test]
    async fn paste_on_provider_list_is_ignored() {
        let mut app = test_app();
        app.sessions.push(SessionState::new(
            10,
            "background".into(),
            PathBuf::from("/tmp"),
        ));
        app.focus = 10;
        app.overlay = Overlay::Login;
        app.login_ui.stage = LoginStage::Choose;

        app.handle_term_event(TermEvent::Paste("sk-leak".into())).await;

        assert!(app.login_ui.input.text().is_empty());
        assert_eq!(app.session(10).expect("session").input.text(), "");
    }
}

/// Ring the terminal bell and best-effort send a desktop notification.
fn notify_desktop(title: &str, body: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
    let _ = std::process::Command::new("notify-send")
        .arg(title)
        .arg(body)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
