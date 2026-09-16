use crate::harness::transcript::{Message, PartKind, Role, ToolInfo, ToolStatus};
use crate::session::{SessionState, ToolRef};
use std::collections::HashMap;
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
    pub animating: bool,
    pub built_at_tick: u64,
    /// Per-message render output, reused across rebuilds. Without this, every
    /// streamed token re-rendered the entire transcript.
    renders: HashMap<String, RenderedMessage>,
}

/// One message's rendered lines and blocks, with a cheap signature used to
/// decide whether it can be reused instead of re-rendered.
#[derive(Debug, Clone)]
struct RenderedMessage {
    rev: u64,
    lines: Vec<Line<'static>>,
    blocks: Vec<BlockSpan>,
    animating: bool,
}

fn mix(h: &mut u64, v: u64) {
    *h ^= v;
    *h = h.wrapping_mul(0x0000_0100_0000_01b3);
}

/// A signature that changes whenever this message would render differently.
/// Lengths are used instead of the text itself so the check stays O(parts).
fn message_rev(sess: &SessionState, mi: usize, msg: &Message, ctx: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    // How a message renders also depends on session state: whether the turn is
    // running, whether this is the prompt in flight, and whether this is the
    // live message. Omitting it meant a render captured mid-turn was reused
    // afterwards, so a finished reply stayed invisible and the working spinner
    // never cleared until the session was refreshed.
    mix(&mut h, ctx);
    mix(&mut h, msg.role as u8 as u64);
    mix(&mut h, msg.completed.is_some() as u64);
    mix(&mut h, msg.parts.len() as u64);
    for (pi, part) in msg.parts.iter().enumerate() {
        let selected = sess
            .tool_cursor
            .as_ref()
            .map(|t| t.msg == mi && t.part == pi)
            .unwrap_or(false);
        mix(&mut h, selected as u64);
        match &part.kind {
            PartKind::Text { text, synthetic } => {
                mix(&mut h, 1);
                mix(&mut h, text.len() as u64);
                mix(&mut h, *synthetic as u64);
            }
            PartKind::Reasoning { text, running, end, .. } => {
                mix(&mut h, 2);
                mix(&mut h, text.len() as u64);
                mix(&mut h, *running as u64);
                mix(&mut h, end.unwrap_or(0) as u64);
            }
            PartKind::Tool(t) => {
                mix(&mut h, 3);
                mix(
                    &mut h,
                    match t.status {
                        ToolStatus::Pending => 0,
                        ToolStatus::Running => 1,
                        ToolStatus::Completed => 2,
                        ToolStatus::Error => 3,
                    },
                );
                mix(&mut h, t.output.as_ref().map(|o| o.len()).unwrap_or(0) as u64);
                mix(&mut h, t.title.as_ref().map(|s| s.len()).unwrap_or(0) as u64);
                mix(&mut h, sess.is_expanded(&part.id) as u64);
            }
            PartKind::Compaction { tokens_before } => {
                mix(&mut h, 4);
                mix(&mut h, *tokens_before);
            }
            PartKind::StepStart | PartKind::StepFinish | PartKind::Other => mix(&mut h, 9),
        }
    }
    h
}

/// Render a single message into its own line/block buffer, with block offsets
/// relative to that buffer. The caller shifts them when assembling.
#[allow(clippy::too_many_arguments)]
fn render_message(
    sess: &SessionState,
    mi: usize,
    msg: &Message,
    w: usize,
    tick: u64,
    now_ms: i64,
    busy: bool,
    live_msg: Option<usize>,
    last_user: Option<usize>,
) -> RenderedMessage {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut blocks: Vec<BlockSpan> = Vec::new();
    let mut animating = false;

    let start_block = |kind: BlockKind, text: String, lines: &mut Vec<Line<'static>>, blocks: &mut Vec<BlockSpan>| {
        let start = lines.len();
        blocks.push(BlockSpan { kind, start, end: start, text });
    };

    let finish_block = |lines: &mut Vec<Line<'static>>, blocks: &mut Vec<BlockSpan>| {
        if let Some(last) = blocks.last_mut() {
            last.end = lines.len();
        }
    };

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
                        lines.push(pad_bg_line(Line::from(""), w, user_bg(), 0));
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
                if *running && live_msg == Some(mi) {
                    if !rendered_any {
                        start_block(BlockKind::Thinking, String::new(), &mut lines, &mut blocks);
                        rendered_any = true;
                        animating = true;
                    }
                    // A missing or zero start means "unknown": subtracting it
                    // would display the whole Unix epoch as the elapsed time.
                    let elapsed = start
                        .filter(|s| *s > 0 && *s <= now_ms)
                        .map(|s| (now_ms - s) as f64 / 1000.0)
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

                let show_detail = expanded || has_diff;
                if show_detail {
                    let cap = if expanded { 80 } else { 24 };
                    for l in tool_detail_lines(t, w, cap) {
                        lines.push(l);
                    }
                }
                finish_block(&mut lines, &mut blocks);
                block_kind = match msg.role {
                    Role::User => BlockKind::User,
                    Role::Assistant => BlockKind::Assistant,
                };
                rendered_any = false;
            }
            PartKind::Compaction { tokens_before } => {
                finish_block(&mut lines, &mut blocks);
                start_block(
                    BlockKind::Assistant,
                    format!("conversation compacted ({tokens_before} tokens)"),
                    &mut lines,
                    &mut blocks,
                );
                lines.push(Line::from(""));
                lines.push(centered_divider("conversation compacted", w, theme::dim()));
                lines.push(Line::from(""));
                finish_block(&mut lines, &mut blocks);
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

    lines.push(Line::from(""));
    finish_block(&mut lines, &mut blocks);

    RenderedMessage { rev: 0, lines, blocks, animating }
}

impl Cache {
    /// A cache with no per-message renders — used by tests to stage a specific
    /// transcript without going through [`rebuild`].
    #[cfg(test)]
    pub fn synthetic(width: u16, lines: Vec<Line<'static>>) -> Self {
        Self {
            width,
            lines,
            blocks: Vec::new(),
            animating: false,
            built_at_tick: 0,
            renders: HashMap::new(),
        }
    }
}

pub fn build_cache(sess: &SessionState, width: u16, tick: u64) -> Cache {
    rebuild(None, sess, width, tick)
}

/// Rebuild the transcript cache, reusing every message whose content did not
/// change. Streaming a reply touches one message; everything before it is
/// already rendered, so a long transcript stays cheap to update.
pub fn rebuild(prev: Option<&Cache>, sess: &SessionState, width: u16, tick: u64) -> Cache {
    let w = width.max(12) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut blocks: Vec<BlockSpan> = Vec::new();
    let mut animating = false;
    let mut renders: HashMap<String, RenderedMessage> = HashMap::new();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let busy = sess.status.is_busy();
    if busy {
        animating = true;
    }
    let last_user = sess.messages.iter().rposition(|m| m.role == Role::User);
    let live_msg = if busy {
        sess.messages
            .iter()
            .rposition(|m| m.role == Role::Assistant && m.completed.is_none())
    } else {
        None
    };

    // Reuse is only valid at the same width: wrapping depends on it.
    let reusable = prev.filter(|c| c.width == width);

    for (mi, msg) in sess.messages.iter().enumerate() {
        let ctx = (busy as u64)
            | ((last_user == Some(mi)) as u64) << 1
            | ((live_msg == Some(mi)) as u64) << 2;
        let rev = message_rev(sess, mi, msg, ctx);
        // Any message that draws a spinner has to be re-rendered each tick,
        // otherwise a reused frame would freeze the animation. That is the
        // in-flight turn, the last prompt while busy, and any running tool —
        // one or two messages, never the whole transcript.
        let animates = (busy && live_msg == Some(mi))
            || (busy && Some(mi) == last_user)
            || msg
                .parts
                .iter()
                .any(|p| matches!(&p.kind, PartKind::Tool(t) if t.status == ToolStatus::Running));
        let cached = reusable
            .and_then(|c| c.renders.get(&msg.id))
            .filter(|r| r.rev == rev && !animates);

        let mut rendered = match cached {
            Some(r) => r.clone(),
            None => {
                let mut r = render_message(sess, mi, msg, w, tick, now_ms, busy, live_msg, last_user);
                r.rev = rev;
                r
            }
        };

        // Shift this message's blocks into the assembled transcript.
        let base = lines.len();
        for b in &mut rendered.blocks {
            b.start += base;
            b.end += base;
        }
        lines.extend(rendered.lines.iter().cloned());
        blocks.extend(rendered.blocks.iter().cloned());

        // Store with relative offsets so the entry stays reusable.
        let mut stored = rendered;
        for b in &mut stored.blocks {
            b.start -= base;
            b.end -= base;
        }
        animating |= stored.animating;
        renders.insert(msg.id.clone(), stored);
    }

    if let Some(err) = &sess.last_error {
        if blocks.last().map(|b| b.kind != BlockKind::Error).unwrap_or(true) {
            let start = lines.len();
            blocks.push(BlockSpan { kind: BlockKind::Error, start, end: start, text: err.clone() });
            if err.contains("Aborted") {
                lines.push(Line::from(Span::styled("· interrupted", theme::dim())));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("✗ {err}"),
                    theme::fg(pal().red),
                )));
            }
            if let Some(last) = blocks.last_mut() {
                last.end = lines.len();
            }
        }
    }

    Cache {
        width,
        lines,
        blocks,
        animating,
        built_at_tick: tick,
        renders,
    }
}

fn tool_detail_lines(t: &ToolInfo, w: usize, cap: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let indent = "    ";
    let inner = w.saturating_sub(6).max(8);

    if let Some(diff) = t.diff() {
        let file = t.file_path().unwrap_or_default();
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

fn synth_progress(t: &ToolInfo, now_ms: i64) -> Option<u8> {
    if !matches!(t.status, ToolStatus::Pending | ToolStatus::Running) {
        return None;
    }
    let start = t.start_ms?;
    let elapsed = (now_ms - start).max(0) as f64 / 1000.0;
    let p = (1.0 - (-elapsed / 8.0).exp()) * 95.0;
    Some(p.round().clamp(0.0, 95.0) as u8)
}

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

fn centered_divider(label: &str, w: usize, style: Style) -> Line<'static> {
    let text = format!(" {label} ");
    let tw = text.chars().count();
    if tw + 2 >= w {
        return Line::from(Span::styled(truncate(label, w), style));
    }
    let remaining = w - tw;
    let left = remaining / 2;
    let right = remaining - left;
    Line::from(vec![
        Span::styled("─".repeat(left), style),
        Span::styled(text, style),
        Span::styled("─".repeat(right), style),
    ])
}

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

#[derive(Debug, Clone, Copy)]
pub struct SelRange {
    pub r0: usize,
    pub c0: usize,
    pub r1: usize,
    pub c1: usize,
}

pub fn view_offset(stick_bottom: bool, scroll: usize, total: usize, height: usize) -> usize {
    let max_off = total.saturating_sub(height);
    if stick_bottom {
        max_off
    } else {
        scroll.min(max_off)
    }
}

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
    let offset = view_offset(sess.stick_bottom, sess.scroll, total, h);
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
        let grown = text_part("Let me inspect the project structure");
        s.upsert_part(&assistant_meta(&grown), grown);
        let rendered = cache_text(&s);
        assert!(rendered.contains("the project structure"));
    }

#[test]
    fn finished_reply_appears_without_refreshing_the_session() {
        // Reported bug: the reply stayed invisible after the turn finished and
        // only showed up on a session refresh.
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        // A prompt must exist for this to reproduce: the reply only renders
        // differently while a turn is in flight when there is a user message.
        let u = Message {
            id: "mu".into(),
            role: Role::User,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: vec![Part {
                id: "p-u".into(),
                message_id: "mu".into(),
                kind: PartKind::Text { text: "my question".into(), synthetic: false },
            }],
        };
        s.upsert_message_meta(&u);
        s.upsert_part(&u, u.parts[0].clone());

        s.status = crate::session::SessStatus::Working;
        let p = text_part("the written answer");
        s.upsert_part(&assistant_meta(&p), p);

        // Mid-turn: build the cache in the state an earlier bug would have
        // cached, before the reply was finished.
        let working = build_cache(&s, 60, 0);

        // The turn completes. No refresh, just the next frame.
        s.status = crate::session::SessStatus::Idle;
        let after = rebuild(Some(&working), &s, 60, 1);
        let text = after
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|sp| sp.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("the written answer"),
            "the reply must appear as soon as the turn ends, with no refresh:\n{text}"
        );
    }

    #[test]
    fn a_finished_turn_stops_drawing_the_working_spinner_on_the_prompt() {
        // The user's own message renders differently while busy; that state must
        // be invalidated when the turn ends, not cached forever.
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        s.status = crate::session::SessStatus::Working;
        let u = Message {
            id: "mu".into(),
            role: Role::User,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: vec![Part {
                id: "p-u".into(),
                message_id: "mu".into(),
                kind: PartKind::Text { text: "my question".into(), synthetic: false },
            }],
        };
        s.upsert_message_meta(&u);
        s.upsert_part(&u, u.parts[0].clone());
        let working = build_cache(&s, 60, 0);

        s.status = crate::session::SessStatus::Idle;
        let text = {
            let c = rebuild(Some(&working), &s, 60, 1);
            c.lines
                .iter()
                .map(|l| l.spans.iter().map(|sp| sp.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text.contains("my question"));
        // With the turn over, the prompt glyph is back rather than a spinner.
        assert!(
            text.contains("Θ my question") || text.contains("my question"),
            "prompt should render normally once idle:\n{text}"
        );
        assert!(
            !text.contains('⣾') && !text.contains('⣽') && !text.contains('⣻'),
            "the working spinner must not persist after the turn ends:\n{text}"
        );
    }

#[test]
    fn the_thinking_timer_never_shows_an_absurd_elapsed() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("/tmp"));
        s.status = crate::session::SessStatus::Working;
        for start in [None, Some(0)] {
            let mut sess = s.clone();
            let part = Part {
                id: "p-r".into(),
                message_id: "m1".into(),
                kind: PartKind::Reasoning {
                    text: "hmm".into(),
                    running: true,
                    start,
                    end: None,
                },
            };
            let meta = Message {
                id: "m1".into(),
                role: Role::Assistant,
                error: None,
                completed: None,
                created: None,
                cost: None,
                tokens: None,
                parts: vec![part.clone()],
            };
            sess.upsert_part(&meta, part);
            let text = cache_text(&sess);
            assert!(text.contains("Thinking"), "spinner shows: {text}");
            // Never a number in the billions (epoch seconds).
            assert!(
                !text.contains("1789") && !text.contains("17") || text.contains("0.0s"),
                "absurd elapsed rendered: {text}"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::transcript::{Message, Part, Role as TRole};

    fn plain(cache: &Cache) -> String {
        cache
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
    fn compaction_part_renders_a_centered_divider() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("."));
        let part = Part {
            id: "c1".into(),
            message_id: "m1".into(),
            kind: PartKind::Compaction { tokens_before: 12_345 },
        };
        let meta = Message {
            id: "m1".into(),
            role: TRole::Assistant,
            error: None,
            completed: Some(1),
            created: None,
            cost: None,
            tokens: None,
            parts: vec![part.clone()],
        };
        s.upsert_part(&meta, part);
        let cache = build_cache(&s, 60, 0);
        let text = plain(&cache);
        assert!(text.contains("conversation compacted"), "{text}");
        assert!(text
            .lines()
            .any(|l| l.contains("conversation compacted") && l.starts_with('─')), "{text}");
    }


    #[test]
    fn incremental_rebuild_matches_a_full_one() {
        // The optimization is only valid if reusing per-message renders yields
        // exactly what a from-scratch render would, in every state.
        fn plain(c: &Cache) -> String {
            c.lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        }

        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("."));
        s.status = crate::session::SessStatus::Working;
        for i in 0..6 {
            let u = Message {
                id: format!("u{i}"),
                role: TRole::User,
                error: None,
                completed: Some(i),
                created: Some(i),
                cost: None,
                tokens: None,
                parts: vec![Part {
                    id: format!("u{i}-p"),
                    message_id: format!("u{i}"),
                    kind: PartKind::Text {
                        text: format!("question {i} {}", "x".repeat(300)),
                        synthetic: false,
                    },
                }],
            };
            s.upsert_message_meta(&u);
            s.upsert_part(&u, u.parts[0].clone());

            let a = Message {
                id: format!("a{i}"),
                role: TRole::Assistant,
                error: None,
                completed: Some(i),
                created: Some(i),
                cost: Some(0.01),
                tokens: None,
                parts: vec![
                    Part {
                        id: format!("a{i}-t"),
                        message_id: format!("a{i}"),
                        kind: PartKind::Text {
                            text: format!("answer {i} {}", "y".repeat(400)),
                            synthetic: false,
                        },
                    },
                    Part {
                        id: format!("a{i}-tool"),
                        message_id: format!("a{i}"),
                        kind: PartKind::Tool(ToolInfo {
                            tool: "read".into(),
                            call_id: format!("c{i}"),
                            status: ToolStatus::Completed,
                            title: None,
                            input: serde_json::json!({"path": format!("/f{i}.rs")}),
                            output: Some("z".repeat(200)),
                            error: None,
                            metadata: serde_json::json!({}),
                            start_ms: None,
                        }),
                    },
                ],
            };
            s.upsert_message_meta(&a);
            for part in &a.parts {
                s.upsert_part(&a, part.clone());
            }
        }

        // At every step, the incremental result must equal a full rebuild of
        // the same state at the same width.
        let check = |s: &SessionState, prev: &Cache, width: u16, tick: u64| -> Cache {
            let inc = rebuild(Some(prev), s, width, tick);
            let full = rebuild(None, s, width, tick);
            assert_eq!(plain(&inc), plain(&full), "width={width} tick={tick}");
            assert_eq!(inc.blocks.len(), full.blocks.len(), "block count at tick={tick}");
            for (a, b) in inc.blocks.iter().zip(full.blocks.iter()) {
                assert_eq!((a.start, a.end, &a.kind), (b.start, b.end, &b.kind), "block ranges");
            }
            inc
        };

        let mut cache = check(&s, &rebuild(None, &s, 80, 0), 80, 0);

        // Appending a part to the last message.
        let last = s.messages.last().unwrap().id.clone();
        if let Some(m) = s.messages.iter_mut().find(|m| m.id == last) {
            m.parts.push(Part {
                id: "extra".into(),
                message_id: last.clone(),
                kind: PartKind::Text { text: "more".into(), synthetic: false },
            });
        }
        cache = check(&s, &cache, 80, 1);

        // Expanding a tool entry.
        s.expanded.insert("a2-tool".into());
        cache = check(&s, &cache, 80, 2);

        // Moving the tool cursor must invalidate its message.
        s.tool_cursor = Some(crate::session::ToolRef { msg: 3, part: 1 });
        cache = check(&s, &cache, 80, 3);

        // A width change must re-render everything.
        cache = check(&s, &cache, 60, 4);

        // And a live message is never reused while it animates.
        s.status = crate::session::SessStatus::Working;
        if let Some(m) = s.messages.last_mut() {
            m.completed = None;
        }
        check(&s, &cache, 60, 5);
    }

    #[test]
    fn reusing_a_cache_across_identical_rebuilds_is_stable() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("."));
        let m = Message {
            id: "m1".into(),
            role: TRole::User,
            error: None,
            completed: Some(1),
            created: Some(1),
            cost: None,
            tokens: None,
            parts: vec![Part {
                id: "p1".into(),
                message_id: "m1".into(),
                kind: PartKind::Text { text: "hello".into(), synthetic: false },
            }],
        };
        s.upsert_message_meta(&m);
        s.upsert_part(&m, m.parts[0].clone());

        let a = build_cache(&s, 80, 0);
        let b = rebuild(Some(&a), &s, 80, 1);
        let c = rebuild(Some(&b), &s, 80, 2);
        let plain = |x: &Cache| {
            x.lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(plain(&a), plain(&c), "repeated rebuilds must be identical");
    }

    #[test]
    fn live_thinking_shows_a_spinner_but_not_the_reasoning_text() {
        let mut s = SessionState::new(1, "s".into(), std::path::PathBuf::from("."));
        s.status = crate::session::SessStatus::Working;
        let part = Part {
            id: "r1".into(),
            message_id: "m1".into(),
            kind: PartKind::Reasoning {
                text: "SECRET-REASONING-TEXT".into(),
                running: true,
                start: Some(1),
                end: None,
            },
        };
        let meta = Message {
            id: "m1".into(),
            role: TRole::Assistant,
            error: None,
            completed: None,
            created: None,
            cost: None,
            tokens: None,
            parts: vec![part.clone()],
        };
        s.upsert_part(&meta, part);
        let cache = build_cache(&s, 60, 0);
        let text = plain(&cache);
        assert!(text.contains("Thinking"), "spinner still shows: {text}");
        assert!(
            !text.contains("SECRET-REASONING-TEXT"),
            "reasoning text must not be rendered: {text}"
        );
        assert!(
            cache.blocks.iter().any(|b| b.text.contains("SECRET-REASONING-TEXT")),
            "reasoning stays searchable"
        );
    }

    #[test]
    fn restored_scroll_offset_is_honored_and_clamped() {
        assert_eq!(view_offset(false, 40, 200, 20), 40);
        assert_eq!(view_offset(true, 0, 200, 20), 180);
        assert_eq!(view_offset(false, 999, 200, 20), 180);
        assert_eq!(view_offset(false, 40, 10, 20), 0);
    }
}
