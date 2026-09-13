//! Overlay surfaces: palette, dialogs, searches, viewer, diff.

use crate::app::App;
use crate::theme::{pal, self};
use crate::ui::conversation::truncate;
use ratatui::layout::Rect;
use ratatui::style::{Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};

pub fn centered_rect_w(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w.max(10),
        height: h,
    }
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let w = (area.width as u32 * percent_x as u32 / 100) as u16;
    let h = height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w.max(10), height: h }
}

fn surface(f: &mut ratatui::Frame, area: Rect, title: Line<'static>) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(pal().border_focus))
        .style(Style::default().bg(pal().bg_float))
        .padding(Padding::new(1, 1, 1, 1))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    inner
}

/// Style-preserving background patch for spans.
trait SpanExt {
    fn patch(self, style: Style) -> Self;
}
impl SpanExt for Span<'static> {
    fn patch(self, style: Style) -> Self {
        Span::styled(self.content, self.style.patch(style))
    }
}

/// Stop drawing when the surface is full.
fn lines_drawable(current: usize, cap: usize) -> bool {
    current + 1 < cap
}

/// Scroll offset keeping `sel` visible in a window of `visible` rows.
fn window_offset(sel: usize, len: usize, visible: usize) -> usize {
    if len <= visible || visible == 0 {
        0
    } else {
        sel.saturating_sub(visible / 2).min(len - visible)
    }
}

fn title_line(spans: Vec<Span<'static>>) -> Line<'static> {
    let mut s = vec![Span::raw(" ")];
    s.extend(spans);
    s.push(Span::raw(" "));
    Line::from(s)
}

// ---------------------------------------------------------------------------

pub fn render_palette(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let cmds = filtered(app.palette.input.text());
    let h = (cmds.len() + 2).min(18) as u16 + 4;
    let rect = centered_rect(56, h, area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("Commands", theme::bold(pal().fg)),
        ]),
    );
    if inner.height == 0 || inner.width < 12 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)),
            Span::styled(
            if app.palette.input.is_empty() {
                "Search commands…".to_string()
            } else {
                app.palette.input.text().to_string()
            },
            if app.palette.input.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        ),
    ]));
    lines.push(Line::from(Span::styled("─".repeat(inner.width as usize - 2), theme::mute())));

    if cmds.is_empty() {
        lines.push(Line::from(Span::styled("  no matching commands", theme::dim())));
    }
    let w = inner.width as usize;
    let visible = inner.height as usize - 2;
    let offset = window_offset(app.palette.selected, cmds.len(), visible);
    for (i, c) in cmds.iter().enumerate().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let selected = i == app.palette.selected.min(cmds.len().saturating_sub(1));
        let label = truncate(c.label, w.saturating_sub(12).max(8));
        let hint = c.hint.to_string();
        let pad = w.saturating_sub(label.chars().count() + hint.chars().count() + 4);
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(
                label,
                if selected { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
            Span::styled(" ".repeat(pad), Style::default()),
            Span::styled(
                hint,
                if selected { theme::fg(pal().cyan) } else { theme::mute() },
            ),
        ];
        if selected {
            for s in &mut spans {
                s.style = s.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);

    // cursor
    let cx = inner.x + 2 + app.palette.input.cursor as u16;
    if cx < inner.x + inner.width {
        f.set_cursor_position((cx, inner.y));
    }
}

fn filtered(query: &str) -> Vec<&'static crate::app::Command> {
    crate::app::filtered_commands(query)
}

// ---------------------------------------------------------------------------

pub fn render_new_session(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let recent_h = app.newdlg.recent.len().min(6) as u16;
    let h = 8 + recent_h;
    let rect = centered_rect(52, h, area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("New Session", theme::bold(pal().fg)),
        ]),
    );
    if inner.height < 5 || inner.width < 20 {
        return;
    }
    let w = inner.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (label, value, field) in [
        ("Name", app.newdlg.name.text().to_string(), 0usize),
        ("Directory", app.newdlg.dir.clone(), 1usize),
    ] {
        let active = app.newdlg.field == field;
        let val = if value.is_empty() {
            if field == 0 { "session".to_string() } else { String::new() }
        } else {
            truncate(&value, w.saturating_sub(14).max(8))
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {label:<10}"),
                if active { theme::bold(pal().cyan) } else { theme::fg(pal().fg_dim) },
            ),
            Span::styled("[", theme::mute()),
            Span::styled(
                if val.is_empty() { " ".to_string() } else { val },
                if active { theme::fg(pal().fg).bg(pal().selection) } else { theme::fg(pal().fg_soft) },
            ),
            Span::styled("]", theme::mute()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Resume recent (Tab · ↑/↓ · Enter):",
        if app.newdlg.field == 2 { theme::bold(pal().cyan) } else { theme::fg(pal().fg_dim) },
    )));
    if app.newdlg.recent.is_empty() {
        lines.push(Line::from(Span::styled(
            "    no previous sessions",
            theme::mute(),
        )));
    }
    let visible = recent_h as usize;
    let offset = window_offset(app.newdlg.recent_selected, app.newdlg.recent.len(), visible);
    for (i, sess) in app.newdlg.recent.iter().enumerate().skip(offset) {
        if !lines_drawable(lines.len(), inner.height as usize) {
            break;
        }
        let is_sel = app.newdlg.field == 2 && i == app.newdlg.recent_selected;
        let ago = rel_time(sess.updated_ms);
        let title = truncate(
            &if sess.title.is_empty() { "untitled".to_string() } else { sess.title.clone() },
            w.saturating_sub(16).max(8),
        );
        let mut spans = vec![
            Span::styled("    ".to_string(), Style::default()),
            Span::styled(
                if is_sel { "▸ " } else { "  " }.to_string(),
                theme::fg(pal().blue),
            ),
            Span::styled(
                title,
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
            Span::styled(
                format!("  {ago}"),
                if is_sel { theme::fg(pal().fg_dim).bg(pal().selection) } else { theme::mute() },
            ),
        ];
        if is_sel {
            for s in &mut spans {
                s.style = s.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        "  Tab next field · Enter create/resume · Esc cancel",
        theme::dim(),
    )));
    f.render_widget(Paragraph::new(lines), inner);

    let cx = match app.newdlg.field {
        0 => inner.x + 13 + app.newdlg.name.cursor as u16,
        1 => inner.x + 13 + app.newdlg.dir.chars().count() as u16,
        _ => return,
    };
    let cy = inner.y + app.newdlg.field as u16;
    if cx < inner.x + inner.width {
        f.set_cursor_position((cx, cy));
    }
}

fn rel_time(ms: Option<i64>) -> String {
    let Some(ms) = ms else { return String::new() };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let secs = ((now - ms) / 1000).max(0);
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

pub fn render_rename(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let rect = centered_rect(44, 7, area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("Rename Session", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 14 {
        return;
    }
    let line = Line::from(vec![
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)),
            Span::styled(
            if app.rename.input.is_empty() {
                "new name…".to_string()
            } else {
                truncate(app.rename.input.text(), inner.width as usize - 4)
            },
            if app.rename.input.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        ),
    ]);
    f.render_widget(
        Paragraph::new(line),
        Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: 1 },
    );
    let cx = inner.x + 2 + app.rename.input.cursor as u16;
    if cx < inner.x + inner.width {
        f.set_cursor_position((cx, inner.y));
    }
}

pub fn render_confirm_quit(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let rect = centered_rect(46, 5, area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("Quit", theme::bold(pal().fg)),
        ]),
    );
    let n = app.working_count();
    let lines = vec![
        Line::from(Span::styled(
            format!("{n} agent{} still working — quit anyway?", if n == 1 { " is" } else { "s are" }),
            theme::fg(pal().fg),
        )),
        Line::from(Span::styled("  y quit · Esc stay", theme::dim())),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_keymap(f: &mut ratatui::Frame, app: &App, area: Rect) {
    use crate::keys::Action;
    let actions = Action::ALL;
    let capturing = app.keymap_ui.capturing;
    let rows = actions.len() as u16 + 4;
    let rect = centered_rect_w(52, rows.min(area.height.saturating_sub(2)), area);
    let inner = surface(
        f,
        rect,
        title_line(vec![Span::styled("Keybindings", theme::bold(pal().fg))]),
    );
    if inner.width < 20 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    match capturing {
        Some(action) => {
            lines.push(Line::from(Span::styled(
                format!(
                    "Press a new key for “{}”…  (Esc to cancel)",
                    action.label()
                ),
                theme::bold(pal().yellow),
            )));
            lines.push(Line::from(""));
        }
        None => {
            lines.push(Line::from(Span::styled(
                "  ↑/↓ select · Enter rebind · r reset · Esc close",
                theme::mute(),
            )));
            lines.push(Line::from(""));
        }
    }

    let visible = inner.height as usize - lines.len();
    let sel = app.keymap_ui.selected.min(actions.len().saturating_sub(1));
    let offset = window_offset(sel, actions.len(), visible);
    for (i, action) in actions.iter().enumerate().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let is_sel = i == sel && capturing.is_none();
        let binding = app.keys.binding_str(*action);
        let label = format!("  {:<26}", action.label());
        let mut spans = vec![
            Span::styled(
                label,
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
            Span::styled(
                format!("{:>width$} ", binding, width = w.saturating_sub(30)),
                if is_sel { theme::fg(pal().cyan) } else { theme::fg(pal().fg_dim) },
            ),
        ];
        if is_sel {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }

    // Fixed contextual keys (not rebindable).
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  fixed: Alt+←↑→↓ focus · Alt+H/J/K/L move · Alt+H/J/K/L resize",
        theme::mute(),
    )));
    lines.push(Line::from(Span::styled(
        "  fixed: PgUp/PgDn scroll · ↑/↓ tool · d diff · o open · a/A/r permission",
        theme::mute(),
    )));
    while lines.len() < inner.height as usize {
        lines.push(Line::from(Span::styled(String::new(), Style::default())));
    }
    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_theme_picker(
    f: &mut ratatui::Frame,
    app: &App,
    screen: Rect,
    anchor: Option<Rect>,
) {
    let names = crate::theme::theme_names();
    let visible = names.len().min(8).max(1);
    let h = (visible + 1) as u16;
    let rect = match anchor {
        Some(r) => Rect {
            x: r.x,
            y: r.y.saturating_sub(h).max(screen.y + 1),
            width: r.width.min(48),
            height: h,
        },
        None => centered_rect(40, h, screen),
    };
    if rect.width < 12 || rect.height < 2 {
        return;
    }
    let bg = Style::default().bg(pal().bg_float);
    let sel = app.theme_ui.selected.min(names.len().saturating_sub(1));
    let offset = window_offset(sel, names.len(), visible as usize);

    let q_line = Line::from(vec![
        Span::styled("  ".to_string(), bg),
        Span::styled(crate::theme::P_SYMBOL.to_string(), theme::bold(pal().cyan)).patch(bg),
        Span::styled(" Themes".to_string(), theme::bold(pal().fg)).patch(bg),
    ]);
    f.render_widget(
        Paragraph::new(q_line),
        Rect { x: rect.x, y: rect.y, width: rect.width, height: 1 },
    );

    for (row, i) in (offset..names.len()).take(visible as usize).enumerate() {
        let Some(name) = names.get(i) else { continue };
        let is_sel = i == sel;
        let is_current = *name == crate::theme::current_name();
        let mark = if is_current { "● " } else { "  " };
        let label = crate::theme::theme_label(name);
        let mut sp = vec![
            Span::styled("    ".to_string(), bg),
            Span::styled(mark.to_string(), theme::fg(pal().green)).patch(bg),
            Span::styled(
                truncate(&label, rect.width as usize - 10),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            )
            .patch(bg),
        ];
        if is_sel {
            for x in &mut sp {
                x.style = x.style.bg(pal().selection);
            }
        }
        let used: usize = sp.iter().map(|x| x.content.chars().count()).sum();
        if used < rect.width as usize {
            sp.push(Span::styled(
                " ".repeat(rect.width as usize - used),
                bg,
            ));
        }
        f.render_widget(
            Paragraph::new(Line::from(sp)),
            Rect { x: rect.x, y: rect.y + 1 + row as u16, width: rect.width, height: 1 },
        );
    }
}

pub fn render_model_picker(
    f: &mut ratatui::Frame,
    app: &App,
    screen: Rect,
    anchor: Option<Rect>,
) {
    let entries = app.filtered_picker_models(app.model_picker.input.text());
    let current = app
        .focused()
        .and_then(|s| s.model.as_ref())
        .map(|m| format!("{}/{}", m.provider_id, m.model_id))
        .unwrap_or_else(|| "default".to_string());
    let visible = entries.len().min(9).max(1);
    let h = (visible + 1) as u16;
    let rect = match anchor {
        Some(r) => Rect {
            x: r.x,
            y: r.y.saturating_sub(h).max(screen.y + 1),
            width: r.width.min(56),
            height: h,
        },
        None => centered_rect(40, h, screen),
    };
    if rect.width < 12 || rect.height < 2 {
        return;
    }
    let bg = Style::default().bg(pal().bg_float);
    let w = rect.width as usize;
    let q = app.model_picker.input.text();
    let mut spans = vec![
        Span::styled("  ".to_string(), bg),
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)).patch(bg),
        Span::styled(
            if q.is_empty() { "Search models…".to_string() } else { q.to_string() },
            if q.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        )
        .patch(bg),
    ];
    let used: usize = spans.iter().map(|x| x.content.chars().count()).sum();
    if used < w {
        spans.push(Span::styled(" ".repeat(w - used), bg));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x: rect.x, y: rect.y, width: rect.width, height: 1 },
    );
    let sel = app.model_picker.selected.min(entries.len().saturating_sub(1));
    let offset = window_offset(sel, entries.len(), visible as usize);
    for (row, i) in (offset..entries.len()).take(visible as usize).enumerate() {
        let Some((label, _)) = entries.get(i) else { continue };
        let is_sel = i == sel;
        let is_current = *label == current;
        let mark = if is_current { "● " } else { "  " };
        let mut sp = vec![
            Span::styled("    ".to_string(), bg),
            Span::styled(mark.to_string(), theme::fg(pal().green)).patch(bg),
            Span::styled(
                truncate(label, w - 10),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            )
            .patch(bg),
        ];
        if is_sel {
            for x in &mut sp {
                x.style = x.style.bg(pal().selection);
            }
        }
        let used: usize = sp.iter().map(|x| x.content.chars().count()).sum();
        if used < w {
            sp.push(Span::styled(" ".repeat(w - used), bg));
        }
        f.render_widget(
            Paragraph::new(Line::from(sp)),
            Rect { x: rect.x, y: rect.y + 1 + row as u16, width: rect.width, height: 1 },
        );
    }
    let cx = rect.x + 4 + app.model_picker.input.cursor as u16;
    if cx < rect.x + rect.width {
        f.set_cursor_position((cx, rect.y));
    }
}

pub fn render_agent_picker(
    f: &mut ratatui::Frame,
    app: &App,
    screen: Rect,
    anchor: Option<Rect>,
) {
    let entries = app.filtered_picker_agents(app.agent_picker.input.text());
    let current = app
        .focused()
        .and_then(|s| s.agent.clone())
        .unwrap_or_else(|| "default".to_string());
    let visible = entries.len().min(8).max(1);
    let h = (visible + 1) as u16;
    let rect = match anchor {
        Some(r) => Rect {
            x: r.x,
            y: r.y.saturating_sub(h).max(screen.y + 1),
            width: r.width.min(52),
            height: h,
        },
        None => centered_rect(40, h, screen),
    };
    if rect.width < 12 || rect.height < 2 {
        return;
    }
    let bg = Style::default().bg(pal().bg_float);
    let w = rect.width as usize;
    let q = app.agent_picker.input.text();
    let mut spans = vec![
        Span::styled("  ".to_string(), bg),
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)).patch(bg),
        Span::styled(
            if q.is_empty() { "Search agents…".to_string() } else { q.to_string() },
            if q.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        )
        .patch(bg),
    ];
    let used: usize = spans.iter().map(|x| x.content.chars().count()).sum();
    if used < w {
        spans.push(Span::styled(" ".repeat(w - used), bg));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x: rect.x, y: rect.y, width: rect.width, height: 1 },
    );
    let sel = app.agent_picker.selected.min(entries.len().saturating_sub(1));
    let offset = window_offset(sel, entries.len(), visible as usize);
    for (row, i) in (offset..entries.len()).take(visible as usize).enumerate() {
        let Some((name, desc)) = entries.get(i) else { continue };
        let is_sel = i == sel;
        let is_current = *name == current;
        let mark = if is_current { "● " } else { "  " };
        let mut sp = vec![
            Span::styled("    ".to_string(), bg),
            Span::styled(mark.to_string(), theme::fg(pal().green)).patch(bg),
            Span::styled(
                truncate(name, 12),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().cyan) },
            )
            .patch(bg),
            Span::styled(
                format!("  {}", truncate(desc, w - 22)),
                if is_sel { theme::fg(pal().fg) } else { theme::fg(pal().fg_soft) },
            )
            .patch(bg),
        ];
        if is_sel {
            for x in &mut sp {
                x.style = x.style.bg(pal().selection);
            }
        }
        let used: usize = sp.iter().map(|x| x.content.chars().count()).sum();
        if used < w {
            sp.push(Span::styled(" ".repeat(w - used), bg));
        }
        f.render_widget(
            Paragraph::new(Line::from(sp)),
            Rect { x: rect.x, y: rect.y + 1 + row as u16, width: rect.width, height: 1 },
        );
    }
    let cx = rect.x + 4 + app.agent_picker.input.cursor as u16;
    if cx < rect.x + rect.width {
        f.set_cursor_position((cx, rect.y));
    }
}

pub fn render_session_list(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let order = app.grid.order();
    let h = (order.len().min(12) + 4) as u16 + 1;
    let rect = centered_rect(56, h.max(6), area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("  Switch Session", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 16 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let visible = inner.height as usize - 1;
    let offset = window_offset(app.session_list.selected, order.len(), visible);
    for (i, sid) in order.iter().enumerate().skip(offset) {
        if lines.len() + 1 >= inner.height as usize {
            break;
        }
        let Some(sess) = app.session(*sid) else { continue };
        let is_sel = i == app.session_list.selected.min(order.len().saturating_sub(1));
        let is_focus = *sid == app.focus;
        let glyph = match &sess.status {
            crate::session::SessStatus::Idle => "○",
            crate::session::SessStatus::Working | crate::session::SessStatus::Thinking => "●",
            crate::session::SessStatus::Connecting => "◦",
            crate::session::SessStatus::Error(_) => "✗",
            crate::session::SessStatus::Retrying(_) => "↻",
            crate::session::SessStatus::Permission => "!",
        };
        let glyph_color = match &sess.status {
            crate::session::SessStatus::Working | crate::session::SessStatus::Thinking => {
                Style::default().fg(theme::spin_rgb(app.tick))
            }
            crate::session::SessStatus::Error(_) => theme::fg(pal().red),
            _ => theme::fg(pal().fg_soft),
        };
        let name = truncate(&sess.name, w.saturating_sub(20).max(8));
        let dir = theme::abbreviate_path(&sess.dir.to_string_lossy());
        let dir = truncate(&dir, w.saturating_sub(name.chars().count() + 14).max(6));
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(
                if is_focus { "▸ " } else { "  " }.to_string(),
                theme::fg(pal().blue),
            ),
            Span::styled(glyph.to_string(), glyph_color),
            Span::styled(
                format!(" {name}"),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
            Span::styled(
                format!("  {dir}"),
                if is_sel { theme::fg(pal().fg_dim).bg(pal().selection) } else { theme::mute() },
            ),
        ];
        if is_sel {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        "  ↑/↓ select · Enter switch · Tab cycles · Esc",
        theme::mute(),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_resume_picker(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let n = app.resume_picker.items.len().min(14);
    let rect = centered_rect(42, (n + 4) as u16, area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("  Resume Session", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 16 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if !app.resume_picker.loaded {
        lines.push(Line::from(Span::styled(
            format!("  loading sessions… {}", theme::spin(0)),
            theme::dim(),
        )));
    } else if app.resume_picker.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no previous sessions for this directory",
            theme::dim(),
        )));
    }
    let max_shown = inner.height as usize;
    let offset = window_offset(app.resume_picker.selected, app.resume_picker.items.len(), max_shown);
    for (i, sess) in app.resume_picker.items.iter().enumerate().skip(offset) {
        if lines.len() >= max_shown {
            break;
        }
        let is_sel = i == app.resume_picker.selected.min(app.resume_picker.items.len().saturating_sub(1));
        let ago = rel_time(sess.updated_ms);
        let title = truncate(
            &if sess.title.is_empty() { "untitled".to_string() } else { sess.title.clone() },
            w.saturating_sub(12).max(8),
        );
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(
                if is_sel { "▸ " } else { "  " }.to_string(),
                theme::fg(pal().blue),
            ),
            Span::styled(
                title,
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
            Span::styled(
                format!("  {ago}"),
                if is_sel { theme::fg(pal().fg_dim).bg(pal().selection) } else { theme::mute() },
            ),
        ];
        if is_sel {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_layout_picker(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let schemes = crate::panes::Scheme::all();
    let hint1 = "  move pane   Alt+Shift+H J K L";
    let hint2 = "  resize pane Alt+H J K L";
    let content_w = schemes
        .iter()
        .map(|s| s.label().len() + 6)
        .chain([hint1.len() + 1, hint2.len() + 1])
        .max()
        .unwrap_or(24) as u16
        + 4;
    let rect = centered_rect_w(content_w, (schemes.len() + 4) as u16 + 3, area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("  Tiling", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 14 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, scheme) in schemes.iter().enumerate() {
        let is_sel = i == app.layout_picker.selected.min(schemes.len() - 1);
        let is_current = *scheme == app.grid.scheme;
        let mark = if is_current { "● " } else { "  " };
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(mark.to_string(), theme::fg(pal().green)).patch(bg_of()),
            Span::styled(
                scheme.label().to_string(),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
        ];
        if is_sel {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(hint1, theme::mute())));
    lines.push(Line::from(Span::styled(hint2, theme::mute())));
    f.render_widget(Paragraph::new(lines), inner);
}

fn bg_of() -> Style {
    Style::default().bg(pal().bg_float)
}

pub fn render_file_search(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let h = (app.file_search.results.len().min(14) + 3) as u16 + 3;
    let rect = centered_rect(56, h.max(7), area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("  Search files", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 14 {
        return;
    }
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)),
        Span::styled(
            if app.file_search.input.is_empty() {
                "Search files…".to_string()
            } else {
                app.file_search.input.text().to_string()
            },
            if app.file_search.input.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        ),
    ])];
    lines.push(Line::from(Span::styled("─".repeat(w - 2), theme::mute())));
    if app.file_search.results.is_empty() {
        lines.push(Line::from(Span::styled("  no matches", theme::dim())));
    }
    let visible = inner.height as usize - 2;
    let offset = window_offset(app.file_search.selected, app.file_search.results.len(), visible);
    for (i, p) in app.file_search.results.iter().enumerate().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let selected = i == app.file_search.selected.min(app.file_search.results.len().saturating_sub(1));
        let path = truncate(p, w.saturating_sub(5));
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(
                path,
                if selected { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
        ];
        if selected {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
    let cx = inner.x + 2 + app.file_search.input.cursor as u16;
    if cx < inner.x + inner.width {
        f.set_cursor_position((cx, inner.y));
    }
}

pub fn render_project_search(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let h = (app.proj_search.results.len().min(14) + 3) as u16 + 3;
    let rect = centered_rect(64, h.max(7), area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("  Search project", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 14 {
        return;
    }
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)),
        Span::styled(
            if app.proj_search.input.is_empty() {
                "Search project…".to_string()
            } else {
                app.proj_search.input.text().to_string()
            },
            if app.proj_search.input.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        ),
    ])];
    lines.push(Line::from(Span::styled("─".repeat(w - 2), theme::mute())));
    if app.proj_search.results.is_empty() {
        lines.push(Line::from(Span::styled(
            if app.proj_search.input.is_empty() {
                "  type to search"
            } else {
                "  no matches"
            },
            theme::dim(),
        )));
    }
    let visible = inner.height as usize - 2;
    let offset = window_offset(app.proj_search.selected, app.proj_search.results.len(), visible);
    for (i, m) in app.proj_search.results.iter().enumerate().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let selected = i == app.proj_search.selected.min(app.proj_search.results.len().saturating_sub(1));
        let file = m.path.rsplit('/').next().unwrap_or(&m.path);
        let prefix = format!("{file}:{}", m.line);
        let text = truncate(m.text.trim(), w.saturating_sub(prefix.chars().count() + 8).max(8));
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(
                format!("{prefix} "),
                if selected { theme::bold(pal().cyan) } else { theme::fg(pal().cyan) },
            ),
            Span::styled(
                text,
                if selected { theme::fg(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
        ];
        if selected {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
    let cx = inner.x + 2 + app.proj_search.input.cursor as u16;
    if cx < inner.x + inner.width {
        f.set_cursor_position((cx, inner.y));
    }
}

pub fn render_conv_search(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let h = (app.conv_search.matches.len().min(12) + 3) as u16 + 3;
    let rect = centered_rect(60, h.max(7), area);
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled("  Search conversation", theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 14 {
        return;
    }
    let w = inner.width as usize;
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().cyan)),
        Span::styled(
            if app.conv_search.input.is_empty() {
                "Search conversation…".to_string()
            } else {
                app.conv_search.input.text().to_string()
            },
            if app.conv_search.input.is_empty() { theme::dim() } else { theme::fg(pal().fg) },
        ),
    ])];
    lines.push(Line::from(Span::styled("─".repeat(w - 2), theme::mute())));
    if app.conv_search.matches.is_empty() {
        lines.push(Line::from(Span::styled(
            if app.conv_search.input.is_empty() {
                "  type to search"
            } else {
                "  no matches"
            },
            theme::dim(),
        )));
    }
    let visible = inner.height as usize - 2;
    let offset = window_offset(app.conv_search.selected, app.conv_search.matches.len(), visible);
    for (i, m) in app.conv_search.matches.iter().enumerate().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let selected = i == app.conv_search.selected.min(app.conv_search.matches.len().saturating_sub(1));
        let label = format!("{:>8}", m.label);
        let excerpt = truncate(&m.excerpt, w.saturating_sub(14).max(8));
        let mut spans = vec![
            Span::styled("  ".to_string(), Style::default()),
            Span::styled(
                format!("{label}  "),
                if selected { theme::bold(pal().cyan) } else { theme::fg(pal().cyan) },
            ),
            Span::styled(
                excerpt,
                if selected { theme::fg(pal().fg) } else { theme::fg(pal().fg_soft) },
            ),
        ];
        if selected {
            for x in &mut spans {
                x.style = x.style.bg(pal().selection);
            }
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
    let cx = inner.x + 2 + app.conv_search.input.cursor as u16;
    if cx < inner.x + inner.width {
        f.set_cursor_position((cx, inner.y));
    }
}

pub fn render_viewer(f: &mut ratatui::Frame, area: Rect, v: &mut crate::app::ViewerState) {
    let rect = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled(truncate(&v.title, rect.width.saturating_sub(6) as usize), theme::bold(pal().fg)),
        ]),
    );
    if inner.width < 8 || inner.height == 0 {
        return;
    }
    if let Some(line) = v.jump_line {
        let h = inner.height as usize;
        v.scroll = (line as usize).saturating_sub(h / 3);
        v.jump_line = None;
    }
    let digits = (v.lines.len().max(1)).to_string().len();
    let w = inner.width as usize;
    let h = inner.height as usize;
    let max_scroll = v.lines.len().saturating_sub(h);
    v.scroll = v.scroll.min(max_scroll);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, row) in v.lines.iter().enumerate().skip(v.scroll) {
        if lines.len() >= h {
            break;
        }
        let num = format!("{:>width$} ", i + 1, width = digits);
        let mut spans = vec![
            Span::styled(format!("  {num}"), theme::mute()),
        ];
        spans.extend(row.iter().cloned());
        if spans.iter().map(|s| s.content.chars().count()).sum::<usize>() > w {
            let mut kept: Vec<Span<'static>> = Vec::new();
            let mut used = 0;
            for s in spans {
                let len = s.content.chars().count();
                if used + len > w {
                    let take = w.saturating_sub(used);
                    let content: String = s.content.chars().take(take).collect();
                    kept.push(Span::styled(content, s.style));
                    break;
                }
                used += len;
                kept.push(s);
            }
            spans = kept;
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_diff(f: &mut ratatui::Frame, area: Rect, d: &crate::app::DiffState) {
    let rect = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    let inner = surface(
        f,
        rect,
        title_line(vec![
            Span::styled(truncate(&d.title, rect.width.saturating_sub(6) as usize), theme::bold(pal().fg)),
        ]),
    );
    let w = inner.width as usize;
    let h = inner.height as usize;
    let max_scroll = d.lines.len().saturating_sub(h);
    let scroll = d.scroll.min(max_scroll);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for row in d.lines.iter().skip(scroll) {
        if lines.len() >= h {
            break;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        for s in &row.spans {
            let len = s.content.chars().count();
            if used + len > w {
                let take = w.saturating_sub(used);
                let content: String = s.content.chars().take(take).collect();
                spans.push(Span::styled(content, s.style));
                break;
            }
            used += len;
            spans.push(s.clone());
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
