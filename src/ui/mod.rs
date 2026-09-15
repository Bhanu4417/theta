//! Root render: header, body (explorer + panes), status bar, overlays.

pub mod conversation;
pub mod explorer;
pub mod overlays;
pub mod pane;
pub mod splash;
pub mod statusbar;

use crate::app::{App, Overlay};
use crate::theme::{pal, self};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::Block;

pub fn render(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(pal().bg)), area);

    let chunks =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    statusbar::render(f, app, chunks[1]);

    let mut body = chunks[0];
    app.too_small = false;
    app.last_body_area = body;

    if app.explorer.open {
        let w = app.cfg.ui.explorer_width.min(body.width.saturating_sub(20));
        if w > 6 {
            let cols =
                Layout::horizontal([Constraint::Length(w), Constraint::Min(1)]).split(body);
            explorer::render(f, app, cols[0]);
            body = cols[1];
        }
    }
    app.last_body_area = body;

    if app.sessions.is_empty() {
        splash::render(f, body, app);
    } else if let Some(max) = app.maximized {
        pane::render(f, app, body, max, true);
    } else {
        match app.grid.rects(body) {
            Some(rects) if !rects.is_empty() => {
                for (sid, r) in rects {
                    let focused = sid == app.focus;
                    pane::render(f, app, r, sid, focused);
                }
            }
            _ => {
                // Terminal too small for the full grid: keep the focused
                // session usable.
                app.too_small = true;
                pane::render(f, app, body, app.focus, true);
            }
        }
    }

    // Full-screen surfaces.
    if let Some(v) = &mut app.viewer {
        overlays::render_viewer(f, area, v);
    }
    if app.diff.is_some() {
        if let Some(d) = &app.diff {
            overlays::render_diff(f, area, d);
        }
    }

    // Modal overlays.
    match app.overlay {
        Overlay::Palette => overlays::render_palette(f, app, area),
        Overlay::NewSession => overlays::render_new_session(f, app, area),
        Overlay::Rename => overlays::render_rename(f, app, area),
        Overlay::ConfirmQuit => overlays::render_confirm_quit(f, app, area),
        Overlay::BusyChoice => {
            // Anchor the mini-dialog just above the focused pane's input box.
            let anchor = app.focused().and_then(|sess| {
                let rects = app.layout_rects(app.last_body_area)?;
                let (_, pr) = rects.iter().find(|(sid, _)| *sid == sess.id)?;
                let inner = Rect {
                    x: pr.x + 1,
                    y: pr.y + 1,
                    width: pr.width.saturating_sub(2),
                    height: pr.height.saturating_sub(2),
                };
                Some(Rect {
                    x: inner.x,
                    y: inner.y + inner.height.saturating_sub(1),
                    width: inner.width,
                    height: 1,
                })
            });
            overlays::render_busy_choice(f, app, area, anchor);
        }
        Overlay::Question => {
            // The question is rendered inline in the session's chatbox
            // (`ui::pane`); the overlay only routes keys to it.
        }
        Overlay::Keymap => overlays::render_keymap(f, app, area),
        Overlay::FileSearch => overlays::render_file_search(f, app, area),
        Overlay::ProjectSearch => overlays::render_project_search(f, app, area),
        Overlay::ConvSearch => overlays::render_conv_search(f, app, area),
        Overlay::Theme => {
            let anchor = app.focused().and_then(|sess| {
                let rects = app.layout_rects(app.last_body_area)?;
                let (_, pr) = rects.iter().find(|(sid, _)| *sid == sess.id)?;
                let inner = Rect {
                    x: pr.x + 1,
                    y: pr.y + 1,
                    width: pr.width.saturating_sub(2),
                    height: pr.height.saturating_sub(2),
                };
                let ih = pane::input_height(sess, inner.width as usize, inner.height as usize);
                Some(Rect {
                    x: inner.x + 1,
                    y: inner.y + inner.height.saturating_sub(ih),
                    width: inner.width.saturating_sub(2),
                    height: ih,
                })
            });
            overlays::render_theme_picker(f, app, area, anchor);
        }
        Overlay::AgyModel => {
            let anchor = app.focused().and_then(|sess| {
                let rects = app.layout_rects(app.last_body_area)?;
                let (_, pr) = rects.iter().find(|(sid, _)| *sid == sess.id)?;
                let inner = Rect {
                    x: pr.x + 1,
                    y: pr.y + 1,
                    width: pr.width.saturating_sub(2),
                    height: pr.height.saturating_sub(2),
                };
                let ih = pane::input_height(sess, inner.width as usize, inner.height as usize);
                Some(Rect {
                    x: inner.x + 1,
                    y: inner.y + inner.height.saturating_sub(ih),
                    width: inner.width.saturating_sub(2),
                    height: ih,
                })
            });
            overlays::render_agy_model_picker(f, app, area, anchor);
        }
        Overlay::ModelPicker | Overlay::AgentPicker => {
            // Anchor the picker to the focused pane's chat box.
            let anchor = app.focused().and_then(|sess| {
                let rects = app.layout_rects(app.last_body_area)?;
                let (_, pr) = rects.iter().find(|(sid, _)| *sid == sess.id)?;
                let inner = Rect {
                    x: pr.x + 1,
                    y: pr.y + 1,
                    width: pr.width.saturating_sub(2),
                    height: pr.height.saturating_sub(2),
                };
                let ih = pane::input_height(sess, inner.width as usize, inner.height as usize);
                Some(Rect {
                    x: inner.x + 1,
                    y: inner.y + inner.height.saturating_sub(ih),
                    width: inner.width.saturating_sub(2),
                    height: ih,
                })
            });
            match app.overlay {
                Overlay::ModelPicker => overlays::render_model_picker(f, app, area, anchor),
                _ => overlays::render_agent_picker(f, app, area, anchor),
            }
        }
        Overlay::SessionList => overlays::render_session_list(f, app, area),
        Overlay::ResumeSession => overlays::render_resume_picker(f, app, area),
        Overlay::LayoutPicker => overlays::render_layout_picker(f, app, area),
        Overlay::Tree => overlays::render_tree(f, app, area),
        Overlay::None => {}
    }
}
