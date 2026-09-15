//! Conversation rendering: transcript → cached lines with blocks.
//!
//! Blocks map transcript entities (messages, tool calls, errors) to line
//! ranges so scrolling/search can jump precisely.

use crate::harness::transcript::{PartKind, Role, ToolInfo, ToolStatus};
use crate::session::{SessionState, ToolRef};
use crate::theme::{pal, self};

use ratatui::style::Color;

fn user_bg() -> Color {
    pal().user_bg
}
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    User,
    Assistant,
    Tool {
        tool_ref: ToolRef,
        part_id: String,
        status: ToolStatus,
        expanded: bool,
        has_diff: bool,
    },
    Thinking,
    Error,
}

#[derive(Debug, Clone)]
pub struct BlockSpan {
    pub kind: BlockKind,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl BlockSpan {
    pub fn label(&self) -> String {
        match &self.kind {
            BlockKind::User => "you".into(),
            BlockKind::Assistant => "ai".into(),
            BlockKind::Tool { .. } => "tool".into(),
            BlockKind::Thinking => "thinking".into(),
            BlockKind::Error => "error".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cache {
    pub width: u16,
    pub lines: Vec<Line<'static>>,
    pub blocks: Vec<BlockSpan>,
    /// True when a spinner is visible and the cache must be rebuilt per tick.
    pub animating: bool,
    pub built_at_tick: u64,
}

pub fn build_cache(sess: &SessionState, width: u16, tick: u64) -> Cache {
    let w = width.max(12) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut blocks: Vec<BlockSpan> = Vec::new();
    let mut animating = false;

    let start_block = |kind: BlockKind, text: String, lines: &mut Vec<Line<'static>>, blocks: &mut Vec<BlockSpan>| {
        let start = lines.len();
        blocks.push(BlockSpan {
            kind,
            start,
            end: start,
            text,
        });
    };

    let finish_block = |lines: &mut Vec<Line<'static>>, blocks: &mut Vec<BlockSpan>| {
        if let Some(last) = blocks.last_mut() {
            last.end = lines.len();
        }
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let busy = sess.status.is_busy();
    if busy {
        animating = true;
    }
    let last_user = sess.messages.iter().rposition(|m| m.role == Role::User);
    // The message currently being generated — the only one allowed to show a
    // live "thinking" timer. Historical turns replayed without an `end` must
    // not animate (they used to show an ever-growing bogus timer).
    let live_msg = if busy {
        sess.messages
            .iter()
            .rposition(|m| m.role == Role::Assistant && m.completed.is_none())
    } else {
        None
    };

    for (mi, msg) in sess.messages.iter().enumerate() {
        let mut rendered_any = false;
        let mut block_kind = match msg.role {
            Role::User => BlockKind::User,
            Role::Assistant => BlockKind::Assistant,
        };
        let mut block_text = String::new();

        for (pi, part) in msg.parts.iter().enumerate() {
            match &part.kind {
                PartKind::Text { text, synthetic } if !synthetic && !text.trim().is_empty() => {
                    if !rendered_any {
                        start_block(block_kind.clone(), String::new(), &mut lines, &mut blocks);
                        rendered_any = true;
                    }
                    match msg.role {
                        Role::User => {
                            // User prompts: Θ marker on a lifted background,
                            // with vertical padding so they breathe.
                            lines.push(pad_bg_line(Line::from(""), w, user_bg(), 0));
                            // While the agent works on this (latest) prompt,
                            // the marker becomes the animated spinner + time.
                            let in_flight = busy && Some(mi) == last_user;
                            let mut spans = if in_flight {
                                vec![
                                    Span::styled(
                                        format!("{} ", theme::spin(tick)),
                                        Style::default().fg(theme::spin_rgb(tick)),
                                    ),
                                    Span::styled(text.clone(), theme::fg(pal().fg)),
                                ]
                            } else {
                                vec![
                                    Span::styled("Θ ".to_string(), theme::bold(pal().cyan)),
                                    Span::styled(text.clone(), theme::fg(pal().fg)),
                                ]
                            };
                            if in_flight {
                                if let Some(t0) = msg.created {
                                    let secs = (now_ms - t0).max(0) as f64 / 1000.0;
                                    spans.push(Span::styled(
                                        format!("  · {secs:.1}s"),
                                        theme::dim(),
                                    ));
                                }
                            }
                            for chunk in wrap_spans(&spans, w.saturating_sub(2)) {
                                lines.push(pad_bg_line(Line::from(chunk), w, user_bg(), 1));
                            }
                            lines.push(pad_bg_line(Line::from(""), w, user_bg(), 0));
                        }
                        Role::Assistant => {
                            // AI replies render plainly on the workspace bg.
                            let base = theme::fg(pal().fg);
                            for l in render_markdown(text, w.saturating_sub(1), base) {
                                let mut sp = vec![Span::raw(" ")];
                                sp.extend(l.spans);
                                lines.push(Line::from(sp));
                            }
                        }
                    }
                    block_text.push_str(text);
                    block_text.push('\n');
                }
                PartKind::Reasoning { text, running, start, end } => {
                    // Collapsed thinking line: animated only for the message
                    // currently being generated; historical reasoning replays
                    // without a live turn so it can't show a bogus timer.
                    if *running && live_msg == Some(mi) {
                        if !rendered_any {
                            start_block(BlockKind::Thinking, String::new(), &mut lines, &mut blocks);
                            rendered_any = true;
                            animating = true;
                        }
                        let elapsed = start
                            .map(|s| (now_ms - s).max(0) as f64 / 1000.0)
                            .unwrap_or(0.0);
                        let spin_style = Style::default().fg(theme::spin_rgb(tick));
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("{} ", theme::spin(tick)),
                                spin_style,
                            ),
                            Span::styled("Thinking…", spin_style),
                            Span::styled(
                                format!(" {elapsed:.1}s"),
                                theme::fg(pal().fg_dim),
                            ),
                        ]));
                        // Preview the latest reasoning lines, like the
                        // OpenCode TUI streams what it is thinking.
                        let recent: Vec<&str> =
                            text.lines().rev().take(3).collect::<Vec<_>>();
                        for rl in recent.into_iter().rev() {
                            let trimmed: String =
                                rl.trim().chars().take(w.saturating_sub(8)).collect();
                            if trimmed.is_empty() {
                                continue;
                            }
                            lines.push(Line::from(Span::styled(
                                format!("    {trimmed}"),
                                theme::italic_dim(),
                            )));
                        }
                        block_text.push_str(text);
                    } else if let Some(e) = end {
                        let start_ms = start.unwrap_or(*e);
                        let secs = (*e - start_ms).max(0) as f64 / 1000.0;
                        if secs >= 1.0 {
                            if !rendered_any {
                                start_block(BlockKind::Thinking, String::new(), &mut lines, &mut blocks);
                                rendered_any = true;
                            }
                            lines.push(Line::from(vec![
                                Span::styled("✓ ", theme::fg(pal().fg_mute)),
                                Span::styled(
                                    format!("thought for {secs:.1}s"),
                                    theme::fg(pal().fg_mute),
                                ),
                            ]));
                        }
                    }
                }
                PartKind::Tool(t) => {
                    let expanded = sess.is_expanded(&part.id);
                    let selected = sess.tool_cursor == Some(ToolRef { msg: mi, part: pi });
                    if matches!(t.status, ToolStatus::Pending | ToolStatus::Running) {
                        animating = true;
                    }
                    finish_block(&mut lines, &mut blocks);
                    let has_diff = t.diff().is_some();
                    start_block(
                        BlockKind::Tool {
                            tool_ref: ToolRef { msg: mi, part: pi },
                            part_id: part.id.clone(),
                            status: t.status,
                            expanded,
                            has_diff,
                        },
                        format!("{} {}", t.tool, t.display_title()),
                        &mut lines,
                        &mut blocks,
                    );


                    let (glyph, glyph_style, title_style) = match t.status {
                        ToolStatus::Pending => ("◦", theme::fg(pal().fg_dim), theme::fg(pal().fg_dim)),
                        ToolStatus::Running => (theme::spin(tick), theme::fg(pal().cyan), theme::fg(pal().fg_soft)),
                        ToolStatus::Completed => ("✓", theme::fg(pal().green), theme::fg(pal().fg_soft)),
                        ToolStatus::Error => ("✗", theme::fg(pal().red), theme::fg(pal().fg_soft)),
                    };
                    let title = t.display_title();
                    let max_title = w.saturating_sub(4);
                    let title = truncate(&title, max_title);
                    let mut spans = vec![
                        Span::styled(" ", Style::default()),
                        Span::styled(glyph.to_string(), glyph_style),
                        Span::styled(" ", Style::default()),
                        Span::styled(title, title_style),
                    ];
                    if selected {
                        for s in &mut spans {
                            s.style = s.style.bg(pal().selection);
                        }
                    }
                    lines.push(Line::from(spans));

                    // Long-running shell work (clones, installs, builds) gets a
                    // live "…ing" label and an animated download bar beneath it.
                    if matches!(t.status, ToolStatus::Pending | ToolStatus::Running)
                        && t.tool == "bash"
                    {
                        let label = activity_label(t);
                        let pct = t
                            .output
                            .as_deref()
                            .and_then(parse_percent)
                            .or_else(|| synth_progress(t, now_ms));
                        lines.push(Line::from(vec![
                            Span::styled("   ".to_string(), Style::default()),
                            Span::styled(label, theme::fg(pal().fg_soft)),
                            Span::styled("…".to_string(), theme::dim()),
                        ]));
                        lines.push(activity_bar_line(w, pct));
                    }

                    // Edit/write tools show their diff inline (like the
                    // OpenCode TUI); expanding reveals the full detail.
                    let show_detail = expanded || has_diff;
                    if show_detail {
                        let cap = if expanded { 80 } else { 24 };
                        for l in tool_detail_lines(t, w, cap) {
                            lines.push(l);
                        }
                    }
                    finish_block(&mut lines, &mut blocks);
                    // next text part opens a fresh block of the message kind.
                    block_kind = match msg.role {
                        Role::User => BlockKind::User,
                        Role::Assistant => BlockKind::Assistant,
                    };
                    rendered_any = false;
                }
                _ => {}
            }
        }

        if rendered_any {
            finish_block(&mut lines, &mut blocks);
            if let Some(b) = blocks.last_mut() {
                b.text = block_text;
            }
        }

        if let Some(err) = &msg.error {
            start_block(BlockKind::Error, err.clone(), &mut lines, &mut blocks);
            if err.contains("Aborted") {
                lines.push(Line::from(Span::styled(
                    " · interrupted",
                    theme::dim(),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" ✗ {err}"),
                    theme::fg(pal().red),
                )));
            }
            finish_block(&mut lines, &mut blocks);
        }

        // Blank line between messages.
        lines.push(Line::from(""));
        finish_block(&mut lines, &mut blocks);
    }

    if let Some(err) = &sess.last_error {
        if blocks.last().map(|b| b.kind != BlockKind::Error).unwrap_or(true) {
            start_block(BlockKind::Error, err.clone(), &mut lines, &mut blocks);
            if err.contains("Aborted") {
                lines.push(Line::from(Span::styled("· interrupted", theme::dim())));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("✗ {err}"),
                    theme::fg(pal().red),
                )));
            }
            finish_block(&mut lines, &mut blocks);
        }
    }

    Cache {
        width,
        lines,
        blocks,
        animating,
        built_at_tick: tick,
    }
}

fn tool_detail_lines(t: &ToolInfo, w: usize, cap: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let indent = "    ";
    let inner = w.saturating_sub(6).max(8);

    if let Some(diff) = t.diff() {
        let file = t.file_path().unwrap_or_default();
        // OpenCode-style: split when the pane is wide, unified otherwise.
        let mut rows = crate::highlight::diff_view(&diff, &file, w, w >= 110);
        let total = rows.len();
        if total > cap {
            rows.truncate(cap);
            rows.push(Line::from(Span::styled(
                format!("{indent}… {} more diff lines (Enter to expand)", total - cap),
                theme::dim(),
            )));
        }
        return rows;
    }

    if let Some(err) = &t.error {
        out.push(Line::from(Span::styled(
            format!("{indent}{}", truncate(err, inner)),
            Style::default().fg(pal().red).add_modifier(Modifier::DIM),
        )));
        return out;
    }

    if let Some(output) = &t.output {
        let body = unwrap_tool_output(output);
        let raw: Vec<&str> = body.lines().collect();
        let cap = cap.min(40);
        let skipped = raw.len().saturating_sub(cap);
        if skipped > 0 {
            out.push(Line::from(Span::styled(
                format!("{indent}… {skipped} earlier lines"),
                theme::dim(),
            )));
        }
        for line in raw.iter().skip(skipped) {
            let mut spans = vec![Span::styled(indent.to_string(), Style::default())];
            spans.extend(wrap_spans(
                &[Span::styled(line.to_string(), theme::dim())],
                inner,
            )
            .into_iter()
            .next()
            .unwrap_or_default());
            out.push(Line::from(spans));
        }
    }
    out
}

/// OpenCode's read tool returns an XML-ish envelope; show just the content.
/// Human label for a running shell command (best-effort).
fn activity_label(t: &ToolInfo) -> String {
    let cmd = t.input_str(&["command", "cmd"]).unwrap_or_default();
    let c = cmd.to_lowercase();
    if c.contains("git clone") {
        "Cloning repository".into()
    } else if c.contains("git pull") || c.contains("git fetch") {
        "Fetching repository".into()
    } else if c.contains("git push") {
        "Pushing".into()
    } else if c.contains("curl") || c.contains("wget") || c.contains("download") {
        "Downloading".into()
    } else if c.contains("install") || (c.contains("add") && c.contains("cargo")) {
        "Installing packages".into()
    } else if c.contains("build") || c.contains("make") || c.contains("cargo") {
        "Building".into()
    } else if c.contains("test") {
        "Running tests".into()
    } else {
        match cmd.split_whitespace().next() {
            Some(first) if !first.is_empty() => format!("Running {first}"),
            _ => "Working".into(),
        }
    }
}

/// Best-effort current percentage from a streamed command output (git clone
/// prints lines like `Receiving objects:  45% (123/456)`).
fn parse_percent(s: &str) -> Option<u8> {
    let mut last = None;
    for tok in s.split(['\r', '\n']) {
        if let Some(idx) = tok.rfind('%') {
            let b = tok.as_bytes();
            let mut j = idx;
            while j > 0 && b[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j < idx {
                if let Ok(n) = tok[j..idx].parse::<u8>() {
                    last = Some(n);
                }
            }
        }
    }
    last
}

/// Fallback when the command gives no live percentage (git suppresses its own
/// progress off a TTY): estimate from elapsed time, easing toward 95%.
fn synth_progress(t: &ToolInfo, now_ms: i64) -> Option<u8> {
    if !matches!(t.status, ToolStatus::Pending | ToolStatus::Running) {
        return None;
    }
    let start = t.start_ms?;
    let elapsed = (now_ms - start).max(0) as f64 / 1000.0;
    let p = (1.0 - (-elapsed / 8.0).exp()) * 95.0;
    Some(p.round().clamp(0.0, 95.0) as u8)
}

/// A determinate progress bar: a dim track with the completed portion filled
/// in the theme accent and the percentage shown at the end. Uses a half-height
/// block so it stays a thin strip.
fn activity_bar_line(w: usize, pct: Option<u8>) -> Line<'static> {
    let bar_w = w.saturating_sub(4).clamp(8, 44);
    let filled = pct.map(|p| (p as usize * bar_w + 50) / 100).unwrap_or(0);
    let mut spans: Vec<Span<'static>> = vec![Span::styled("   ".to_string(), Style::default())];
    for i in 0..bar_w {
        let color = if i < filled { pal().cyan } else { pal().border };
        spans.push(Span::styled("▄".to_string(), Style::default().fg(color)));
    }
    if let Some(p) = pct {
        spans.push(Span::styled(format!("  {p}%"), theme::fg(pal().fg_soft)));
    }
    Line::from(spans)
}

fn unwrap_tool_output(output: &str) -> &str {
    let trimmed = output.trim_start();
    if trimmed.starts_with('<') {
        if let Some(start) = trimmed.find("<content>") {
            let rest = &trimmed[start + "<content>".len()..];
            if let Some(end) = rest.find("</content>") {
                return rest[..end].trim_start_matches('\n');
            }
            return rest.trim_start_matches('\n');
        }
    }
    output
}

fn render_markdown(text: &str, w: usize, base: Style) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    let mut lang = String::new();
    let mut code = String::new();

    for line in text.lines() {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.trim_start().strip_prefix("```") {
            if in_code {
                out.extend(code_block_lines(&code, &lang, w));
                code.clear();
                lang.clear();
                in_code = false;
            } else {
                in_code = true;
                lang = rest.trim().to_string();
            }
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        if trimmed.trim().is_empty() {
            out.push(Line::from(""));
            continue;
        }
        let t = trimmed.trim_start();
        for prefix in ["### ", "## ", "# "] {
            if let Some(h) = t.strip_prefix(prefix) {
                out.push(Line::from(Span::styled(
                    h.to_string(),
                    theme::bold(pal().blue),
                )));
                continue;
            }
        }
        let _ = t;
        if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            let mut spans = vec![Span::styled("  • ".to_string(), theme::fg(pal().cyan))];
            spans.extend(inline_md(rest, base));
            out.extend(wrap_spans(&spans, w.saturating_sub(2)).into_iter().map(|spans| {
                let mut s = vec![Span::raw(" ")];
                s.extend(spans);
                Line::from(s)
            }));
            continue;
        }
        // numbered list
        if let Some((num, rest)) = split_numbered(t) {
            let mut spans = vec![Span::styled(format!("  {num} "), theme::fg(pal().cyan))];
            spans.extend(inline_md(rest, base));
            out.extend(wrap_spans(&spans, w.saturating_sub(2)).into_iter().map(|spans| {
                let mut s = vec![Span::raw(" ")];
                s.extend(spans);
                Line::from(s)
            }));
            continue;
        }
        let spans = inline_md(t, base);
        for chunk in wrap_spans(&spans, w.saturating_sub(1)) {
            let mut s = vec![Span::raw(" ")];
            s.extend(chunk);
            out.push(Line::from(s));
        }
    }
    if in_code {
        out.extend(code_block_lines(&code, &lang, w));
    }
    out
}

fn split_numbered(t: &str) -> Option<(String, &str)> {
    let dot = t.find(". ")?;
    let num: String = t[..dot].chars().collect();
    if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) && num.len() <= 3 {
        Some((num, &t[dot + 2..]))
    } else {
        None
    }
}

fn inline_md(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(pos) = chars[i + 1..].iter().position(|&x| x == '`') {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), base));
                }
                let code: String = chars[i + 1..i + 1 + pos].iter().collect();
                spans.push(Span::styled(code, theme::fg(pal().teal)));
                i += pos + 2;
                continue;
            }
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(pos) = find_seq(&chars[i + 2..], &['*', '*']) {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), base));
                }
                let b: String = chars[i + 2..i + 2 + pos].iter().collect();
                spans.push(Span::styled(b, base.add_modifier(Modifier::BOLD)));
                i += pos + 4;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, base));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn find_seq(chars: &[char], seq: &[char]) -> Option<usize> {
    if seq.is_empty() || chars.len() < seq.len() {
        return None;
    }
    (0..=chars.len() - seq.len()).find(|&i| chars[i..i + seq.len()] == *seq)
}

fn code_block_lines(code: &str, lang: &str, w: usize) -> Vec<Line<'static>> {
    let hl_guard = crate::highlight::get();
    let hl = hl_guard.as_ref().map(|(_, h)| h).expect("highlighter");
    let inner = w.saturating_sub(4).max(8);
    let cap = 300;
    let total = code.trim_end_matches('\n').lines().count().max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut count = 0usize;
    'outer: for spans in hl.highlight_code(code, lang).into_iter() {
        for chunk in wrap_spans(&spans, inner) {
            if count >= cap {
                out.push(Line::from(Span::styled(
                    format!("    … {} more lines", total.saturating_sub(cap)),
                    theme::dim(),
                )));
                break 'outer;
            }
            out.push(bg_line(chunk, w));
            count += 1;
        }
    }
    if out.is_empty() {
        out.push(bg_line(vec![], w));
    }
    out
}

/// Fill a rendered line out to `w` columns on a background, with left padding.
fn pad_bg_line(line: Line<'static>, w: usize, bg: ratatui::style::Color, pad: usize) -> Line<'static> {
    let bg_style = Style::default().bg(bg);
    let mut spans: Vec<Span<'static>> = vec![Span::styled(" ".repeat(pad), bg_style)];
    for s in line.spans {
        spans.push(Span::styled(s.content, s.style.patch(bg_style)));
    }
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if used < w {
        spans.push(Span::styled(" ".repeat(w - used), bg_style));
    }
    Line::from(spans)
}

fn bg_line(chunk: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let bg = Style::default().bg(pal().code_bg);
    let mut spans = vec![Span::styled("  ".to_string(), bg)];
    for s in chunk {
        spans.push(Span::styled(s.content, s.style.patch(bg)));
    }
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if used < w {
        spans.push(Span::styled(" ".repeat(w - used), bg));
    }
    Line::from(spans)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 1 {
        "…".into()
    } else {
        let keep = max - 1;
        let mut out: String = s.chars().take(keep).collect();
        out.push('…');
        out
    }
}

/// Word-wrap styled spans into rows of spans (char-based widths).
pub fn wrap_spans(spans: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(4);
    let mut chars: Vec<Ch> = Vec::new();
    for s in spans {
        for c in s.content.chars() {
            chars.push(Ch { c, st: s.style });
        }
    }
    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Ch> = Vec::new();
    let mut last_space: Option<usize> = None;

    for ch in chars {
        if ch.c == '\n' {
            out.push(regroup(&cur));
            cur.clear();
            last_space = None;
            continue;
        }
        cur.push(ch);
        if ch.c == ' ' {
            last_space = Some(cur.len() - 1);
        }
        if cur.len() == width {
            match last_space {
                Some(sp) if sp + 1 < cur.len() => {
                    let rest = cur.split_off(sp + 1);
                    while cur.last().map(|c| c.c == ' ').unwrap_or(false) {
                        cur.pop();
                    }
                    out.push(regroup(&cur));
                    cur = rest;
                }
                _ => {
                    out.push(regroup(&cur));
                    cur.clear();
                }
            }
            last_space = None;
        }
    }
    if !cur.is_empty() {
        out.push(regroup(&cur));
    }
    if out.is_empty() {
        out.push(vec![Span::raw(String::new())]);
    }
    out
}

#[derive(Clone, Copy)]
struct Ch {
    c: char,
    st: Style,
}

fn regroup(chars: &[Ch]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut st: Option<Style> = None;
    for ch in chars {
        match st {
            Some(s) if s == ch.st => buf.push(ch.c),
            Some(s) => {
                spans.push(Span::styled(std::mem::take(&mut buf), s));
                st = Some(ch.st);
                buf.push(ch.c);
            }
            None => {
                st = Some(ch.st);
                buf.push(ch.c);
            }
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, st.unwrap_or_default()));
    }
    spans
}

/// A text selection in cache-line coordinates. `c0` applies to row `r0`,
/// `c1` to row `r1`; rows in between are fully selected.
#[derive(Debug, Clone, Copy)]
pub struct SelRange {
    pub r0: usize,
    pub c0: usize,
    pub r1: usize,
    pub c1: usize,
}

/// Render the transcript into `area`, honoring scroll + stick-to-bottom.
pub fn render(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    sess: &SessionState,
    cache: &Cache,
    sel: Option<SelRange>,
) {
    if area.width < 4 || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let total = cache.lines.len();
    // Like the OpenCode TUI: never scroll past the end — the last line stays
    // anchored to the bottom of the viewport.
    let max_off = total.saturating_sub(h);
    let offset = if sess.stick_bottom {
        max_off
    } else {
        sess.scroll.min(max_off)
    };
    let end = (offset + h).min(total);
    let slice: Vec<Line<'static>> = match sel {
        Some(sel) => cache.lines[offset..end]
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let abs = offset + i;
                if abs < sel.r0 || abs > sel.r1 {
                    line.clone()
                } else {
                    let c0 = if abs == sel.r0 { sel.c0 } else { 0 };
                    let c1 = if abs == sel.r1 { sel.c1 } else { usize::MAX };
                    patch_range(line, c0, c1, pal().selection)
                }
            })
            .collect(),
        None => cache.lines[offset..end].to_vec(),
    };
    let para = Paragraph::new(ratatui::text::Text::from(slice));
    f.render_widget(para, area);
}

/// Apply a selection background to the `[c0, c1)` char range of a line,
/// splitting spans as needed.
fn patch_range(line: &Line<'static>, c0: usize, c1: usize, bg: Color) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut idx = 0usize;
    for span in &line.spans {
        let chars: Vec<char> = span.content.chars().collect();
        let len = chars.len();
        let s = idx;
        let e = idx + len;
        idx = e;
        if e <= c0 || s >= c1 || len == 0 {
            out.push(span.clone());
            continue;
        }
        let local0 = c0.saturating_sub(s).min(len);
        let local1 = (c1.saturating_sub(s)).min(len).max(local0);
        if local0 > 0 {
            out.push(Span::styled(chars[..local0].iter().collect::<String>(), span.style));
        }
        if local1 > local0 {
            out.push(Span::styled(
                chars[local0..local1].iter().collect::<String>(),
                span.style.bg(bg),
            ));
        }
        if local1 < len {
            out.push(Span::styled(chars[local1..].iter().collect::<String>(), span.style));
        }
    }
    Line::from(out)
}

#[cfg(test)]
mod transcript_boundary_tests {
    use super::*;
    use crate::harness::transcript::{Message, Part, PartKind, Role, ToolInfo, ToolStatus};
    use crate::session::SessionState;
    use serde_json::json;

    fn text_part(text: &str) -> Part {
        Part {
            id: "p-text".into(),
            message_id: "m1".into(),
            kind: PartKind::Text {
                text: text.into(),
                synthetic: false,
            },
        }
    }

    fn assistant_meta(part: &Part) -> Message {
        Message {
            id: part.message_id.clone(),
            role: Role::Assistant,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: vec![part.clone()],
        }
    }

    fn cache_text(sess: &SessionState) -> String {
        build_cache(sess, 60, 0)
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn streaming_assistant_text_renders_incrementally() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        let first = text_part("Let me inspect");
        s.upsert_part(&assistant_meta(&first), first);
        assert!(cache_text(&s).contains("Let me inspect"));
        // The same part id with more text simulates a streaming delta.
        let grown = text_part("Let me inspect the project structure");
        s.upsert_part(&assistant_meta(&grown), grown);
        let rendered = cache_text(&s);
        assert!(rendered.contains("the project structure"));
    }

    #[test]
    fn tool_states_render_from_neutral_model() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        let tool = ToolInfo {
            tool: "bash".into(),
            call_id: "c1".into(),
            status: ToolStatus::Running,
            title: Some("Running tests".into()),
            input: json!({"command": "cargo test"}),
            output: None,
            error: None,
            metadata: json!({}),
            start_ms: None,
        };
        let part = Part {
            id: "p-tool".into(),
            message_id: "m1".into(),
            kind: PartKind::Tool(tool),
        };
        s.upsert_part(&assistant_meta(&part), part);
        assert!(cache_text(&s).contains("Running tests"));
    }

    #[test]
    fn user_message_renders_from_neutral_model() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        let part = Part {
            id: "p-user".into(),
            message_id: "mu".into(),
            kind: PartKind::Text {
                text: "fix the login bug".into(),
                synthetic: false,
            },
        };
        let msg = Message {
            id: "mu".into(),
            role: Role::User,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: vec![part.clone()],
        };
        s.upsert_part(&msg, part);
        assert!(cache_text(&s).contains("fix the login bug"));
    }
}
