//! Session pane frame: header title, transcript, input box.

use crate::app::App;

use crate::session::{SessionState, SessStatus};
use crate::theme::{pal, self};
use crate::ui::conversation;
use ratatui::layout::Rect;
use ratatui::style::{Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

pub fn render(f: &mut ratatui::Frame, app: &mut App, area: Rect, sid: u32, focused: bool) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let Some(sess) = app.session(sid) else { return };
    if area.width < 8 || area.height < 3 {
        return;
    }

    let busy = sess.status.is_busy();
    let border = if focused {
        pal().border_focus
    } else if busy {
        pal().border_busy
    } else {
        pal().border
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(pal().bg))
        .title(pane_title(sess, focused, area.width, app.tick));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let allow_cursor = focused
        && app.overlay == crate::app::Overlay::None
        && app.viewer.is_none()
        && app.diff.is_none();

    if inner.height < 3 || inner.width < 6 {
        return;
    }

    // Layout: conversation (min), separator row, input rows.
    let w = inner.width as usize;
    let input_rows = input_height(sess, w, inner.height as usize);
    let conv_h = inner.height.saturating_sub(input_rows);
    let conv_area = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: conv_h,
    };

    // Cache rebuild (drop the session borrow first).
    let width = conv_area.width.max(10);
    let stale = {
        let dirty = app.session(sid).map(|s| s.dirty).unwrap_or(true);
        match app.conv_cache.get(&sid) {
            Some(c) => c.width != width || dirty || (c.animating && c.built_at_tick != app.tick),
            None => true,
        }
    };
    if stale {
        if let Some(s) = app.session(sid) {
            let cache = conversation::build_cache(s, width, app.tick);
            app.conv_cache.insert(sid, cache);
        }
        if let Some(s) = app.session_mut(sid) {
            s.dirty = false;
        }
    }

    let cache_empty = app
        .conv_cache
        .get(&sid)
        .map(|c| c.lines.is_empty())
        .unwrap_or(true);
    if cache_empty {
        let name = app.session(sid).map(|s| s.name.clone()).unwrap_or_default();
        let pseudo = crate::session::SessionState::new(sid, name, std::path::PathBuf::new());
        empty_hint(f, conv_area, &pseudo);
    } else if let (Some(s), Some(cache)) = (app.session(sid), app.conv_cache.get(&sid)) {
        if conv_area.height > 0 {
            conversation::render(f, conv_area, s, cache);
        }
    }

    // Input area.
    let input_area = Rect {
        x: inner.x + 1,
        y: inner.y + conv_h,
        width: inner.width.saturating_sub(2),
        height: input_rows,
    };

    // Slash-command popup above the input.
    let input_text = app
        .session(sid)
        .map(|s| s.input.text().to_string())
        .unwrap_or_default();
    if focused && input_text.trim_start().starts_with('/') {
        let matches = crate::app::slash_matches(&input_text, app);
        if !matches.is_empty() {
            let sel = app.session(sid).map(|s| s.slash_selected).unwrap_or(0);
            render_slash_popup(f, &matches, sel.min(matches.len() - 1), input_area);
        }
    }

    if input_area.height > 0 {
        let sess = app.session(sid).unwrap();
        render_input(f, app, sess, input_area, focused, allow_cursor);
    }
}

fn render_slash_popup(
    f: &mut ratatui::Frame,
    matches: &[crate::app::SlashItem],
    selected: usize,
    input_area: Rect,
) {
    if input_area.width < 16 || input_area.y == 0 {
        return;
    }
    let bg = Style::default().bg(pal().bg_float);
    let visible = (input_area.y as usize).min(8).min(matches.len());
    let offset = if matches.len() <= visible {
        0
    } else {
        selected.saturating_sub(visible / 2).min(matches.len() - visible)
    };
    let h = visible as u16;
    let y = input_area.y.saturating_sub(h);

    for (row, i) in (offset..offset + visible).enumerate() {
        let Some(item) = matches.get(i) else { continue };
        let is_sel = i == selected;
        let w = input_area.width as usize;
        let name = conversation::truncate(&format!("/{}", item.name), 14);
        let desc = conversation::truncate(&item.desc, w.saturating_sub(22).max(8));
        let mut spans = vec![
            Span::styled("▌".to_string(), Style::default().fg(pal().border_focus).bg(pal().bg_float)),
            Span::styled("    ".to_string(), bg),
            Span::styled(
                format!("{name:<14}"),
                if is_sel { theme::bold(pal().cyan) } else { theme::fg(pal().cyan) },
            )
            .patch(bg),
            Span::styled(
                format!(" {}", desc),
                if is_sel { theme::fg(pal().fg) } else { theme::fg(pal().fg_soft) },
            )
            .patch(bg),
        ];
        if is_sel {
            for sp in &mut spans {
                sp.style = sp.style.bg(pal().selection);
            }
        }
        // fill the row with panel background
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        if used < w {
            spans.push(Span::styled(" ".repeat(w - used), bg));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x: input_area.x, y: y + row as u16, width: input_area.width, height: 1 },
        );
    }
}

/// Render a hint for empty transcripts: the Th. mark + plain description.
pub fn empty_hint(f: &mut ratatui::Frame, area: Rect, sess: &SessionState) {
    if area.height < 4 || area.width < 20 {
        return;
    }
    let hint = Line::from(Span::styled(
        format!("Describe a task for {}", sess.name),
        theme::dim(),
    ));

    let mark_h = crate::theme::MARK_H as u16;
    let icon_w = crate::theme::MARK_W as u16;
    if area.height >= mark_h + 3 && area.width >= icon_w + 4 {
        // Left-aligned, like the homescreen.
        let total = mark_h + 2;
        let y = area.y + (area.height - total) / 2;
        let x = area.x + 4;
        for i in 0..crate::theme::MARK_H {
            f.render_widget(
                Paragraph::new(crate::theme::theta_mark_line(i)),
                Rect { x, y: y + i as u16, width: icon_w.min(area.width), height: 1 },
            );
        }
        f.render_widget(
            Paragraph::new(hint),
            Rect {
                x: area.x + 4,
                y: y + mark_h + 1,
                width: area.width.saturating_sub(4),
                height: 1,
            },
        );
        return;
    }

    let lines = vec![Line::from(""), Line::from(hint)];
    let hh = lines.len() as u16;
    let v = (area.height as usize).saturating_sub(lines.len()) / 2;
    f.render_widget(
        Paragraph::new(lines).style(Style::default()),
        Rect {
            x: area.x,
            y: area.y + v as u16,
            width: area.width,
            height: hh,
        },
    );
}
trait SpanExt {
    fn patch(self, style: Style) -> Self;
}
impl SpanExt for Span<'static> {
    fn patch(self, style: Style) -> Self {
        Span::styled(self.content, self.style.patch(style))
    }
}

fn pane_title(sess: &SessionState, focused: bool, width: u16, tick: u64) -> Line<'static> {
    let (glyph, gstyle) = match &sess.status {
        SessStatus::Connecting => (theme::spin(tick).to_string(), theme::fg(pal().fg_dim)),
        SessStatus::Idle => ("○".to_string(), theme::mute()),
        SessStatus::Working | SessStatus::Thinking => {
            // The focused pane's spinner lives on the in-flight message in
            // the chat; keep the title static to avoid double animation.
            if focused {
                ("●".to_string(), Style::default().fg(pal().cyan))
            } else {
                (
                    theme::spin(tick).to_string(),
                    Style::default().fg(theme::spin_rgb(tick)),
                )
            }
        }
        SessStatus::Retrying(_) => ("↻".to_string(), theme::fg(pal().yellow)),
        SessStatus::Error(_) => ("✗".to_string(), theme::fg(pal().red)),
        SessStatus::Permission => ("!".to_string(), theme::fg(pal().yellow)),
    };
    let name_style = if focused {
        theme::bold(pal().fg)
    } else {
        theme::fg(pal().fg_soft)
    };
    let max_name = width.saturating_sub(8) as usize;
    let name = conversation::truncate(&sess.name, max_name);

    let left = vec![
        Span::styled(" ".to_string(), Style::default()),
        Span::styled(crate::theme::P_SYMBOL.to_string(), theme::fg(if sess.status.is_busy() { pal().cyan } else { pal().blue })),
        Span::styled(" ".to_string(), Style::default()),
        Span::styled(name, name_style),
    ];
    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len = 1;
    let pad = (width as usize).saturating_sub(left_len + right_len + 2);
    let mut spans = left;
    spans.push(Span::styled(" ".repeat(pad), Style::default()));
    spans.push(Span::styled(glyph, gstyle));
    spans.push(Span::styled(" ", Style::default()));
    Line::from(spans)
}

pub fn input_height(sess: &SessionState, w: usize, total_h: usize) -> u16 {
    // Box: input at top + blank + footer + bottom pad, min 4 rows.
    let max = (total_h.saturating_sub(1)).max(4).min(9) as usize;
    if sess.pending_perm.is_some() {
        return 4usize.min(max) as u16;
    }
    let text_rows = if sess.input.is_empty() {
        1
    } else {
        conversation::wrap_spans(
            &[Span::raw(sess.input.buf.clone())],
            w.saturating_sub(4).max(8),
        )
        .len()
        .min(6)
    };
    ((text_rows + 3).min(max).max(4)) as u16
}

fn render_input(
    f: &mut ratatui::Frame,
    app: &App,
    sess: &SessionState,
    area: Rect,
    focused: bool,
    allow_cursor: bool,
) {
    if area.width < 8 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    let bg = Style::default().bg(pal().bg_float);
    let bar_color = if focused { pal().border_focus } else { pal().border };
    let bar_style = Style::default().fg(bar_color).bg(pal().bg_float);

    // Panel fill for every row (footer overwrites its own row after).
    for r in 0..area.height {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▌".to_string(), bar_style),
                Span::styled(" ".repeat(w.saturating_sub(1)), bg),
            ])),
            Rect { x: area.x, y: area.y + r, width: area.width, height: 1 },
        );
    }

    // Footer (one above the bottom pad): agent · model left, folder · usage right.
    let agent = sess.agent.clone().unwrap_or_else(|| "build".to_string());
    let agent_len = agent.chars().count();
    let model_label = sess
        .model
        .as_ref()
        .map(|m| format!("{}/{}", m.provider_id, m.model_id))
        .or_else(|| {
            app.default_model
                .as_ref()
                .map(|m| format!("{}/{}", m.provider_id, m.model_id))
        })
        .unwrap_or_else(|| "default model".to_string());
    let dir = theme::abbreviate_path(&sess.dir.to_string_lossy());

    let mut right_parts: Vec<String> = vec![dir];
    if sess.ctx_tokens > 0 {
        right_parts.push(format!(
            "{} ctx",
            crate::ui::statusbar::fmt_tokens(sess.ctx_tokens)
        ));
    }
    if sess.cost > 0.0 {
        right_parts.push(format!("${:.4}", sess.cost));
    }
    if sess.pending_perm.is_some() {
        right_parts.push("a allow · A always · r reject".into());
    } else if sess.status.is_busy() {
        right_parts.push("ctrl+c interrupt".into());
    }
    let right = right_parts.join(" · ");

    let left_len = agent_len + 3 + model_label.chars().count();
    let right_fit = conversation::truncate(&right, w.saturating_sub(left_len + 4).max(4));
    let gap = w
        .saturating_sub(2)
        .saturating_sub(left_len)
        .saturating_sub(right_fit.chars().count());
    let footer = Line::from(vec![
        Span::styled("▌".to_string(), bar_style),
        Span::styled(" ".to_string(), bg),
        Span::styled(agent.clone(), theme::fg(pal().purple)).patch(bg),
        Span::styled(" · ".to_string(), theme::mute()).patch(bg),
        Span::styled(
            conversation::truncate(
                &model_label,
                w.saturating_sub(agent_len + 8).max(6),
            ),
            theme::fg(pal().fg_soft),
        )
        .patch(bg),
        Span::styled(" ".repeat(gap.min(w)), bg),
        Span::styled(right_fit, theme::mute()).patch(bg),
        Span::styled(" ".to_string(), bg),
    ]);
    let footer_y = area.y + area.height.saturating_sub(1);
    f.render_widget(
        Paragraph::new(footer),
        Rect { x: area.x, y: footer_y, width: area.width, height: 1 },
    );

    // Content rows: input at the top of the box, then blank before the footer.
    let body = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(1),
        height: area.height.saturating_sub(3),
    };
    if body.height == 0 || body.width < 6 {
        return;
    }

    if let Some(perm) = &sess.pending_perm {
        let detail = conversation::truncate(
            &format!("{} {}", perm.kind, perm.detail),
            body.width as usize,
        );
        let l1 = Line::from(vec![
            Span::styled(format!("{} ", crate::theme::P_SYMBOL), theme::bold(pal().yellow)).patch(bg),
            Span::styled("Permission: ".to_string(), theme::bold(pal().yellow)).patch(bg),
            Span::styled(detail, theme::fg(pal().fg)).patch(bg),
        ]);
        let l2 = Line::from(Span::styled(
            "  [a] allow once   [A] always   [r] reject".to_string(),
            theme::dim().patch(bg),
        ));
        let mut rows = vec![Line::from(Span::styled(" ".to_string(), bg)), l1, l2];
        rows.truncate(body.height as usize);
        while rows.len() < body.height as usize {
            rows.push(Line::from(Span::styled(" ".to_string(), bg)));
        }
        f.render_widget(Paragraph::new(rows), body);
        return;
    }

    let prompt_style = if focused {
        theme::bold(pal().cyan)
    } else {
        theme::mute()
    };
    let avail_w = body.width as usize - 2;

    let mut lines: Vec<Line<'static>> = Vec::new();
    if sess.status == SessStatus::Connecting {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", theme::spin(app.tick)),
                Style::default().fg(theme::spin_rgb(app.tick)),
            )
            .patch(bg),
            Span::styled("connecting to opencode…".to_string(), theme::dim()).patch(bg),
        ]));
    } else if sess.input.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", crate::theme::P_SYMBOL), prompt_style).patch(bg),
            Span::styled("Ask this agent…".to_string(), theme::dim()).patch(bg),
        ]));
    } else {
        let buf = sess.input.buf.clone();
        let cursor = sess.input.cursor;
        let rows_wrapped = conversation::wrap_spans(
            &[Span::styled(buf.clone(), theme::fg(pal().fg))],
            avail_w,
        );
        let chars: Vec<char> = buf.chars().collect();
        let before: String = chars[..cursor.min(chars.len())].iter().collect();
        let cursor_row = conversation::wrap_spans(&[Span::raw(before)], avail_w)
            .len()
            .saturating_sub(1);
        let max_rows = body.height as usize;
        let start = if cursor_row + 1 >= max_rows {
            cursor_row + 1 - max_rows
        } else {
            0
        };
        for (i, row) in rows_wrapped.iter().enumerate().skip(start) {
            if lines.len() >= max_rows {
                break;
            }
            let mark = if i == start || rows_wrapped.len() == 1 {
                prompt_style
            } else {
                theme::mute()
            };
            let mut spans: Vec<Span<'static>> =
                vec![Span::styled(format!("{} ", crate::theme::P_SYMBOL), mark).patch(bg)];
            spans.extend(row.iter().cloned().map(|sp| sp.patch(bg)));
            lines.push(Line::from(spans));
        }
    }
    while lines.len() < body.height as usize {
        lines.push(Line::from(Span::styled(" ".to_string(), bg)));
    }
    lines.truncate(body.height as usize);
    f.render_widget(Paragraph::new(lines), body);

    if allow_cursor && !sess.input.is_empty() {
        let chars: Vec<char> = sess.input.buf.chars().collect();
        let before: String = chars[..sess.input.cursor.min(chars.len())].iter().collect();
        let cursor_row = conversation::wrap_spans(&[Span::raw(before)], avail_w)
            .len()
            .saturating_sub(1);
        let max_rows = body.height as usize;
        let row_start = if cursor_row + 1 >= max_rows {
            cursor_row + 1 - max_rows
        } else {
            0
        };
        let row_start_chars: usize = conversation::wrap_spans(
            &[Span::raw(sess.input.buf.clone())],
            avail_w,
        )
        .iter()
        .take(cursor_row)
        .map(|r| r.iter().map(|sp| sp.content.chars().count()).sum::<usize>())
        .sum();
        let col = sess.input.cursor.saturating_sub(row_start_chars);
        let vis_row = cursor_row.min(row_start + max_rows - 1) - row_start;
        let px = body.x + 2 + (col as u16).min(avail_w as u16);
        let py = body.y + vis_row as u16;
        if px < body.x + body.width && py < body.y + body.height {
            f.set_cursor_position((px, py));
        }
    }
}

