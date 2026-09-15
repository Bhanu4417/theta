//! Global status bar: sessions, activity, cost, git, context.

use crate::app::App;
use crate::theme::{pal, self};
use ratatui::style::{Color, Style};
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
    let active = app.active_count();
    // A `/push` (or similar) status takes over the activity strip.
    let activity = app.sessions.iter().find_map(|s| s.activity.as_ref());

    let mut left: Vec<Span<'static>> = vec![Span::styled(" ".to_string(), bg)];
    if n > 0 {
        left.push(Span::styled(crate::theme::P_SYMBOL.to_string(), theme::bold(pal().blue)));
        left.push(Span::styled(
            format!(" {n} session{}", if n == 1 { "" } else { "s" }),
            Style::default().bg(barbg).fg(pal().fg_soft),
        ));
        if let Some(act) = activity {
            sep(&mut left);
            left.push(Span::styled(
                act.text.clone(),
                if act.done {
                    Style::default().bg(barbg).fg(pal().green)
                } else {
                    Style::default().bg(barbg).fg(pal().fg_soft)
                },
            ));
            if !act.done {
                left.push(Span::styled(" ", bg));
                left.extend(push_anim(
                    act.started.elapsed().as_millis() as usize,
                    barbg,
                ));
            }
        } else {
            if working > 0 {
                sep(&mut left);
                left.push(Span::styled(
                    format!("{working} working"),
                    Style::default().bg(barbg).fg(pal().cyan),
                ));
            }
            // Slider animates as long as anything is going on in any workspace.
            if active > 0 {
                left.push(Span::styled(" ", bg));
                left.extend(working_scanner(
                    app.started.elapsed().as_millis() as usize,
                    pal().cyan,
                    barbg,
                ));
            }
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
    // Build stamp: after `/refresh` this time changes, confirming the newest
    // binary took over.
    let build_hash = option_env!("THETA_BUILD_HASH").unwrap_or("dev");
    let build_time = option_env!("THETA_BUILD_TIME").unwrap_or("--:--:--");
    right.push(Span::styled("  ", bg));
    right.push(Span::styled(
        format!("v{}", env!("CARGO_PKG_VERSION")),
        theme::mute(),
    ));
    right.push(Span::styled(
        format!(" {build_hash} {build_time}"),
        Style::default().bg(barbg).fg(pal().fg_mute),
    ));
    right.push(Span::styled(" ", bg));

    let left_len: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_len: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(left_len + right_len);

    let mut spans = left;
    spans.push(Span::styled(" ".repeat(pad), bg));
    spans.extend(right);

    f.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}

/// Port of OpenCode's Knight-Rider working animation (`ui/spinner.ts`,
/// `style: "blocks"`): an 8-cell scanner of `■`/`⬝` sweeping back and forth
/// with a fading trail, advanced one 40ms frame at a time.
const SCAN_WIDTH: usize = 8;
const SCAN_HOLD_END: usize = 9;
const SCAN_HOLD_START: usize = 30;
const SCAN_TRAIL: usize = 6;
const SCAN_MIN_ALPHA: f32 = 0.3;
const SCAN_INACTIVE: f32 = 0.6;
const SCAN_INTERVAL_MS: usize = 40;

struct ScanState {
    active: usize,
    holding: bool,
    hold_progress: usize,
    hold_total: usize,
    movement_progress: usize,
    movement_total: usize,
    forward: bool,
}

fn scan_state(frame: usize) -> ScanState {
    let forward = SCAN_WIDTH;
    let back = SCAN_WIDTH - 1;
    if frame < forward {
        ScanState {
            active: frame,
            holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame,
            movement_total: forward,
            forward: true,
        }
    } else if frame < forward + SCAN_HOLD_END {
        ScanState {
            active: SCAN_WIDTH - 1,
            holding: true,
            hold_progress: frame - forward,
            hold_total: SCAN_HOLD_END,
            movement_progress: 0,
            movement_total: 0,
            forward: true,
        }
    } else if frame < forward + SCAN_HOLD_END + back {
        let bi = frame - forward - SCAN_HOLD_END;
        ScanState {
            active: SCAN_WIDTH - 2 - bi,
            holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: bi,
            movement_total: back,
            forward: false,
        }
    } else {
        ScanState {
            active: 0,
            holding: true,
            hold_progress: frame - forward - SCAN_HOLD_END - back,
            hold_total: SCAN_HOLD_START,
            movement_progress: 0,
            movement_total: 0,
            forward: false,
        }
    }
}

fn trail_alpha(i: usize) -> f32 {
    if i == 0 {
        1.0
    } else if i == 1 {
        0.9
    } else {
        0.65f32.powi((i - 1) as i32)
    }
}

fn brighten(color: ratatui::style::Color, factor: f32) -> ratatui::style::Color {
    match color {
        ratatui::style::Color::Rgb(r, g, b) => ratatui::style::Color::Rgb(
            (r as f32 * factor).min(255.0).round() as u8,
            (g as f32 * factor).min(255.0).round() as u8,
            (b as f32 * factor).min(255.0).round() as u8,
        ),
        other => other,
    }
}

fn working_scanner(
    elapsed_ms: usize,
    accent: ratatui::style::Color,
    barbg: ratatui::style::Color,
) -> Vec<Span<'static>> {
    let total_frames = SCAN_WIDTH + SCAN_HOLD_END + (SCAN_WIDTH - 1) + SCAN_HOLD_START;
    let frame = (elapsed_ms / SCAN_INTERVAL_MS) % total_frames;
    let st = scan_state(frame);

    let fade = if st.holding && st.hold_total > 0 {
        let p = (st.hold_progress as f32 / st.hold_total as f32).min(1.0);
        SCAN_MIN_ALPHA.max(1.0 - p * (1.0 - SCAN_MIN_ALPHA))
    } else if !st.holding && st.movement_total > 0 {
        let denom = st.movement_total.saturating_sub(1).max(1) as f32;
        let p = (st.movement_progress as f32 / denom).min(1.0);
        SCAN_MIN_ALPHA + p * (1.0 - SCAN_MIN_ALPHA)
    } else {
        1.0
    };

    let mut out = Vec::with_capacity(SCAN_WIDTH);
    for ci in 0..SCAN_WIDTH {
        let dd: i32 = if st.forward {
            st.active as i32 - ci as i32
        } else {
            ci as i32 - st.active as i32
        };
        let index: i32 = if st.holding {
            dd + st.hold_progress as i32
        } else if dd > 0 && (dd as usize) < SCAN_TRAIL {
            dd
        } else if dd == 0 {
            0
        } else {
            -1
        };
        let (ch, alpha) = if index >= 0 && (index as usize) < SCAN_TRAIL {
            ("■", trail_alpha(index as usize))
        } else {
            ("⬝", SCAN_INACTIVE * fade)
        };
        let base = if index == 1 { brighten(accent, 1.15) } else { accent };
        let color = crate::theme::blend(base, barbg, alpha.clamp(0.0, 1.0));
        out.push(Span::styled(
            ch.to_string(),
            Style::default().fg(color).bg(barbg),
        ));
    }
    out
}

/// Mono black-and-white push bar: an 8-cell full-block track that fills
/// left-to-right, easing toward ~95% like the shell download bar. Full blocks
/// keep it aligned with the neighbouring text.
fn push_anim(elapsed_ms: usize, barbg: ratatui::style::Color) -> Vec<Span<'static>> {
    let n = 8usize;
    let pct = (1.0 - (-(elapsed_ms as f32) / 700.0).exp()) * 95.0;
    let filled = ((pct / 100.0) * n as f32).round() as usize;
    let mut out: Vec<Span<'static>> = Vec::with_capacity(n);
    for i in 0..n {
        let fg = if i < filled { Color::White } else { Color::Gray };
        out.push(Span::styled(
            "█".to_string(),
            Style::default().fg(fg).bg(barbg),
        ));
    }
    out
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
