//! Syntax highlighting via syntect with a Tokyo Night color theme.

use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use ratatui::text::{Line, Span};
use syntect::highlighting::{
    FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;

use crate::theme::pal;

pub struct Highlighter {
    ps: SyntaxSet,
    theme: Theme,
}

static HL: RwLock<Option<(u64, Highlighter)>> = RwLock::new(None);

/// Cache of rendered diffs (keyed by patch/width/theme hash).
static DIFF_CACHE: RwLock<Option<HashMap<u64, Arc<Vec<Line<'static>>>>>> = RwLock::new(None);

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
        // Defensive cap: highlighting is synchronous, so a very large input
        // would block the UI for a long time. Render such content plainly.
        const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
        if content.len() > MAX_HIGHLIGHT_BYTES {
            return content
                .lines()
                .map(|l| vec![Span::styled(l.to_string(), Style::default().fg(pal().fg))])
                .collect();
        }
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

// ---------------------------------------------------------------------------
// OpenCode-style diff view: line numbers, syntax colours, tinted add/remove
// backgrounds. Renders split when wide enough, unified otherwise.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum DKind {
    Ctx,
    Add,
    Del,
}

struct DLine {
    kind: DKind,
    old: u64,
    new: u64,
    text: String,
}

struct Hunk {
    header: String,
    lines: Vec<DLine>,
}

fn hunk_starts(header: &str) -> (u64, u64) {
    let mut old = 1;
    let mut new = 1;
    for tok in header.split_whitespace() {
        if let Some(r) = tok.strip_prefix('-') {
            old = r.split(',').next().unwrap_or("1").parse().unwrap_or(1);
        } else if let Some(r) = tok.strip_prefix('+') {
            new = r.split(',').next().unwrap_or("1").parse().unwrap_or(1);
        }
    }
    (old, new)
}

fn parse_unified(patch: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut cur: Option<(String, Vec<DLine>, u64, u64)> = None;
    for raw in patch.lines() {
        if raw.starts_with("@@") {
            if let Some((h, l, _, _)) = cur.take() {
                hunks.push(Hunk { header: h, lines: l });
            }
            let (o, n) = hunk_starts(raw);
            cur = Some((raw.to_string(), Vec::new(), o, n));
            continue;
        }
        let Some((_, lines, old, new)) = cur.as_mut() else {
            continue;
        };
        if raw.starts_with("+++")
            || raw.starts_with("---")
            || raw.starts_with("Index:")
            || raw.starts_with("===")
            || raw.starts_with("diff ")
            || raw.starts_with("index ")
        {
            continue;
        }
        if let Some(rest) = raw.strip_prefix('+') {
            lines.push(DLine { kind: DKind::Add, old: 0, new: *new, text: rest.to_string() });
            *new += 1;
        } else if let Some(rest) = raw.strip_prefix('-') {
            lines.push(DLine { kind: DKind::Del, old: *old, new: 0, text: rest.to_string() });
            *old += 1;
        } else if let Some(rest) = raw.strip_prefix(' ') {
            lines.push(DLine { kind: DKind::Ctx, old: *old, new: *new, text: rest.to_string() });
            *old += 1;
            *new += 1;
        } else if raw.starts_with('\\') {
            // "\ No newline at end of file" — ignore
        } else {
            lines.push(DLine { kind: DKind::Ctx, old: *old, new: *new, text: raw.to_string() });
            *old += 1;
            *new += 1;
        }
    }
    if let Some((h, l, _, _)) = cur.take() {
        hunks.push(Hunk { header: h, lines: l });
    }
    hunks
}

fn added_bg() -> Color {
    crate::theme::blend(pal().green, pal().code_bg, 0.22)
}

fn removed_bg() -> Color {
    crate::theme::blend(pal().red, pal().code_bg, 0.22)
}

fn lang_of(file: &str) -> String {
    std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

fn gutter_width(hunks: &[Hunk]) -> usize {
    let mut maxn = 0u64;
    for h in hunks {
        for l in &h.lines {
            maxn = maxn.max(l.old).max(l.new);
        }
    }
    maxn.to_string().len().max(1)
}

fn clip(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for s in spans {
        if used >= max {
            break;
        }
        let chars: Vec<char> = s.content.chars().collect();
        let take = (max - used).min(chars.len());
        if take > 0 {
            out.push(Span::styled(chars[..take].iter().collect::<String>(), s.style));
            used += take;
        }
    }
    if out.is_empty() {
        out.push(Span::raw(String::new()));
    }
    out
}

fn hl_spans(hl: &Highlighter, text: &str, lang: &str) -> Vec<Span<'static>> {
    if text.is_empty() {
        return vec![Span::raw(String::new())];
    }
    hl.highlight_code(text, lang)
        .into_iter()
        .next()
        .unwrap_or_else(|| vec![Span::raw(String::new())])
}

fn pad_to(mut spans: Vec<Span<'static>>, width: usize, bg: Color) -> Vec<Span<'static>> {
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), Style::default().bg(bg)));
    }
    spans
}

fn tint(mut spans: Vec<Span<'static>>, bg: Option<Color>) -> Vec<Span<'static>> {
    if let Some(bg) = bg {
        for s in &mut spans {
            s.style = s.style.bg(bg);
        }
    }
    spans
}

/// Render a unified diff the way the OpenCode TUI does: line numbers, `+`/`-`
/// signs, syntax-highlighted content and tinted add/remove backgrounds.
/// `split` requests a side-by-side view (used when the pane is wide).
pub fn diff_view(patch: &str, file: &str, width: usize, split: bool) -> Vec<Line<'static>> {
    // Diffs are immutable and rendered on every spinner tick; cache the
    // highlighted result (keyed by content, width, layout and theme).
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        patch.hash(&mut h);
        file.hash(&mut h);
        width.hash(&mut h);
        split.hash(&mut h);
        crate::theme::theme_version().hash(&mut h);
        h.finish()
    };
    if let Ok(g) = DIFF_CACHE.read() {
        if let Some(m) = g.as_ref() {
            if let Some(v) = m.get(&key) {
                return (**v).clone();
            }
        }
    }
    let lines = diff_view_uncached(patch, file, width, split);
    if let Ok(mut g) = DIFF_CACHE.write() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > 256 {
            m.clear();
        }
        m.insert(key, Arc::new(lines.clone()));
    }
    lines
}

fn diff_view_uncached(patch: &str, file: &str, width: usize, split: bool) -> Vec<Line<'static>> {
    let hunks = parse_unified(patch);
    if hunks.is_empty() {
        return diff_lines(patch);
    }
    let width = width.max(20);
    if split && width >= 110 {
        render_split(&hunks, file, width)
    } else {
        render_unified(&hunks, file, width)
    }
}

fn render_unified(hunks: &[Hunk], file: &str, width: usize) -> Vec<Line<'static>> {
    let g = gutter_width(hunks);
    let lang = lang_of(file);
    let add_bg = added_bg();
    let del_bg = removed_bg();
    let pane_bg = pal().bg;
    let guard = get();
    let hl = guard.as_ref().map(|(_, h)| h).expect("highlighter");
    let mut out: Vec<Line<'static>> = Vec::new();

    for h in hunks {
        out.push(Line::from(Span::styled(
            h.header.clone(),
            Style::default().fg(pal().blue).bg(pane_bg),
        )));
        for l in &h.lines {
            let (old, new, sign, bg, sign_color) = match l.kind {
                DKind::Ctx => (
                    format!("{:>g$}", l.old),
                    format!("{:>g$}", l.new),
                    ' ',
                    pane_bg,
                    pal().fg_mute,
                ),
                DKind::Add => (
                    " ".repeat(g),
                    format!("{:>g$}", l.new),
                    '+',
                    add_bg,
                    pal().green,
                ),
                DKind::Del => (
                    format!("{:>g$}", l.old),
                    " ".repeat(g),
                    '-',
                    del_bg,
                    pal().red,
                ),
            };
            let gutter = format!("{old} {new} ");
            let sign_s = format!("{sign} ");
            let content_w = width.saturating_sub(gutter.chars().count() + 2);
            let content = clip(hl_spans(hl, &l.text, &lang), content_w);
            let mut spans = vec![
                Span::styled(gutter, Style::default().fg(pal().fg_mute)),
                Span::styled(sign_s, Style::default().fg(sign_color).add_modifier(Modifier::BOLD)),
            ];
            spans.extend(content);
            spans = pad_to(spans, width, bg);
            spans = tint(spans, Some(bg));
            out.push(Line::from(spans));
        }
    }
    out
}

fn render_split(hunks: &[Hunk], file: &str, width: usize) -> Vec<Line<'static>> {
    let g = gutter_width(hunks);
    let lang = lang_of(file);
    let add_bg = added_bg();
    let del_bg = removed_bg();
    let pane_bg = pal().bg;
    let sep_w = 3usize; // " │ "
    let pane_w = width.saturating_sub(sep_w);
    let left_w = pane_w / 2;
    let right_w = pane_w - left_w;
    let guard = get();
    let hl = guard.as_ref().map(|(_, h)| h).expect("highlighter");
    let mut out: Vec<Line<'static>> = Vec::new();

    for h in hunks {
        out.push(Line::from(Span::styled(
            h.header.clone(),
            Style::default().fg(pal().blue).bg(pane_bg),
        )));
        for (left, right) in split_rows(&h.lines) {
            let ls = render_side(hl, left, false, g, left_w, &lang, add_bg, del_bg, pane_bg);
            let rs = render_side(hl, right, true, g, right_w, &lang, add_bg, del_bg, pane_bg);
            let mut spans = ls;
            spans.push(Span::styled(
                " │ ".to_string(),
                Style::default().fg(pal().border).bg(pane_bg),
            ));
            spans.extend(rs);
            out.push(Line::from(spans));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_side(
    hl: &Highlighter,
    line: Option<&DLine>,
    is_new: bool,
    g: usize,
    width: usize,
    lang: &str,
    add_bg: Color,
    del_bg: Color,
    pane_bg: Color,
) -> Vec<Span<'static>> {
    let (num, sign, bg, sign_color) = match line {
        Some(l) => {
            let n = if is_new { l.new } else { l.old };
            let (sign, bg, sc) = match l.kind {
                DKind::Add => ('+', add_bg, pal().green),
                DKind::Del => ('-', del_bg, pal().red),
                DKind::Ctx => (' ', pane_bg, pal().fg_mute),
            };
            (format!("{n:>g$}"), sign, bg, sc)
        }
        None => (" ".repeat(g), ' ', pane_bg, pal().fg_mute),
    };
    let gutter = format!("{num} ");
    let content_w = width.saturating_sub(g + 3);
    let content = match line {
        Some(l) => clip(hl_spans(hl, &l.text, lang), content_w),
        None => vec![Span::raw(String::new())],
    };
    let mut spans = vec![
        Span::styled(gutter, Style::default().fg(pal().fg_mute)),
        Span::styled(format!("{sign} "), Style::default().fg(sign_color).add_modifier(Modifier::BOLD)),
    ];
    spans.extend(content);
    spans = pad_to(spans, width, bg);
    tint(spans, Some(bg))
}

/// Pair removed/added runs so they line up side by side; context is shared.
fn split_rows(lines: &[DLine]) -> Vec<(Option<&DLine>, Option<&DLine>)> {
    let mut out: Vec<(Option<&DLine>, Option<&DLine>)> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].kind == DKind::Ctx {
            out.push((Some(&lines[i]), Some(&lines[i])));
            i += 1;
            continue;
        }
        let mut dels: Vec<&DLine> = Vec::new();
        let mut adds: Vec<&DLine> = Vec::new();
        while i < lines.len() && lines[i].kind == DKind::Del {
            dels.push(&lines[i]);
            i += 1;
        }
        while i < lines.len() && lines[i].kind == DKind::Add {
            adds.push(&lines[i]);
            i += 1;
        }
        if dels.is_empty() && adds.is_empty() {
            // Unknown kind (shouldn't happen); consume to avoid a loop.
            i += 1;
            continue;
        }
        let n = dels.len().max(adds.len());
        for k in 0..n {
            out.push((dels.get(k).copied(), adds.get(k).copied()));
        }
    }
    out
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    const PATCH: &str = "Index: /a/b.rs\n\
===================================================================\n\
--- /a/b.rs\n\
+++ /a/b.rs\n\
@@ -142,7 +142,6 @@\n\
 binary = \"opencode\"\n\
 port_base = 4310\n\
-keep_alive = true\n\
 \n\
 [ui]\n\
 restore = true\n\
@@ -300,3 +299,4 @@\n\
 fn main() {}\n\
-old\n\
+new\n\
+extra\n";

    #[test]
    fn parses_and_renders_unified_and_split() {
        let u = diff_view(PATCH, "/a/b.rs", 80, false);
        assert!(!u.is_empty());
        let s = diff_view(PATCH, "/a/b.rs", 120, true);
        assert!(!s.is_empty());
        // A hit must be byte-identical to the first render.
        let u2 = diff_view(PATCH, "/a/b.rs", 80, false);
        assert_eq!(u.len(), u2.len());
    }

    #[test]
    fn tolerates_partial_and_empty() {
        // Empty input renders nothing (no panic).
        assert!(diff_view("", "/x.rs", 40, false).is_empty());
        // A header-only hunk is still renderable.
        assert!(!diff_view("@@ -1,1 +1,1 @@\n", "/x.rs", 40, false).is_empty());
    }
}
