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
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    Keymap,
    Editor,
}

impl Action {
    pub const ALL: [Action; 33] = [
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
        Action::FocusLeft,
        Action::FocusRight,
        Action::FocusUp,
        Action::FocusDown,
        Action::MoveLeft,
        Action::MoveRight,
        Action::MoveUp,
        Action::MoveDown,
        Action::ResizeLeft,
        Action::ResizeRight,
        Action::ResizeUp,
        Action::ResizeDown,
        Action::Keymap,
        Action::Editor,
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
            Action::FocusLeft => "focus_left",
            Action::FocusRight => "focus_right",
            Action::FocusUp => "focus_up",
            Action::FocusDown => "focus_down",
            Action::MoveLeft => "move_left",
            Action::MoveRight => "move_right",
            Action::MoveUp => "move_up",
            Action::MoveDown => "move_down",
            Action::ResizeLeft => "resize_left",
            Action::ResizeRight => "resize_right",
            Action::ResizeUp => "resize_up",
            Action::ResizeDown => "resize_down",
            Action::Keymap => "keymap",
            Action::Editor => "editor",
        }
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
            Action::FocusLeft => "Focus pane to the left",
            Action::FocusRight => "Focus pane to the right",
            Action::FocusUp => "Focus pane above",
            Action::FocusDown => "Focus pane below",
            Action::MoveLeft => "Move pane left",
            Action::MoveRight => "Move pane right",
            Action::MoveUp => "Move pane up",
            Action::MoveDown => "Move pane down",
            Action::ResizeLeft => "Resize pane left",
            Action::ResizeRight => "Resize pane right",
            Action::ResizeUp => "Resize pane taller",
            Action::ResizeDown => "Resize pane shorter",
            Action::Keymap => "Open keymap",
            Action::Editor => "Edit prompt in $EDITOR",
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
        (Action::Switch, String::new()),
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
        (Action::FocusLeft, "alt+left".into()),
        (Action::FocusRight, "alt+right".into()),
        (Action::FocusUp, "alt+up".into()),
        (Action::FocusDown, "alt+down".into()),
        (Action::MoveLeft, "alt+shift+left".into()),
        (Action::MoveRight, "alt+shift+right".into()),
        (Action::MoveUp, "alt+shift+up".into()),
        (Action::MoveDown, "alt+shift+down".into()),
        (Action::ResizeLeft, "ctrl+alt+left".into()),
        (Action::ResizeRight, "ctrl+alt+right".into()),
        (Action::ResizeUp, "ctrl+alt+up".into()),
        (Action::ResizeDown, "ctrl+alt+down".into()),
        (Action::Keymap, "f1".into()),
        (Action::Editor, "ctrl+g".into()),
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

}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::HashMap;

    #[test]
    fn arrow_family_resolves_to_pane_ops() {
        let km = Keymap::load(&HashMap::new());
        let ev = |code, mods| KeyEvent::new(code, mods);
        assert_eq!(
            km.action_for(&ev(KeyCode::Left, KeyModifiers::ALT)),
            Some(Action::FocusLeft)
        );
        assert_eq!(
            km.action_for(&ev(
                KeyCode::Left,
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )),
            Some(Action::MoveLeft)
        );
        assert_eq!(
            km.action_for(&ev(
                KeyCode::Left,
                KeyModifiers::ALT | KeyModifiers::CONTROL
            )),
            Some(Action::ResizeLeft)
        );
        assert_eq!(
            km.action_for(&ev(
                KeyCode::Down,
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )),
            Some(Action::MoveDown)
        );
        assert_eq!(
            km.action_for(&ev(
                KeyCode::Up,
                KeyModifiers::ALT | KeyModifiers::CONTROL
            )),
            Some(Action::ResizeUp)
        );
        // Ctrl+O is no longer bound to Switch.
        assert_eq!(
            km.action_for(&ev(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            None
        );
    }
}
