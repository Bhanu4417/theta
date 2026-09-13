//! Global keybindings: actions, key specs, defaults, persistence.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    NewSession,
    Resume,
    Switch,
    Palette,
    Close,
    Quit,
    Maximize,
    Tiling,
    Explorer,
    Files,
    Project,
    Conversation,
    Model,
    Agent,
    GitDiff,
    GitLog,
    Interrupt,
    FocusNext,
    FocusPrev,
    Keymap,
}

impl Action {
    pub const ALL: [Action; 20] = [
        Action::NewSession,
        Action::Resume,
        Action::Switch,
        Action::Palette,
        Action::Close,
        Action::Quit,
        Action::Maximize,
        Action::Tiling,
        Action::Explorer,
        Action::Files,
        Action::Project,
        Action::Conversation,
        Action::Model,
        Action::Agent,
        Action::GitDiff,
        Action::GitLog,
        Action::Interrupt,
        Action::FocusNext,
        Action::FocusPrev,
        Action::Keymap,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Action::NewSession => "new_session",
            Action::Resume => "resume",
            Action::Switch => "switch",
            Action::Palette => "palette",
            Action::Close => "close",
            Action::Quit => "quit",
            Action::Maximize => "maximize",
            Action::Tiling => "tiling",
            Action::Explorer => "explorer",
            Action::Files => "files",
            Action::Project => "project",
            Action::Conversation => "conversation",
            Action::Model => "model",
            Action::Agent => "agent",
            Action::GitDiff => "git_diff",
            Action::GitLog => "git_log",
            Action::Interrupt => "interrupt",
            Action::FocusNext => "focus_next",
            Action::FocusPrev => "focus_prev",
            Action::Keymap => "keymap",
        }
    }

    pub fn from_name(s: &str) -> Option<Action> {
        Some(match s {
            "new_session" => Action::NewSession,
            "resume" => Action::Resume,
            "switch" => Action::Switch,
            "palette" => Action::Palette,
            "close" => Action::Close,
            "quit" => Action::Quit,
            "maximize" => Action::Maximize,
            "tiling" => Action::Tiling,
            "explorer" => Action::Explorer,
            "files" => Action::Files,
            "project" => Action::Project,
            "conversation" => Action::Conversation,
            "model" => Action::Model,
            "agent" => Action::Agent,
            "git_diff" => Action::GitDiff,
            "git_log" => Action::GitLog,
            "interrupt" => Action::Interrupt,
            "focus_next" => Action::FocusNext,
            "focus_prev" => Action::FocusPrev,
            "keymap" => Action::Keymap,
            _ => return None,
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Action::NewSession => "New session",
            Action::Resume => "Resume session",
            Action::Switch => "Switch to next session",
            Action::Palette => "Command palette",
            Action::Close => "Close session",
            Action::Quit => "Quit",
            Action::Maximize => "Maximize / restore pane",
            Action::Tiling => "Change tiling",
            Action::Explorer => "Toggle file explorer",
            Action::Files => "Search files",
            Action::Project => "Search project",
            Action::Conversation => "Search conversation",
            Action::Model => "Change model",
            Action::Agent => "Change agent",
            Action::GitDiff => "Git diff (workspace)",
            Action::GitLog => "Git log",
            Action::Interrupt => "Interrupt agent",
            Action::FocusNext => "Focus next pane",
            Action::FocusPrev => "Focus previous pane",
            Action::Keymap => "Open keymap",
        }
    }
}

/// A parsed key binding like `ctrl+shift+f`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl KeySpec {
    pub fn parse(s: &str) -> Option<KeySpec> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut key = String::new();
        for part in s.split('+') {
            let p = part.trim().to_ascii_lowercase();
            match p.as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                k if k.is_empty() => return None,
                k => key = k.to_string(),
            }
        }
        if key.is_empty() {
            return None;
        }
        Some(KeySpec { ctrl, alt, shift, key })
    }

    pub fn to_str(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        if self.shift {
            s.push_str("shift+");
        }
        s.push_str(&self.key);
        s
    }

    /// Human display: `^N`, `F1`, `Ctrl+Shift+F`, `Tab`.
    pub fn display(&self) -> String {
        if !self.ctrl && !self.alt && !self.shift {
            return match self.key.as_str() {
                "tab" => "Tab".into(),
                "space" => "Space".into(),
                "enter" => "Enter".into(),
                "esc" => "Esc".into(),
                other => {
                    if let Some(rest) = other.strip_prefix('f') {
                        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                            return format!("F{rest}");
                        }
                    }
                    other.to_uppercase()
                }
            };
        }
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        match self.key.as_str() {
            "space" => s.push_str("Space"),
            k if k.len() == 1 => s.push_str(&k.to_ascii_uppercase()),
            k => s.push_str(&{
                let mut t = k.to_string();
                if let Some(first) = t.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                t
            }),
        }
        s
    }

    /// Normalize a KeyEvent into a spec string (None = not bindable).
    pub fn spec_of(key: &KeyEvent) -> Option<String> {
        let (ctrl, alt, shift) = (
            key.modifiers.contains(KeyModifiers::CONTROL),
            key.modifiers.contains(KeyModifiers::ALT),
            key.modifiers.contains(KeyModifiers::SHIFT),
        );
        let key_name: String = match key.code {
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
            KeyCode::Tab => "tab".into(),
            KeyCode::BackTab => {
                return Some(KeySpec { ctrl, alt, shift: true, key: "tab".into() }.to_str())
            }
            KeyCode::Enter => "enter".into(),
            KeyCode::Esc => "esc".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Left => "left".into(),
            KeyCode::Right => "right".into(),
            KeyCode::Up => "up".into(),
            KeyCode::Down => "down".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pageup".into(),
            KeyCode::PageDown => "pagedown".into(),
            KeyCode::F(n) => format!("f{n}"),
            _ => return None,
        };
        Some(
            KeySpec { ctrl, alt, shift, key: key_name }.to_str(),
        )
    }
}

pub fn default_bindings() -> Vec<(Action, String)> {
    vec![
        (Action::NewSession, "ctrl+n".into()),
        (Action::Resume, "ctrl+r".into()),
        (Action::Switch, "ctrl+o".into()),
        (Action::Palette, "ctrl+k".into()),
        (Action::Close, "ctrl+w".into()),
        (Action::Quit, "ctrl+q".into()),
        (Action::Maximize, "ctrl+space".into()),
        (Action::Tiling, "ctrl+t".into()),
        (Action::Explorer, "ctrl+b".into()),
        (Action::Files, "ctrl+p".into()),
        (Action::Project, "ctrl+shift+f".into()),
        (Action::Conversation, "ctrl+f".into()),
        (Action::Model, String::new()),
        (Action::Agent, String::new()),
        (Action::GitDiff, String::new()),
        (Action::GitLog, String::new()),
        (Action::Interrupt, "ctrl+c".into()),
        (Action::FocusNext, "tab".into()),
        (Action::FocusPrev, "shift+tab".into()),
        (Action::Keymap, "f1".into()),
    ]
}

use std::collections::HashMap;

pub struct Keymap {
    bindings: Vec<(Action, Option<KeySpec>)>,
}

impl Keymap {
    /// Defaults overridden by user config (`keys.action = "ctrl+x"`).
    pub fn load(overrides: &HashMap<String, String>) -> Self {
        let mut bindings: Vec<(Action, Option<KeySpec>)> = Vec::new();
        for (action, def) in default_bindings() {
            let spec_str = overrides.get(action.name()).cloned().unwrap_or(def);
            bindings.push((action, KeySpec::parse(&spec_str)));
        }
        Self { bindings }
    }

    /// The command bound to this key event, if any.
    pub fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        let spec = KeySpec::spec_of(key)?;
        self.bindings
            .iter()
            .find(|(_, ks)| ks.as_ref().map(|k| k.to_str() == spec).unwrap_or(false))
            .map(|(a, _)| *a)
    }


    pub fn binding_of(&self, action: Action) -> Option<&KeySpec> {
        self.bindings
            .iter()
            .find(|(a, _)| *a == action)
            .and_then(|(_, ks)| ks.as_ref())
    }

    pub fn binding_str(&self, action: Action) -> String {
        self.binding_of(action)
            .map(|ks| ks.display())
            .unwrap_or_else(|| "—".into())
    }

    pub fn set(&mut self, action: Action, spec: KeySpec) {
        if let Some(entry) = self.bindings.iter_mut().find(|(a, _)| *a == action) {
            entry.1 = Some(spec);
        }
    }


    pub fn reset(&mut self, action: Action) {
        let def = default_bindings()
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, s)| KeySpec::parse(&s));
        if let Some(entry) = self.bindings.iter_mut().find(|(a, _)| *a == action) {
            entry.1 = def.flatten();
        }
    }

    /// (action, spec-string) pairs for persistence — empty = unbound.
    pub fn to_config_map(&self) -> HashMap<String, String> {
        self.bindings
            .iter()
            .map(|(a, ks)| (a.name().to_string(), ks.as_ref().map(|s| s.to_str()).unwrap_or_default()))
            .collect()
    }
}
