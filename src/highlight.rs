//! Syntax highlighting via syntect with a Tokyo Night color theme.

use ratatui::style::{Color, Modifier, Style};
use std::sync::RwLock;
use ratatui::text::{Line, Span};
use syntect::highlighting::{
    FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;

use crate::theme::{pal, rgb_tuple};

pub struct Highlighter {
    ps: SyntaxSet,
    theme: Theme,
}

static HL: RwLock<Option<(u64, Highlighter)>> = RwLock::new(None);

fn current() -> (u64, Highlighter) {
    (crate::theme::theme_version(), Highlighter::build())
}

/// Returns a snapshot rebuilt automatically when the theme changes.
pub fn get() -> std::sync::RwLockReadGuard<'static, Option<(u64, Highlighter)>> {
    loop {
        {
            let g = HL.read().expect("hl lock");
            if let Some((ver, _)) = g.as_ref() {
                if *ver == crate::theme::theme_version() {
                    return g;
                }
            }
        }
        {
            let mut g = HL.write().expect("hl lock");
            let stale = g.as_ref().map(|(v, _)| *v != crate::theme::theme_version());
            if stale.unwrap_or(true) {
                *g = Some(current());
            }
        }
        // fall through to the read path on the next loop iteration
        {
            let g = HL.read().expect("hl lock");
            if g.is_some() {
                return g;
            }
        }
    }
}

fn color(c: ratatui::style::Color) -> syntect::highlighting::Color {
    match c {
        ratatui::style::Color::Rgb(r, g, b) => {
            syntect::highlighting::Color { r, g, b, a: 0xff }
        }
        _ => syntect::highlighting::Color { r: 255, g: 255, b: 255, a: 0xff },
    }
}

fn item(scope: &str, fg: ratatui::style::Color, style: FontStyle) -> ThemeItem {
    ThemeItem {
        scope: scope.parse::<ScopeSelectors>().unwrap_or_default(),
        style: StyleModifier {
            foreground: Some(color(fg)),
            background: None,
            font_style: Some(style),
        },
    }
}

impl Highlighter {
    fn build() -> Self {
        let ps = SyntaxSet::load_defaults_newlines();
        let p = crate::theme::pal();
        let mut settings = ThemeSettings::default();
        settings.foreground = Some(color(p.fg));
        settings.background = Some(color(p.code_bg));

        let it = FontStyle::ITALIC;
        let scopes = vec![
            item("comment", p.fg_dim, it),
            item("string", p.green, FontStyle::empty()),
            item("string.regexp", p.teal, FontStyle::empty()),
            item("constant.numeric", p.orange, FontStyle::empty()),
            item("constant.language", p.orange, FontStyle::empty()),
            item("constant.character.escape", p.orange, FontStyle::empty()),
            item("keyword", p.purple, FontStyle::empty()),
            item("keyword.operator", p.cyan, FontStyle::empty()),
            item("storage", p.purple, FontStyle::empty()),
            item("entity.name.function", p.blue, FontStyle::empty()),
            item("support.function", p.blue, FontStyle::empty()),
            item("entity.name.type", p.teal, FontStyle::empty()),
            item("entity.name.class", p.teal, FontStyle::empty()),
            item("entity.name.struct", p.teal, FontStyle::empty()),
            item("entity.name.enum", p.teal, FontStyle::empty()),
            item("support.type", p.teal, FontStyle::empty()),
            item("support.class", p.teal, FontStyle::empty()),
            item("entity.other.attribute-name", p.purple, FontStyle::empty()),
            item("entity.name.tag", p.purple, FontStyle::empty()),
            item("variable.language", p.red, FontStyle::empty()),
            item("variable.other.property", p.cyan, FontStyle::empty()),
            item("meta.definition.variable", p.fg, FontStyle::empty()),
            item("punctuation.definition", p.cyan, FontStyle::empty()),
            item("markup.heading", p.blue, FontStyle::BOLD),
            item("markup.raw", p.green, FontStyle::empty()),
        ];

        let theme = Theme {
            name: Some("theta-active-theme".into()),
            author: Some("theta".into()),
            settings,
            scopes,
        };
        Self { ps, theme }
    }

    fn syntax_for(
        &self,
        path: &str,
        lang: &str,
    ) -> &syntect::parsing::SyntaxReference {
        self.ps
            .find_syntax_by_token(lang)
            .or_else(|| self.ps.find_syntax_by_token(path.rsplit('.').next().unwrap_or("")))
            .unwrap_or_else(|| self.ps.find_syntax_plain_text())
    }

    /// Highlight code into per-line owned spans (no trailing newline).
    pub fn highlight_code(&self, code: &str, lang: &str) -> Vec<Vec<Span<'static>>> {
        self.highlight_impl(code, "", lang)
    }

    /// Highlight a file by extension; returns per-line owned spans.
    pub fn highlight_file(&self, path: &str, content: &str) -> Vec<Vec<Span<'static>>> {
        self.highlight_impl(content, path, "")
    }

    fn highlight_impl(&self, content: &str, path: &str, lang: &str) -> Vec<Vec<Span<'static>>> {
        let syntax = self.syntax_for(path, lang);
        let mut hl = syntect::easy::HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in syntect::util::LinesWithEndings::from(content) {
            let ranges = match hl.highlight_line(line, &self.ps) {
                Ok(r) => r,
                Err(_) => {
                    out.push(vec![Span::styled(
                        line.trim_end().to_string(),
                        Style::default().fg(pal().fg),
                    )]);
                    continue;
                }
            };
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (st, text) in ranges {
                let mut style = Style::default().fg(syntect_to_ratatui(st.foreground));
                if st.font_style.contains(FontStyle::BOLD) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if st.font_style.contains(FontStyle::ITALIC) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if st.font_style.contains(FontStyle::UNDERLINE) {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                let text = text.trim_end_matches('\n');
                if text.is_empty() {
                    continue;
                }
                spans.push(Span::styled(text.to_string(), style));
            }
            if spans.is_empty() {
                spans.push(Span::styled(String::new(), Style::default()));
            }
            out.push(spans);
        }
        if out.is_empty() {
            out.push(vec![Span::styled(String::new(), Style::default())]);
        }
        out
    }
}

fn syntect_to_ratatui(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

/// Diff text → colored lines (+ green, - red, @@ blue, headers dim).
pub fn diff_lines(diff: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for raw in diff.lines() {
        let style = if raw.starts_with("+++") || raw.starts_with("---") || raw.starts_with("diff ") || raw.starts_with("index ") {
            Style::default().fg(pal().fg_dim)
        } else if raw.starts_with("@@") {
            Style::default().fg(pal().blue)
        } else if raw.starts_with('+') {
            Style::default().fg(pal().green)
        } else if raw.starts_with('-') {
            Style::default().fg(pal().red)
        } else {
            Style::default().fg(pal().fg_soft)
        };
        out.push(Line::from(Span::styled(raw.to_string(), style)));
    }
    out
}
