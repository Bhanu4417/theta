//! Empty state and startup splash — blocky Theta mark, single colour.

use crate::app::App;
use crate::session::SessStatus;
use crate::theme::{pal, self};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

fn icon_lines() -> Vec<Line<'static>> {
    (0..crate::theme::MARK_H)
        .map(crate::theme::theta_mark_line)
        .collect()
}

fn below_lines(kind: SplashKind, tick: u64) -> Vec<Line<'static>> {
    let mut v = vec![Line::from("")];
    v.push(Line::from(Span::styled(
        "Multi-session agentic coding workspace",
        theme::dim(),
    )));
    v.push(Line::from(""));
    if kind == SplashKind::Initializing {
        v.push(Line::from(vec![
            Span::styled(format!("{} ", theme::spin(tick)), theme::fg(pal().cyan)),
            Span::styled("Initializing…", theme::dim()),
        ]));
        return v;
    }
    for (key, desc) in [
        ("^N", "new session"),
        ("^R", "resume session"),
        ("^O", "switch session"),
        ("^K", "commands"),
    ] {
        v.push(Line::from(vec![
            Span::styled(key.to_string(), theme::fg(pal().cyan)),
            Span::styled(format!("  {desc}"), theme::dim()),
        ]));
    }
    v
}

#[derive(Clone, Copy, PartialEq)]
enum SplashKind {
    Empty,
    Initializing,
}

pub fn render(f: &mut ratatui::Frame, area: Rect, app: &App) {
    if area.width < 34 || area.height < 12 {
        return;
    }
    let icon_w = crate::theme::MARK_W as u16;
    let icon_h = crate::theme::MARK_H as u16;

    let all_connecting = !app.sessions.is_empty()
        && app
            .sessions
            .iter()
            .all(|s| s.status == SessStatus::Connecting);
    let kind = if all_connecting && app.restored {
        SplashKind::Initializing
    } else {
        SplashKind::Empty
    };

    let icon = icon_lines();
    let below = below_lines(kind, app.tick);
    let total_h = icon_h + below.len() as u16;

    // Terminal too small for the full mark: show just the text block.
    if total_h >= area.height {
        let th = below.len() as u16;
        let y = area.y + (area.height - th) / 2;
        let x = area.x + 4;
        let anim = below;
        f.render_widget(
            Paragraph::new(anim),
            Rect { x, y, width: area.width.saturating_sub(8), height: th },
        );
        return;
    }
    let y = area.y + (area.height - total_h) / 2;
    let x = area.x + 4; // left-aligned with a small margin

    for (i, line) in icon.iter().enumerate() {
        f.render_widget(
            Paragraph::new(line.clone()),
            Rect { x, y: y + i as u16, width: icon_w.min(area.width), height: 1 },
        );
    }
    let below_y = y + icon.len() as u16;
    let below_h = below.len() as u16;
    let w = area.right().saturating_sub(x);
    f.render_widget(
        Paragraph::new(below),
        Rect { x, y: below_y, width: w, height: below_h },
    );
}
