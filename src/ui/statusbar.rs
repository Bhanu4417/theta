//! Global status bar: sessions, activity, cost, git, context.

use crate::app::App;
use crate::theme::{pal, self};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn render(f: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    if area.width < 20 {
        return;
    }
    // No darker band when the workspace is empty — blend with the splash.
    let barbg = if app.sessions.is_empty() { pal().bg } else { pal().bg_dark };
    let bg = Style::default().bg(barbg);
    let dim = Style::default().bg(barbg).fg(pal().fg_dim);
    let sep = |spans: &mut Vec<Span<'static>>| {
        spans.push(Span::styled("  ·  ", dim));
    };

    let n = app.sessions.len();
    let working = app.working_count();
    let workspace_cost: f64 = app.sessions.iter().map(|s| s.cost).sum();

    let mut left: Vec<Span<'static>> = vec![Span::styled(" ".to_string(), bg)];
    if n > 0 {
        left.push(Span::styled(crate::theme::P_SYMBOL.to_string(), theme::bold(pal().blue)));
        left.push(Span::styled(
            format!(" {n} session{}", if n == 1 { "" } else { "s" }),
            Style::default().bg(barbg).fg(pal().fg_soft),
        ));
        if working > 0 {
            sep(&mut left);
            left.push(Span::styled(
                format!("{working} working"),
                Style::default().bg(barbg).fg(pal().cyan),
            ));
        }
        if workspace_cost > 0.0 {
            sep(&mut left);
            let cost = if workspace_cost >= 1.0 {
                format!("${workspace_cost:.2}")
            } else {
                format!("${workspace_cost:.4}")
            };
            left.push(Span::styled(
                cost,
                Style::default().bg(barbg).fg(pal().orange),
            ));
        }
    }

    if app.too_small {
        sep(&mut left);
        left.push(Span::styled(
            "pane too small — focused only",
            Style::default().bg(barbg).fg(pal().yellow),
        ));
    }

    if let Some((msg, _)) = &app.flash {
        sep(&mut left);
        left.push(Span::styled(
            msg.clone(),
            Style::default().bg(barbg).fg(pal().yellow),
        ));
    }

    // Right cluster: dir · git · ctx · cost · model · hint
    let mut right: Vec<Span<'static>> = Vec::new();
    if let Some(s) = app.focused() {
        let dir = theme::abbreviate_path(&s.dir.to_string_lossy());
        right.push(Span::styled(dir, dim));
        sep(&mut right);
        if let Some(git) = &app.git_display {
            if let Some(branch) = &git.branch {
                right.push(Span::styled(branch.clone(), dim));
                let summary = git.summary();
                if !summary.is_empty() {
                    right.push(Span::styled(
                        format!(" {summary}"),
                        Style::default().bg(barbg).fg(pal().orange),
                    ));
                }
                sep(&mut right);
            }
        }
        if s.ctx_tokens > 0 {
            let limit = app
                .focused()
                .and_then(|sess| {
                    let want = sess.model.as_ref().or(app.default_model.as_ref())?;
                    app.providers
                        .iter()
                        .find(|p| p.provider_id == want.provider_id && p.model_id == want.model_id)
                })
                .and_then(|p| p.context_limit);
            match limit {
                Some(lim) if lim > 0 => {
                    let pct = (s.ctx_tokens as f64 / lim as f64 * 100.0).min(999.0);
                    right.push(Span::styled(
                        format!("ctx {:.0}%", pct),
                        Style::default().bg(barbg).fg(pal().fg_soft),
                    ));
                }
                _ => {
                    right.push(Span::styled(
                        format!("ctx {}", fmt_tokens(s.ctx_tokens)),
                        Style::default().bg(barbg).fg(pal().fg_soft),
                    ));
                }
            }
        }
        if s.cost > 0.0 {
            right.push(Span::styled(
                format!(" ${:.4}", s.cost),
                Style::default().bg(barbg).fg(pal().orange),
            ));
        }
    }
    right.push(Span::styled("   ", bg));
    let hint = |action: crate::keys::Action| {
        Span::styled(format!("{} ", app.keys.binding_str(action)), theme::mute())
    };
    right.push(hint(crate::keys::Action::Palette));
    right.push(Span::styled("Commands", theme::mute()));
    right.push(Span::styled("  ", bg));
    right.push(hint(crate::keys::Action::Switch));
    right.push(Span::styled("Switch", theme::mute()));
    right.push(Span::styled("  ", bg));
    right.push(hint(crate::keys::Action::NewSession));
    right.push(Span::styled("New", theme::mute()));
    right.push(Span::styled("  ", bg));
    right.push(hint(crate::keys::Action::Tiling));
    right.push(Span::styled("Tiling", theme::mute()));
    right.push(Span::styled("  ", bg));
    right.push(hint(crate::keys::Action::Quit));
    right.push(Span::styled("Quit", theme::mute()));
    right.push(Span::styled(" ", bg));

    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(left_len + right_len);

    let mut spans = left;
    spans.push(Span::styled(" ".repeat(pad), bg));
    spans.extend(right);

    f.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}

pub fn fmt_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1_000.0)
    } else {
        format!("{t}")
    }
}
