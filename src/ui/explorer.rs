use crate::app::App;
use crate::theme::{pal, self};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

pub fn render(f: &mut ratatui::Frame, app: &App, area: Rect) {
    if area.width < 10 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(pal().border))
        .style(Style::default().bg(pal().bg))
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(crate::theme::P_SYMBOL.to_string(), theme::fg(pal().blue)),
            Span::styled(" PROJECT", theme::bold(pal().fg_soft)),
            Span::styled(" ", Style::default()),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height == 0 {
        return;
    }

    let w = inner.width.saturating_sub(1) as usize;
    let h = inner.height as usize;
    let items = &app.explorer.items;
    let sel = app.explorer.selected.min(items.len().saturating_sub(1));
    let start = if h >= items.len() {
        0
    } else {
        sel.saturating_sub(h / 2).min(items.len() - h)
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, item) in items.iter().enumerate().skip(start) {
        if lines.len() >= h {
            break;
        }
        let indent = "  ".repeat(item.depth.min(8));
        let (glyph, style) = if item.is_dir {
            ("▸ ", theme::fg(pal().blue))
        } else {
            ("  ", theme::fg(pal().fg_soft))
        };
        let name = crate::ui::conversation::truncate(&item.name, w.saturating_sub(indent.len() + 2));
        let selected = i == sel;
        let mut spans = vec![Span::styled(
            format!("{indent}{glyph}"),
            if selected { style.bg(pal().selection) } else { style },
        )];
        spans.push(Span::styled(
            name,
            if selected {
                Style::default().fg(pal().fg).bg(pal().selection)
            } else {
                style
            },
        ));
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(empty)", theme::dim())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
