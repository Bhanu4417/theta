//! Theme engine: runtime palettes driving every colour in the UI.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// Style-preserving patch for spans: merges `style` over the span's own style,
/// so callers can add a background without discarding the foreground. Shared by
/// the pane and overlay renderers (it was duplicated in both).
pub trait SpanExt {
    fn patch(self, style: Style) -> Self;
}

impl SpanExt for Span<'static> {
    fn patch(self, style: Style) -> Self {
        Span::styled(self.content, self.style.patch(style))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub name: &'static str,
    pub label: &'static str,

    pub bg: Color,
    pub bg_dark: Color,
    pub bg_float: Color,
    pub selection: Color,
    pub code_bg: Color,
    pub user_bg: Color,

    pub border: Color,
    pub border_busy: Color,
    pub border_focus: Color,

    pub fg: Color,
    pub fg_dim: Color,
    pub fg_mute: Color,
    pub fg_soft: Color,

    pub blue: Color,
    pub cyan: Color,
    pub teal: Color,
    pub purple: Color,
    pub orange: Color,
    pub yellow: Color,
    pub green: Color,
    pub red: Color,

    /// "Th." wordmark: T, h, dot.
    pub mark_t: Color,
    /// Thinking/loader spinner gradient stops (cycled).
    pub spin_stops: [Color; 4],
}

const fn c(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const THEMES: &[Palette] = &[
    // ── Theta default (Tokyo Night) ─────────────────────────────
    Palette {
        name: "theta-night",
        label: "Theta Night",
        bg: c(0x1a1b26), bg_dark: c(0x16161e), bg_float: c(0x1f2233),
        selection: c(0x292e42), code_bg: c(0x1b1c2c), user_bg: c(0x24283b),
        border: c(0x2f334d), border_busy: c(0x444c74), border_focus: c(0x7aa2f7),
        fg: c(0xc0caf5), fg_dim: c(0x565f89), fg_mute: c(0x414868), fg_soft: c(0x9aa4c4),
        blue: c(0x7aa2f7), cyan: c(0x7dcfff), teal: c(0x73daca), purple: c(0xbb9af7),
        orange: c(0xff9e64), yellow: c(0xe0af68), green: c(0x9ece6a), red: c(0xf7768e),
        mark_t: c(0x7aa2f7),
        spin_stops: [c(0x7dcfff), c(0x7aa2f7), c(0xbb9af7), c(0x73daca)],
    },
    // ── Theta Moon (tokyonight moon) ────────────────────────────
    Palette {
        name: "theta-moon",
        label: "Theta Moon",
        bg: c(0x222436), bg_dark: c(0x1e2030), bg_float: c(0x2d3f76),
        selection: c(0x3654a0), code_bg: c(0x252639), user_bg: c(0x2f334d),
        border: c(0x3654a0), border_busy: c(0x4c6ead), border_focus: c(0x82aaff),
        fg: c(0xc8d3f5), fg_dim: c(0x636da2), fg_mute: c(0x49508a), fg_soft: c(0xa9b1d6),
        blue: c(0x82aaff), cyan: c(0x86e1fc), teal: c(0x4fd6be), purple: c(0xc099ff),
        orange: c(0xffc777), yellow: c(0xffc777), green: c(0xc3e88d), red: c(0xff757f),
        mark_t: c(0x82aaff),
        spin_stops: [c(0x86e1fc), c(0x82aaff), c(0xc099ff), c(0x4fd6be)],
    },
    // ── One Drift (one dark) ───────────────────────────────────
    Palette {
        name: "one-drift",
        label: "One Drift",
        bg: c(0x282c34), bg_dark: c(0x21252b), bg_float: c(0x2f343e),
        selection: c(0x3e4451), code_bg: c(0x2a2f39), user_bg: c(0x353b47),
        border: c(0x3e4451), border_busy: c(0x545862), border_focus: c(0x61afef),
        fg: c(0xabb2bf), fg_dim: c(0x5c6370), fg_mute: c(0x4b5263), fg_soft: c(0x9da5b4),
        blue: c(0x61afef), cyan: c(0x56b6c2), teal: c(0x56b6c2), purple: c(0xc678dd),
        orange: c(0xd19a66), yellow: c(0xe5c07b), green: c(0x98c379), red: c(0xe06c75),
        mark_t: c(0x61afef),
        spin_stops: [c(0x56b6c2), c(0x61afef), c(0xc678dd), c(0x98c379)],
    },
    // ── Rose Fjord (rose pine) ─────────────────────────────────
    Palette {
        name: "rose-fjord",
        label: "Rose Fjord",
        bg: c(0x191724), bg_dark: c(0x1f1d2e), bg_float: c(0x26233a),
        selection: c(0x403d52), code_bg: c(0x1f1d2e), user_bg: c(0x2f2c45),
        border: c(0x403d52), border_busy: c(0x524f67), border_focus: c(0xc4a7e7),
        fg: c(0xe0def4), fg_dim: c(0x6e6a86), fg_mute: c(0x514968), fg_soft: c(0x908caa),
        blue: c(0x9ccfd8), cyan: c(0x9ccfd8), teal: c(0x9ccfd8), purple: c(0xc4a7e7),
        orange: c(0xf6c177), yellow: c(0xf6c177), green: c(0x31748f), red: c(0xeb6f92),
        mark_t: c(0xc4a7e7),
        spin_stops: [c(0xebbcba), c(0xc4a7e7), c(0x9ccfd8), c(0xf6c177)],
    },
    // ── Velvet Mocha (catppuccin mocha) ────────────────────────
    Palette {
        name: "velvet-mocha",
        label: "Velvet Mocha",
        bg: c(0x1e1e2e), bg_dark: c(0x181825), bg_float: c(0x313244),
        selection: c(0x45475a), code_bg: c(0x1f1f30), user_bg: c(0x363a4f),
        border: c(0x45475a), border_busy: c(0x585b70), border_focus: c(0x89b4fa),
        fg: c(0xcdd6f4), fg_dim: c(0x6c7086), fg_mute: c(0x585b70), fg_soft: c(0xa6adc8),
        blue: c(0x89b4fa), cyan: c(0x89dceb), teal: c(0x94e2d5), purple: c(0xcba6f7),
        orange: c(0xfab387), yellow: c(0xf9e2af), green: c(0xa6e3a1), red: c(0xf38ba8),
        mark_t: c(0x89b4fa),
        spin_stops: [c(0x89dceb), c(0x89b4fa), c(0xf5c2e7), c(0x94e2d5)],
    },
    // ── Ember Gruv (gruvbox dark) ──────────────────────────────
    Palette {
        name: "ember-gruv",
        label: "Ember Gruv",
        bg: c(0x282828), bg_dark: c(0x1d2021), bg_float: c(0x32302f),
        selection: c(0x45403d), code_bg: c(0x2c2828), user_bg: c(0x3c3836),
        border: c(0x45403d), border_busy: c(0x665c54), border_focus: c(0x83a598),
        fg: c(0xebdbb2), fg_dim: c(0x928374), fg_mute: c(0x665c54), fg_soft: c(0xbdae93),
        blue: c(0x83a598), cyan: c(0x8ec07c), teal: c(0x8ec07c), purple: c(0xd3869b),
        orange: c(0xfe8019), yellow: c(0xfabd2f), green: c(0xb8bb26), red: c(0xfb4934),
        mark_t: c(0xfabd2f),
        spin_stops: [c(0x8ec07c), c(0xfabd2f), c(0xd3869b), c(0x83a598)],
    },
    // ── Pine Grove (everforest) ────────────────────────────────
    Palette {
        name: "pine-grove",
        label: "Pine Grove",
        bg: c(0x2d353b), bg_dark: c(0x272e33), bg_float: c(0x343f44),
        selection: c(0x414b50), code_bg: c(0x303a40), user_bg: c(0x3d484d),
        border: c(0x414b50), border_busy: c(0x525c62), border_focus: c(0xa7c080),
        fg: c(0xd3c6aa), fg_dim: c(0x859289), fg_mute: c(0x525c62), fg_soft: c(0x9da9a0),
        blue: c(0x7fbbb3), cyan: c(0x83c092), teal: c(0x83c092), purple: c(0xd699b6),
        orange: c(0xe69875), yellow: c(0xdbbc7f), green: c(0xa7c080), red: c(0xe67e80),
        mark_t: c(0xa7c080),
        spin_stops: [c(0x83c092), c(0xa7c080), c(0xd699b6), c(0x7fbbb3)],
    },
    // ── Wave Garden (kanagawa) ─────────────────────────────────
    Palette {
        name: "wave-garden",
        label: "Wave Garden",
        bg: c(0x1f1f28), bg_dark: c(0x16161d), bg_float: c(0x2a2a37),
        selection: c(0x2d4f67), code_bg: c(0x21212b), user_bg: c(0x2b2b3a),
        border: c(0x2d4f67), border_busy: c(0x44446a), border_focus: c(0x7e9cd8),
        fg: c(0xdcd7ba), fg_dim: c(0x727169), fg_mute: c(0x54546d), fg_soft: c(0xa8a49c),
        blue: c(0x7e9cd8), cyan: c(0x7aa89f), teal: c(0x7aa89f), purple: c(0x957fb8),
        orange: c(0xffa066), yellow: c(0xffe8be), green: c(0x98bb6c), red: c(0xe46876),
        mark_t: c(0x7e9cd8),
        spin_stops: [c(0x7aa89f), c(0x7e9cd8), c(0x957fb8), c(0xffa066)],
    },
    // ── Night Cape (dracula) ───────────────────────────────────
    Palette {
        name: "night-cape",
        label: "Night Cape",
        bg: c(0x282a36), bg_dark: c(0x21222c), bg_float: c(0x343746),
        selection: c(0x44475a), code_bg: c(0x2b2d3a), user_bg: c(0x3b3d4f),
        border: c(0x44475a), border_busy: c(0x6272a4), border_focus: c(0xbd93f9),
        fg: c(0xf8f8f2), fg_dim: c(0x6272a4), fg_mute: c(0x4d5472), fg_soft: c(0xbfc9d4),
        blue: c(0x8be9fd), cyan: c(0x8be9fd), teal: c(0x50fa7b), purple: c(0xbd93f9),
        orange: c(0xffb86c), yellow: c(0xf1fa8c), green: c(0x50fa7b), red: c(0xff5555),
        mark_t: c(0xbd93f9),
        spin_stops: [c(0x8be9fd), c(0xbd93f9), c(0xff79c6), c(0x50fa7b)],
    },
    // ── Synth '84 (synthwave84) ────────────────────────────────
    Palette {
        name: "synth-84",
        label: "Synth '84",
        bg: c(0x241b2f), bg_dark: c(0x1a1428), bg_float: c(0x33293f),
        selection: c(0x443359), code_bg: c(0x291f37), user_bg: c(0x3a2d4d),
        border: c(0x443359), border_busy: c(0x5b4b78), border_focus: c(0xfede5d),
        fg: c(0xf8f8f2), fg_dim: c(0x7b6d8d), fg_mute: c(0x584a70), fg_soft: c(0xd6c7e3),
        blue: c(0x36f9f6), cyan: c(0x36f9f6), teal: c(0x72f1b8), purple: c(0xff7edb),
        orange: c(0xfe4450), yellow: c(0xfede5d), green: c(0x72f1b8), red: c(0xfe4450),
        mark_t: c(0x36f9f6),
        spin_stops: [c(0x36f9f6), c(0xff7edb), c(0xfede5d), c(0x72f1b8)],
    },
    // ── Mirage Ayu (ayu dark) ──────────────────────────────────
    Palette {
        name: "mirage-ayu",
        label: "Mirage Ayu",
        bg: c(0x0b0e14), bg_dark: c(0x0d1017), bg_float: c(0x151a22),
        selection: c(0x1f2633), code_bg: c(0x11151d), user_bg: c(0x1a202b),
        border: c(0x1f2633), border_busy: c(0x2c364a), border_focus: c(0x39bae6),
        fg: c(0xbfbdb6), fg_dim: c(0x626a73), fg_mute: c(0x4a525d), fg_soft: c(0x8a9199),
        blue: c(0x59c2ff), cyan: c(0x39bae6), teal: c(0x95e6cb), purple: c(0xd2a6ff),
        orange: c(0xffb454), yellow: c(0xffd173), green: c(0xaad94c), red: c(0xf07178),
        mark_t: c(0x59c2ff),
        spin_stops: [c(0x39bae6), c(0x59c2ff), c(0xd2a6ff), c(0xaad94c)],
    },
    // ── Vesper Noir (vesper) ───────────────────────────────────
    Palette {
        name: "vesper-noir",
        label: "Vesper Noir",
        bg: c(0x101010), bg_dark: c(0x0a0a0a), bg_float: c(0x1c1c1c),
        selection: c(0x2a2a2a), code_bg: c(0x151515), user_bg: c(0x222222),
        border: c(0x2a2a2a), border_busy: c(0x3f3f3f), border_focus: c(0xffc799),
        fg: c(0xffffff), fg_dim: c(0x8c8c8c), fg_mute: c(0x5f5f5f), fg_soft: c(0xb4b4b4),
        blue: c(0x99ffe4), cyan: c(0x99ffe4), teal: c(0x99ffe4), purple: c(0x9ca0a0),
        orange: c(0xffc799), yellow: c(0xffc799), green: c(0x8c9779), red: c(0xff8080),
        mark_t: c(0xffc799),
        spin_stops: [c(0x99ffe4), c(0xffc799), c(0xffffff), c(0x8c9779)],
    },
    // ── Cobalt Tide (cobalt2) ──────────────────────────────────
    Palette {
        name: "cobalt-tide",
        label: "Cobalt Tide",
        bg: c(0x193549), bg_dark: c(0x122b3b), bg_float: c(0x21475e),
        selection: c(0x2d5470), code_bg: c(0x1d3d52), user_bg: c(0x264a63),
        border: c(0x2d5470), border_busy: c(0x3e6f8f), border_focus: c(0x0088ff),
        fg: c(0xffffff), fg_dim: c(0x7f9db3), fg_mute: c(0x5c7b93), fg_soft: c(0xbfd4e2),
        blue: c(0x0088ff), cyan: c(0x00e0ff), teal: c(0x00e0ff), purple: c(0xff628c),
        orange: c(0xff9d00), yellow: c(0xffd500), green: c(0x3ad900), red: c(0xff628c),
        mark_t: c(0x0088ff),
        spin_stops: [c(0x00e0ff), c(0x0088ff), c(0xff628c), c(0x3ad900)],
    },
    // ── Theta Day (tokyonight day, light) ──────────────────────
    Palette {
        name: "theta-day",
        label: "Theta Day",
        bg: c(0xe1e2e7), bg_dark: c(0xc4c8da), bg_float: c(0xd0d5e3),
        selection: c(0xb6bfe3), code_bg: c(0xd4d8e2), user_bg: c(0xc8cfe5),
        border: c(0xa9b1d6), border_busy: c(0x8990b3), border_focus: c(0x2e7de9),
        fg: c(0x3760bf), fg_dim: c(0x8990b3), fg_mute: c(0x9aa5ce), fg_soft: c(0x6172b0),
        blue: c(0x2e7de9), cyan: c(0x007197), teal: c(0x007197), purple: c(0x9854f1),
        orange: c(0xb15c00), yellow: c(0x8c6c3e), green: c(0x387068), red: c(0xf52a65),
        mark_t: c(0x2e7de9),
        spin_stops: [c(0x007197), c(0x2e7de9), c(0x9854f1), c(0x387068)],
    },
];

static CURRENT: RwLock<Palette> = RwLock::new(PALETTES[0]);
static PALETTES: &[Palette] = THEMES;

static VERSION: AtomicU64 = AtomicU64::new(1);

/// The active palette (copied — cheap).
pub fn pal() -> Palette {
    *CURRENT.read().expect("theme lock")
}

pub fn current_name() -> &'static str {
    pal().name
}

pub fn theme_names() -> Vec<&'static str> {
    THEMES.iter().map(|t| t.name).collect()
}

pub fn theme_label(name: &str) -> String {
    THEMES
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.label.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Switch theme by name; bumps the highlight version so syntax colours rebuild.
pub fn set_theme(name: &str) -> bool {
    if let Some(p) = THEMES.iter().find(|t| t.name == name) {
        *CURRENT.write().expect("theme lock") = *p;
        VERSION.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

pub fn theme_version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

fn to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

/// Linear blend: `t` of `a` mixed over `b` (0 = all b, 1 = all a).
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = to_rgb(a);
    let (br, bg, bb) = to_rgb(b);
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 * t + y as f32 * (1.0 - t)).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// Style helpers reading the active palette.
pub fn fg(c: Color) -> Style {
    Style::default().fg(c)
}

pub fn dim() -> Style {
    fg(pal().fg_dim)
}

pub fn mute() -> Style {
    fg(pal().fg_mute)
}

pub fn bold(c: Color) -> Style {
    Style::default().fg(c).add_modifier(ratatui::style::Modifier::BOLD)
}

pub const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// Spinner frame for the given tick.
pub fn spin(tick: u64) -> &'static str {
    SPINNER[(tick % SPINNER.len() as u64) as usize]
}

/// Smoothly cycling colour for spinners and accents (theme gradient).
pub fn spin_rgb(tick: u64) -> Color {
    let stops = pal().spin_stops;
    let segs = stops.len();
    let steps_per_seg = 10.0f64;
    let pos = (tick as f64 / steps_per_seg) % segs as f64;
    let i = pos.floor() as usize;
    let t = (pos - i as f64) as f32;
    let a = stops[i];
    let b = stops[(i + 1) % segs];
    let (ar, ag, ab) = match a { Color::Rgb(r, g, b) => (r, g, b), _ => (255, 255, 255) };
    let (br, bg_, bb) = match b { Color::Rgb(r, g, b) => (r, g, b), _ => (255, 255, 255) };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg_), mix(ab, bb))
}

/// Abbreviate a path with `~` for the home directory.
pub fn abbreviate_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy().to_string();
        if path == home {
            return "~".into();
        }
        if let Some(rest) = path.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    path.to_string()
}

/// The Θ identity glyph used across the UI.
pub const P_SYMBOL: &str = "Θ";

/// The "Th." wordmark — compact, mono-colour using the theme accent.
pub const MARK_W: usize = 27;
pub const MARK_H: usize = 8;

type MarkSeg = (usize, usize); // (col_start, col_end) inclusive

fn mark_segs(row: usize) -> &'static [MarkSeg] {
    match row {
        0 | 1 => &[(0, 10), (13, 14)],
        2 => &[(4, 6), (13, 14)],
        3 => &[(4, 6), (13, 20)],
        4 | 5 => &[(4, 6), (13, 14), (19, 20)],
        6 | 7 => &[(4, 6), (13, 14), (19, 20), (24, 26)],
        _ => &[],
    }
}

/// One styled row of the "Th." wordmark (theme accent colour).
pub fn theta_mark_line(row: usize) -> Line<'static> {
    let p = pal();
    let accent = p.mark_t;
    let mut cells: Vec<bool> = vec![false; MARK_W];
    for &(c0, c1) in mark_segs(row) {
        for c in c0..=c1.min(MARK_W - 1) {
            cells[c] = true;
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0usize;
    while i < MARK_W {
        if cells[i] {
            let start = i;
            while i < MARK_W && cells[i] {
                i += 1;
            }
            spans.push(Span::styled(
                "█".repeat(i - start),
                Style::default().fg(accent),
            ));
        } else {
            let start = i;
            while i < MARK_W && !cells[i] {
                i += 1;
            }
            if i < MARK_W {
                spans.push(Span::raw(" ".repeat(i - start)));
            }
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_and_midpoint() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 40);
        assert_eq!(blend(a, b, 0.0), b);
        assert_eq!(blend(a, b, 1.0), a);
        assert_eq!(blend(a, b, 0.5), Color::Rgb(50, 100, 20));
    }

    #[test]
    fn themes_have_unique_names() {
        let names = theme_names();
        assert!(names.contains(&"theta-night"), "default theme present");
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "theme names must be unique");
    }
}
