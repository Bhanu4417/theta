use crate::app::App;

use crate::session::{SessionState, SessStatus};
use crate::theme::{pal, self, SpanExt};
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

    let w = inner.width as usize;
    let input_rows = input_height(sess, w, inner.height as usize);
    let conv_h = inner.height.saturating_sub(input_rows);
    let conv_area = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: conv_h,
    };

    let width = conv_area.width.max(10);
    let stale = {
        let dirty = app.session(sid).map(|s| s.dirty).unwrap_or(true);
        match app.conv_cache.get(&sid) {
            Some(c) => c.width != width || dirty || (c.animating && c.built_at_tick != app.tick),
            None => true,
        }
    };
    if stale {
        // Rebuild incrementally: hand the previous cache over so only the
        // messages that changed are re-rendered. Streaming a reply changes one
        // message, so a long transcript no longer re-renders on every token.
        let prev = app.conv_cache.get(&sid).cloned();
        if let Some(s) = app.session(sid) {
            let cache = conversation::rebuild(prev.as_ref(), s, width, app.tick);
            app.conv_cache.insert(sid, cache);
        }
        if let Some(s) = app.session_mut(sid) {
            s.dirty = false;
        }
    }

    if let Some(cache) = app.conv_cache.get(&sid) {
        let h = conv_area.height as usize;
        let total = cache.lines.len();
        if h > 0 {
            let bottom = total.saturating_sub(h);
            if let Some(s) = app.session_mut(sid) {
                if !s.stick_bottom && s.scroll >= bottom {
                    s.stick_bottom = true;
                    s.scroll = bottom;
                }
            }
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
            let sel = app.select.as_ref().filter(|sel| sel.sid == sid).and_then(|sel| {
                let h = conv_area.height as usize;
                if h == 0 {
                    return None;
                }
                let total = cache.lines.len();
                let offset = conversation::view_offset(s.stick_bottom, s.scroll, total, h);
                let to_abs = |p: crate::app::SelectPoint| -> (usize, usize) {
                    let row = (p.row.saturating_sub(conv_area.y) as usize)
                        .min(h.saturating_sub(1));
                    let col = p.col.saturating_sub(conv_area.x) as usize;
                    (offset + row, col)
                };
                let (ar, ac) = to_abs(sel.anchor);
                let (hr, hc) = to_abs(sel.head);
                let ((r0, c0), (r1, c1)) = if (ar, ac) <= (hr, hc) {
                    ((ar, ac), (hr, hc))
                } else {
                    ((hr, hc), (ar, ac))
                };
                Some(conversation::SelRange { r0, c0, r1, c1 })
            });
            conversation::render(f, conv_area, s, cache, sel);
        }
    }

    let input_area = Rect {
        x: inner.x + 1,
        y: inner.y + conv_h,
        width: inner.width.saturating_sub(2),
        height: input_rows,
    };

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

    if focused {
        if let Some(s) = app.session(sid) {
            if !s.mention_results.is_empty() {
                render_mention_popup(f, &s.mention_results, s.mention_selected, input_area);
            }
        }
    }

    if input_area.height > 0 {
        let sess = app.session(sid).unwrap();
        render_input(f, app, sess, input_area, focused, allow_cursor);
    }
}

fn render_mention_popup(
    f: &mut ratatui::Frame,
    results: &[String],
    selected: usize,
    input_area: Rect,
) {
    if input_area.width < 18 || input_area.y == 0 {
        return;
    }
    let bg = Style::default().bg(pal().bg_float);
    let visible = (input_area.y as usize).min(8).min(results.len());
    if visible == 0 {
        return;
    }
    let offset = if results.len() <= visible {
        0
    } else {
        selected
            .saturating_sub(visible / 2)
            .min(results.len() - visible)
    };
    let y = input_area.y.saturating_sub(visible as u16);
    let w = input_area.width as usize;
    for (row, i) in (offset..offset + visible).enumerate() {
        let Some(path) = results.get(i) else { continue };
        let is_sel = i == selected;
        let mut spans = vec![
            Span::styled("▌".to_string(), Style::default().fg(pal().border_focus).bg(pal().bg_float)),
            Span::styled("  @".to_string(), theme::fg(pal().cyan).patch(bg)),
            Span::styled(
                conversation::truncate(path, w.saturating_sub(8)),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg_soft) },
            )
            .patch(bg),
        ];
        if is_sel {
            for s in &mut spans {
                s.style = s.style.bg(pal().selection);
            }
        }
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

    let lines = vec![Line::from(""), hint];
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
fn input_row_spans(
    prefix: String,
    mark: Style,
    row: &[Span<'static>],
    bar: Style,
    text: Style,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(prefix, mark.patch(bar))];
    spans.extend(row.iter().cloned().map(|sp| sp.patch(text)));
    spans
}

fn pane_title(sess: &SessionState, focused: bool, width: u16, tick: u64) -> Line<'static> {
    let (glyph, gstyle) = match &sess.status {
        SessStatus::Connecting => (theme::spin(tick).to_string(), theme::fg(pal().fg_dim)),
        SessStatus::Idle => ("○".to_string(), theme::mute()),
        SessStatus::Working | SessStatus::Thinking => {
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
        SessStatus::Question => ("?".to_string(), theme::fg(pal().purple)),
        SessStatus::Compacting => (
            theme::spin(tick).to_string(),
            Style::default().fg(theme::spin_rgb(tick)),
        ),
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

fn input_layout(buf: &str, width: usize, cursor: usize) -> (Vec<Vec<Span<'static>>>, usize, usize) {
    let width = width.max(4);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut crow = 0usize;
    let mut ccol = 0usize;
    let mut offset = 0usize;
    let mut matched = false;
    for line in buf.split('\n') {
        let line_len = line.chars().count();
        let wrapped = if line.is_empty() {
            vec![Vec::new()]
        } else {
            conversation::wrap_spans(&[Span::raw(line.to_string())], width)
        };
        if !matched && cursor >= offset && cursor <= offset + line_len {
            let start = rows.len();
            let mut rem = cursor - offset;
            let mut placed = false;
            for (ri, r) in wrapped.iter().enumerate() {
                let len: usize = r.iter().map(|s| s.content.chars().count()).sum();
                if rem <= len {
                    crow = start + ri;
                    ccol = rem;
                    placed = true;
                    break;
                }
                rem -= len;
            }
            if !placed {
                crow = start + wrapped.len().saturating_sub(1);
                ccol = wrapped
                    .last()
                    .map(|r| r.iter().map(|s| s.content.chars().count()).sum())
                    .unwrap_or(0);
            }
            matched = true;
        }
        rows.extend(wrapped);
        offset += line_len + 1; 
    }
    if !matched && !rows.is_empty() {
        crow = rows.len() - 1;
        ccol = rows.last().map(|r| r.iter().map(|s| s.content.chars().count()).sum()).unwrap_or(0);
    }
    (rows, crow, ccol)
}

pub fn input_height(sess: &SessionState, w: usize, total_h: usize) -> u16 {
    let max = (total_h.saturating_sub(1)).clamp(4, 18);
    if sess.pending_question.is_some() {
        let opts = sess
            .pending_question
            .as_ref()
            .and_then(|pq| pq.current())
            .map(|q| q.options.len().min(6))
            .unwrap_or(0);
        let custom = sess
            .pending_question
            .as_ref()
            .and_then(|pq| pq.current())
            .map(|q| q.custom)
            .unwrap_or(false) as usize;
        return ((opts + custom + 4 + 3).min(max).max(6)) as u16;
    }
    if sess.pending_perm.is_some() {
        return 6usize.min(max).max(5) as u16;
    }
    let queue_rows = sess.queue.len().min(4);
    let text_rows = if sess.input.is_empty() {
        1
    } else {
        input_layout(&sess.input.buf, w.saturating_sub(5).max(8), 0)
            .0
            .len()
            .min(8)
    };
    ((text_rows + queue_rows + 3).min(max).max(4)) as u16
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

    let fill_rows = area.height.saturating_sub(1);
    if fill_rows > 0 {
        let fill = Line::from(vec![
            Span::styled("▌".to_string(), bar_style),
            Span::styled(" ".repeat(w.saturating_sub(1)), bg),
        ]);
        let lines: Vec<Line<'static>> = std::iter::repeat_with(|| fill.clone())
            .take(fill_rows as usize)
            .collect();
        f.render_widget(
            Paragraph::new(lines),
            Rect { x: area.x, y: area.y, width: area.width, height: fill_rows },
        );
    }

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

    let mut right_parts: Vec<String> = Vec::new();
    if sess.ctx_tokens > 0 {
        right_parts.push(match context_limit_for(app, sess) {
            Some(lim) if lim > 0 => {
                let pct = (sess.ctx_tokens as f64 / lim as f64 * 100.0).min(999.0);
                format!("ctx {pct:.0}%")
            }
            _ => format!(
                "{} ctx",
                crate::ui::statusbar::fmt_tokens(sess.ctx_tokens)
            ),
        });
    }
    if sess.cost > 0.0 {
        right_parts.push(format!("${:.4}", sess.cost));
    }
    // Old tool output was rolled out of the prompt this turn. Worth showing:
    // it explains why an earlier command's output is no longer in context.
    if let Some((tokens, n)) = sess.last_prune {
        if tokens > 0 {
            right_parts.push(format!(
                "{} pruned ({n})",
                crate::ui::statusbar::fmt_tokens(tokens)
            ));
        }
    }
    right_parts.push(dir);
    if sess.interrupt_armed.is_some() {
        right_parts.push("press Esc again to interrupt".into());
    }
    if !sess.queue.is_empty() {
        right_parts.push(format!("{} queued", sess.queue.len()));
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

    let body = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(1),
        height: area.height.saturating_sub(3),
    };
    if body.height == 0 || body.width < 6 {
        return;
    }

    if sess.pending_perm.is_some() || sess.pending_question.is_some() {
        let rows = prompt_rows(sess, body.width as usize, body.height as usize, bg);
        f.render_widget(Paragraph::new(rows), body);
        return;
    }

    let prompt_style = if focused {
        theme::bold(pal().cyan)
    } else {
        theme::mute()
    };
    let input_style = theme::fg(pal().fg).patch(bg);
    let avail_w = body.width as usize - 2;
    let body_w = body.width as usize;

    let queue_shown = sess.queue.len().min(body.height.saturating_sub(1) as usize);
    let queue_h = queue_shown as u16;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for q in sess.queue.iter().take(queue_shown) {
        lines.push(queued_line(&q.display, body_w, bg));
    }

    let input_max = (body.height as usize).saturating_sub(queue_shown).max(1);
    let input_start = lines.len();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    let mut rendered_start = 0usize;
    if sess.status == SessStatus::Connecting {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", theme::spin(app.tick)),
                Style::default().fg(theme::spin_rgb(app.tick)),
            )
            .patch(bg),
            Span::styled("connecting…".to_string(), theme::dim()).patch(bg),
        ]));
    } else if sess.input.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", crate::theme::P_SYMBOL), prompt_style).patch(bg),
            Span::styled("Ask this agent…".to_string(), theme::dim()).patch(bg),
        ]));
    } else {
        let (rows_wrapped, cr, cc) =
            input_layout(&sess.input.buf, avail_w, sess.input.cursor);
        cursor_row = cr;
        cursor_col = cc;
        let max_rows = input_max;
        let start = (cursor_row + 1).saturating_sub(max_rows);
        rendered_start = start;
        for (i, row) in rows_wrapped.iter().enumerate().skip(start) {
            if lines.len() >= input_start + max_rows {
                break;
            }
            let (prefix, mark) = if i == 0 {
                (format!("{} ", crate::theme::P_SYMBOL), prompt_style)
            } else {
                ("  ".to_string(), theme::mute())
            };
            lines.push(Line::from(input_row_spans(prefix, mark, row, bg, input_style)));
        }
    }
    while lines.len() < body.height as usize {
        lines.push(Line::from(Span::styled(" ".to_string(), bg)));
    }
    lines.truncate(body.height as usize);
    f.render_widget(Paragraph::new(lines), body);

    if allow_cursor && sess.status != SessStatus::Connecting && sess.pending_perm.is_none() {
        let vis_row = cursor_row.saturating_sub(rendered_start);
        let px = body.x + 2 + (cursor_col as u16).min(avail_w as u16);
        let py = body.y + queue_h + vis_row as u16;
        if px < body.x + body.width && py < body.y + body.height {
            f.set_cursor_position((px, py));
        }
    }
}

fn context_limit_for(app: &App, sess: &SessionState) -> Option<u64> {
    let want = sess.model.as_ref().or(app.default_model.as_ref())?;
    app.providers
        .iter()
        .find(|p| p.provider_id == want.provider_id && p.model_id == want.model_id)
        .and_then(|p| p.context_limit)
}

fn queued_line(text: &str, w: usize, bg: Style) -> Line<'static> {
    let label = "— queued";
    let label_w = label.chars().count();
    let text_w = w.saturating_sub(label_w + 3).max(4);
    let flat = text.replace('\n', " ");
    let shown = conversation::truncate(&flat, text_w);
    let used = shown.chars().count();
    let gap = w.saturating_sub(1 + used + label_w);
    Line::from(vec![
        Span::styled(" ", bg),
        Span::styled(shown, theme::fg(pal().fg_soft)).patch(bg),
        Span::styled(" ".repeat(gap), bg),
        Span::styled(label.to_string(), theme::fg(pal().yellow)).patch(bg),
    ])
}

fn prompt_rows(sess: &SessionState, w: usize, h: usize, bg: Style) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = Vec::new();
    let text_w = w.saturating_sub(2).max(8);

    if let Some(perm) = &sess.pending_perm {
        rows.push(Line::from(vec![
            Span::styled("! ".to_string(), theme::bold(pal().yellow)).patch(bg),
            Span::styled("Allow ".to_string(), theme::bold(pal().fg)).patch(bg),
            Span::styled(perm.kind.clone(), theme::bold(pal().yellow)).patch(bg),
        ]));
        if !perm.detail.trim().is_empty() {
            let detail = conversation::truncate(&perm.detail, text_w);
            rows.push(Line::from(
                Span::styled(format!("  {detail}"), theme::fg(pal().fg_soft)).patch(bg),
            ));
        }
        rows.push(Line::from(vec![
            Span::styled("  ".to_string(), bg),
            Span::styled("[a]".to_string(), theme::bold(pal().green)).patch(bg),
            Span::styled(" allow once   ".to_string(), theme::fg(pal().fg_soft)).patch(bg),
            Span::styled("[A]".to_string(), theme::bold(pal().green)).patch(bg),
            Span::styled(" always   ".to_string(), theme::fg(pal().fg_soft)).patch(bg),
            Span::styled("[r]".to_string(), theme::bold(pal().red)).patch(bg),
            Span::styled(" reject".to_string(), theme::fg(pal().fg_soft)).patch(bg),
        ]));
        while rows.len() < h {
            rows.push(Line::from(Span::styled(" ".to_string(), bg)));
        }
        rows.truncate(h);
        return rows;
    }

    let Some(pq) = &sess.pending_question else {
        return rows;
    };
    let (header, question, multiple, custom, options) = match pq.current() {
        Some(q) => (
            q.header.clone(),
            q.question.clone(),
            q.multiple,
            q.custom,
            q.options.clone(),
        ),
        None => (String::new(), String::new(), false, false, Vec::new()),
    };
    let sel = pq.selected.get(pq.qi).copied().unwrap_or(0);

    let mut head = vec![
        Span::styled("? ".to_string(), theme::bold(pal().purple)).patch(bg),
        Span::styled(
            if header.trim().is_empty() {
                "agent asks".to_string()
            } else {
                header
            },
            theme::bold(pal().fg),
        )
        .patch(bg),
    ];
    if pq.questions.len() > 1 {
        head.push(
            Span::styled(
                format!("   ({}/{})", pq.qi + 1, pq.questions.len()),
                theme::dim(),
            )
            .patch(bg),
        );
    }
    rows.push(Line::from(head));

    for chunk in conversation::wrap_spans(
        &[Span::styled(question, theme::fg(pal().fg_soft))],
        text_w,
    ) {
        let mut sp = vec![Span::styled("  ".to_string(), bg)];
        sp.extend(chunk.into_iter().map(|s| s.patch(bg)));
        rows.push(Line::from(sp));
    }

    let max_rows = h.saturating_sub(1);
    for (i, o) in options.iter().enumerate() {
        if rows.len() >= max_rows {
            break;
        }
        let is_sel = i == sel;
        let checked = pq
            .chosen
            .get(pq.qi)
            .and_then(|v| v.get(i))
            .copied()
            .unwrap_or(false);
        let mark = if multiple {
            if checked { "[x] " } else { "[ ] " }
        } else if is_sel {
            "(•) "
        } else {
            "( ) "
        };
        let mut sp = vec![
            Span::styled("  ".to_string(), bg),
            Span::styled(mark.to_string(), theme::fg(pal().cyan)).patch(bg),
            Span::styled(
                conversation::truncate(&o.label, w.saturating_sub(8)),
                if is_sel { theme::bold(pal().fg) } else { theme::fg(pal().fg) },
            )
            .patch(bg),
        ];
        if !o.description.trim().is_empty() {
            sp.push(
                Span::styled(
                    format!(
                        "  {}",
                        conversation::truncate(&o.description, w.saturating_sub(30).max(8))
                    ),
                    theme::dim(),
                )
                .patch(bg),
            );
        }
        if is_sel {
            for s in &mut sp {
                s.style = s.style.bg(pal().selection);
            }
        }
        rows.push(Line::from(sp));
    }

    if custom && rows.len() < max_rows {
        rows.push(Line::from(vec![
            Span::styled("  custom: ".to_string(), theme::dim()).patch(bg),
            Span::styled(
                conversation::truncate(&pq.custom, w.saturating_sub(12)),
                theme::fg(pal().yellow),
            )
            .patch(bg),
            Span::styled("▏".to_string(), theme::fg(pal().cyan)).patch(bg),
        ]));
    }

    if rows.len() < h {
        let hint = if custom {
            "type an answer · Enter confirm · Esc reject"
        } else if multiple {
            "↑/↓ move · Space toggle · Enter confirm · Esc reject"
        } else {
            "↑/↓ move · Enter confirm · Esc reject"
        };
        rows.push(Line::from(Span::styled(format!("  {hint}"), theme::dim()).patch(bg)));
    }

    while rows.len() < h {
        rows.push(Line::from(Span::styled(" ".to_string(), bg)));
    }
    rows.truncate(h);
    rows
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_chatbox_text_uses_the_theme_foreground() {
        let bg = Style::default().bg(pal().bg_float);
        let row = vec![Span::raw("hello")];
        let spans = input_row_spans(
            format!("{} ", crate::theme::P_SYMBOL),
            theme::bold(pal().cyan),
            &row,
            bg,
            theme::fg(pal().fg).patch(bg),
        );
        let text = spans.last().expect("text span");
        assert_eq!(
            text.style.fg,
            Some(pal().fg),
            "typed text must use the theme foreground, not the terminal default"
        );
        assert_eq!(text.style.bg, Some(pal().bg_float), "on the chatbox background");
        assert_eq!(text.content, "hello");
    }

    #[test]
    fn prompt_glyph_keeps_its_accent_color() {
        let bg = Style::default().bg(pal().bg_float);
        let spans = input_row_spans(
            format!("{} ", crate::theme::P_SYMBOL),
            theme::bold(pal().cyan),
            &[Span::raw("x")],
            bg,
            theme::fg(pal().fg).patch(bg),
        );
        assert_eq!(spans[0].style.fg, Some(pal().cyan), "{:?}", spans[0].style);
        assert_eq!(spans[0].style.bg, Some(pal().bg_float));
    }
}
