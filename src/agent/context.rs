use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use crate::ai::{ChatMessage, Role};
use crate::harness::transcript::TokenUsage;

pub const SUMMARY_OPEN: &str = "<conversation-summary>";
pub const SUMMARY_CLOSE: &str = "</conversation-summary>";

pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Prefix of the stand-in left behind where tool output was pruned. Doubles as
/// the "already pruned" marker, which keeps [`prune_tool_outputs`] idempotent.
pub const PRUNED_MARKER: &str = "[output pruned:";

/// How much recent tool output is kept verbatim, and how much must be freed
/// before pruning is worth doing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneSettings {
    pub enabled: bool,
    /// Walking back from the newest message, tool output within this budget is
    /// kept. Everything older is a prune candidate.
    pub protect_tokens: u64,
    /// Candidates are only pruned once they would free at least this much —
    /// rewriting a few hundred tokens is not worth the churn.
    pub minimum_tokens: u64,
}

impl Default for PruneSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            protect_tokens: 40_000,
            minimum_tokens: 20_000,
        }
    }
}

/// What one prune pass removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Tokens freed.
    pub pruned_tokens: u64,
    /// Messages rewritten.
    pub messages: usize,
}

impl PruneReport {
    pub fn did_something(&self) -> bool {
        self.messages > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    pub reserve_tokens: u64,
    /// Recent tokens kept verbatim across a compaction. `0` means adapt to the
    /// model's window.
    pub keep_recent_tokens: u64,
    pub tool_result_cap: usize,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            reserve_tokens: 16_384,
            keep_recent_tokens: 0,
            tool_result_cap: TOOL_RESULT_MAX_CHARS,
        }
    }
}

/// Floor and ceiling for the adaptive keep budget, mirroring OpenCode's
/// `clamp(threshold / 4, 2000, 15000)`.
pub const KEEP_RECENT_MIN: u64 = 2_000;
pub const KEEP_RECENT_MAX: u64 = 15_000;

/// Recent tokens to keep verbatim. An explicit `keep_recent_tokens` wins;
/// otherwise take a quarter of the usable window, clamped. A fixed budget is a
/// poor fit for every model — 20k is most of a small window and a rounding
/// error on a huge one.
pub fn effective_keep_recent(context_window: u64, s: &CompactionSettings) -> u64 {
    if s.keep_recent_tokens > 0 {
        return s.keep_recent_tokens;
    }
    let usable = context_window.saturating_sub(s.reserve_tokens);
    (usable / 4).clamp(KEEP_RECENT_MIN, KEEP_RECENT_MAX)
}

pub type ModelOverrides = HashMap<String, (Option<u64>, Option<u64>)>;

pub fn resolve_settings(
    base: CompactionSettings,
    overrides: &ModelOverrides,
    model: &str,
) -> CompactionSettings {
    let bare = model.rsplit('/').next().unwrap_or(model);
    let mut s = base;
    if let Some((r, k)) = overrides.get(model).or_else(|| overrides.get(bare)) {
        if let Some(r) = r {
            s.reserve_tokens = *r;
        }
        if let Some(k) = k {
            s.keep_recent_tokens = *k;
        }
    }
    s
}


pub fn calculate_context_tokens(usage: TokenUsage) -> u64 {
    usage.context()
}

pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4) + 1
}

pub fn message_tokens(m: &ChatMessage) -> u64 {
    let mut t = estimate_tokens(&m.text);
    for c in &m.tool_calls {
        t += estimate_tokens(&c.name) + estimate_tokens(&c.arguments);
    }
    t
}

pub fn total_tokens(messages: &[ChatMessage]) -> u64 {
    messages.iter().map(message_tokens).sum()
}

pub fn estimate_context_tokens(messages: &[ChatMessage]) -> u64 {
    if let Some(i) = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant && m.tokens.is_some())
    {
        let base = calculate_context_tokens(messages[i].tokens.unwrap());
        let tail: u64 = messages[i + 1..].iter().map(message_tokens).sum();
        return base + tail;
    }
    total_tokens(messages)
}

pub fn should_compact(context_tokens: u64, context_window: u64, s: &CompactionSettings) -> bool {
    if context_window == 0 {
        return false;
    }
    context_tokens > context_window.saturating_sub(s.reserve_tokens)
}

/// Replace the bulk of old tool output with a short stand-in.
///
/// Tool output is what makes a long session expensive: one `cargo build` or
/// `webfetch` can be tens of thousands of tokens, and every later request
/// re-sends it. This keeps a rolling window of the most recent output —
/// `protect_tokens` worth, counting back from the newest message — and elides
/// everything older.
///
/// The current turn is always protected: its output is what the model is
/// actively reasoning about. Pruning stops at the most recent compaction
/// boundary, since a summary already stands in for that history.
///
/// The message itself is kept and only its `text` is replaced, because a
/// provider rejects an assistant `tool_calls` entry with no matching tool
/// result. Pruned output is reported so the caller can log what it freed.
pub fn prune_tool_outputs(messages: &mut [ChatMessage], s: &PruneSettings) -> PruneReport {
    if !s.enabled || messages.is_empty() {
        return PruneReport::default();
    }

    let mut total = 0u64;
    let mut prunable = 0u64;
    let mut candidates: Vec<usize> = Vec::new();
    let mut user_turns = 0usize;

    for i in (0..messages.len()).rev() {
        let m = &messages[i];
        if m.role == Role::User {
            user_turns += 1;
        }
        if user_turns < 2 {
            continue;
        }
        if is_summary(m) {
            break;
        }
        if m.role != Role::Tool {
            continue;
        }
        let text = m.text.trim_start();
        if text.starts_with(PRUNED_MARKER) {
            continue;
        }
        let t = message_tokens(m);
        total = total.saturating_add(t);
        if total <= s.protect_tokens {
            continue;
        }
        prunable = prunable.saturating_add(t);
        candidates.push(i);
    }

    if prunable < s.minimum_tokens {
        return PruneReport::default();
    }

    let mut freed = 0u64;
    for &i in &candidates {
        let t = message_tokens(&messages[i]);
        messages[i].text = pruned_placeholder(t);
        freed = freed.saturating_add(t);
    }
    PruneReport { pruned_tokens: freed, messages: candidates.len() }
}

/// Replace the stand-in text left where output was pruned.
fn pruned_placeholder(tokens: u64) -> String {
    format!(
        "{PRUNED_MARKER} {tokens} tokens elided to save context. \
         Re-run the command if the output is needed again.]"
    )
}


pub fn is_summary(m: &ChatMessage) -> bool {
    m.role == Role::System && m.text.trim_start().starts_with(SUMMARY_OPEN)
}

pub fn system_prefix(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .take_while(|m| m.role == Role::System && !is_summary(m))
        .count()
}

fn is_cut_point(m: &ChatMessage) -> bool {
    matches!(m.role, Role::User | Role::Assistant)
}

fn is_turn_start(m: &ChatMessage) -> bool {
    m.role == Role::User
}

fn find_valid_cut_points(messages: &[ChatMessage], start: usize, end: usize) -> Vec<usize> {
    (start..end).filter(|i| is_cut_point(&messages[*i])).collect()
}

fn find_turn_start(messages: &[ChatMessage], index: usize, start: usize) -> Option<usize> {
    (start..=index).rev().find(|i| is_turn_start(&messages[*i]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPoint {
    pub first_kept: usize,
    pub turn_start: Option<usize>,
    pub is_split_turn: bool,
}

pub fn find_cut_point(
    messages: &[ChatMessage],
    start: usize,
    end: usize,
    keep_recent_tokens: u64,
) -> CutPoint {
    let cut_points = find_valid_cut_points(messages, start, end);
    if cut_points.is_empty() {
        return CutPoint { first_kept: start, turn_start: None, is_split_turn: false };
    }
    let mut accumulated = 0u64;
    let mut cut = cut_points[0];
    for i in (start..end).rev() {
        let t = message_tokens(&messages[i]);
        if t == 0 {
            continue;
        }
        accumulated += t;
        if accumulated >= keep_recent_tokens {
            if let Some(c) = cut_points.iter().find(|c| **c >= i) {
                cut = *c;
            }
            break;
        }
    }
    let starts_turn = is_turn_start(&messages[cut]);
    let turn_start = if starts_turn {
        None
    } else {
        find_turn_start(messages, cut, start)
    };
    CutPoint {
        first_kept: cut,
        is_split_turn: !starts_turn && turn_start.is_some(),
        turn_start,
    }
}


#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOps {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
    pub edited: BTreeSet<String>,
}

impl FileOps {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.written.is_empty() && self.edited.is_empty()
    }

    pub fn extract_from_message(&mut self, m: &ChatMessage) {
        if m.role != Role::Assistant {
            return;
        }
        for c in &m.tool_calls {
            let Some(path) = path_from_args(&c.arguments) else { continue };
            match c.name.as_str() {
                "read" => {
                    self.read.insert(path);
                }
                "write" => {
                    self.written.insert(path);
                }
                "edit" => {
                    self.edited.insert(path);
                }
                _ => {}
            }
        }
    }

    pub fn from_messages(messages: &[ChatMessage]) -> Self {
        let mut ops = FileOps::default();
        for m in messages {
            ops.extract_from_message(m);
        }
        ops
    }

    pub fn from_summary(text: &str) -> Self {
        let mut ops = FileOps::default();
        for path in block_entries(text, "read-files") {
            ops.read.insert(path);
        }
        for path in block_entries(text, "modified-files") {
            ops.edited.insert(path);
        }
        ops
    }

    pub fn merge(&mut self, other: &FileOps) {
        self.read.extend(other.read.iter().cloned());
        self.written.extend(other.written.iter().cloned());
        self.edited.extend(other.edited.iter().cloned());
    }

    pub fn compute_lists(&self) -> (Vec<String>, Vec<String>) {
        let modified: BTreeSet<&String> = self.written.iter().chain(self.edited.iter()).collect();
        let read_only: Vec<String> = self
            .read
            .iter()
            .filter(|f| !modified.contains(f))
            .cloned()
            .collect();
        let modified_files: Vec<String> = modified.into_iter().cloned().collect();
        (read_only, modified_files)
    }

    pub fn format(&self) -> String {
        let (read, modified) = self.compute_lists();
        let mut sections = Vec::new();
        if !read.is_empty() {
            sections.push(format!("<read-files>\n{}\n</read-files>", read.join("\n")));
        }
        if !modified.is_empty() {
            sections.push(format!(
                "<modified-files>\n{}\n</modified-files>",
                modified.join("\n")
            ));
        }
        if sections.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", sections.join("\n\n"))
        }
    }
}

fn path_from_args(args: &str) -> Option<String> {
    let v: Value = serde_json::from_str(args).ok()?;
    for key in ["path", "file_path", "filePath", "file"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn block_entries(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = text.find(&open) else {
        return Vec::new();
    };
    let rest = &text[start + open.len()..];
    let end = rest.find(&close).unwrap_or(rest.len());
    rest[..end]
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}


fn truncate_for_summary(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated = text.chars().count() - max;
    let head: String = text.chars().take(max).collect();
    format!("{head}\n\n[... {truncated} more characters truncated]")
}


pub fn serialize_conversation(messages: &[ChatMessage], tool_result_cap: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        if is_summary(m) {
            parts.push(format!("[Previous summary]: {}", strip_summary_markers(&m.text)));
            continue;
        }
        match m.role {
            Role::System => {
                if !m.text.is_empty() {
                    parts.push(format!("[System]: {}", m.text));
                }
            }
            Role::User => {
                if !m.text.is_empty() {
                    parts.push(format!("[User]: {}", m.text));
                }
            }
            Role::Assistant => {
                if !m.text.is_empty() {
                    parts.push(format!("[Assistant]: {}", m.text));
                }
                if !m.tool_calls.is_empty() {
                    let calls: Vec<String> = m
                        .tool_calls
                        .iter()
                        .map(|c| format!("{}({})", c.name, args_kv(&c.arguments)))
                        .collect();
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Role::Tool => {
                if !m.text.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&m.text, tool_result_cap)
                    ));
                }
            }
        }
    }
    parts.join("\n\n")
}

fn args_kv(args: &str) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(args) else {
        return args.to_string();
    };
    map.iter()
        .map(|(k, v)| format!("{k}={}", serde_json::to_string(v).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn strip_summary_markers(text: &str) -> String {
    let t = text.trim();
    let t = t
        .strip_prefix(SUMMARY_OPEN)
        .unwrap_or(t)
        .strip_suffix(SUMMARY_CLOSE)
        .unwrap_or(t);
    let mut out = String::new();
    let mut skip = false;
    for line in t.lines() {
        let l = line.trim_start();
        if l.starts_with("<read-files>") || l.starts_with("<modified-files>") {
            skip = true;
            continue;
        }
        if l.starts_with("</read-files>") || l.starts_with("</modified-files>") {
            skip = false;
            continue;
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}


pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

pub const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\nUpdate the existing structured summary with new information. RULES:\n- PRESERVE all existing information from the previous summary\n- ADD new progress, decisions, and context from the new messages\n- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed\n- UPDATE \"Next Steps\" based on what was accomplished\n- PRESERVE exact file paths, function names, and error messages\n- If something is no longer relevant, you may remove it\n\nUse this EXACT format:\n\n## Goal\n[Preserve existing goals, add new ones if the task expanded]\n\n## Constraints & Preferences\n- [Preserve existing, add new ones discovered]\n\n## Progress\n### Done\n- [x] [Include previously done items AND newly completed items]\n\n### In Progress\n- [ ] [Current work - update based on progress]\n\n### Blocked\n- [Current blockers - remove if resolved]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale] (preserve all previous, add new)\n\n## Next Steps\n1. [Update based on current state]\n\n## Critical Context\n- [Preserve important context, add new if needed]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.\n\nSummarize the prefix to provide context for the retained suffix:\n\n## Original Request\n[What did the user ask for in this turn?]\n\n## Early Progress\n- [Key decisions and work done in the prefix]\n\n## Context for Suffix\n- [Information needed to understand the retained recent work]\n\nBe concise. Focus on what's needed to understand the kept suffix.";

pub const BRANCH_SUMMARY_PROMPT: &str = "Summarize the work on this branch so it can be carried into a different branch. Cover the goal, what was attempted, decisions made, dead ends, and any files touched. Do not continue the conversation.";

pub fn summarization_user_message(
    conversation: &str,
    previous_summary: Option<&str>,
    custom_instructions: Option<&str>,
) -> String {
    let mut base = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    }
    .to_string();
    if let Some(extra) = custom_instructions.filter(|c| !c.trim().is_empty()) {
        base = format!("{base}\n\nAdditional focus: {extra}");
    }
    let mut out = format!("<conversation>\n{conversation}\n</conversation>\n\n");
    if let Some(prev) = previous_summary {
        out.push_str(&format!("<previous-summary>\n{prev}\n</previous-summary>\n\n"));
    }
    out.push_str(&base);
    out
}

pub const SUMMARY_MAX_TOKENS_CAP: u64 = 4_096;

pub fn summary_max_tokens(reserve_tokens: u64) -> u64 {
    (reserve_tokens * 4 / 5).clamp(256, SUMMARY_MAX_TOKENS_CAP)
}


#[derive(Debug, Clone, PartialEq)]
pub struct Preparation {
    pub first_kept: usize,
    pub messages_to_summarize: Vec<ChatMessage>,
    pub turn_prefix: Vec<ChatMessage>,
    pub is_split_turn: bool,
    pub tokens_before: u64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOps,
}

pub fn prepare(messages: &[ChatMessage], settings: &CompactionSettings) -> Option<Preparation> {
    let prefix = system_prefix(messages);
    let (previous_raw, previous_summary, boundary_start) =
        match messages.iter().rposition(is_summary) {
            Some(i) => {
                let raw = messages[i].text.clone();
                let stripped = strip_summary_markers(&raw);
                (Some(raw), Some(stripped), i + 1)
            }
            None => (None, None, prefix),
        };
    let end = messages.len();
    if end <= boundary_start {
        return None;
    }

    let tokens_before = estimate_context_tokens(messages);
    let cut = find_cut_point(messages, boundary_start, end, settings.keep_recent_tokens);
    let first_kept = cut.first_kept;
    let history_end = if cut.is_split_turn {
        cut.turn_start.unwrap_or(first_kept)
    } else {
        first_kept
    };

    let messages_to_summarize: Vec<ChatMessage> =
        messages[boundary_start..history_end].to_vec();
    let mut turn_prefix: Vec<ChatMessage> = Vec::new();
    if cut.is_split_turn {
        if let Some(ts) = cut.turn_start {
            turn_prefix = messages[ts..first_kept].to_vec();
        }
    }
    if messages_to_summarize.is_empty() && turn_prefix.is_empty() {
        return None;
    }

    let mut file_ops = FileOps::from_messages(&messages_to_summarize);
    for m in &turn_prefix {
        file_ops.extract_from_message(m);
    }
    if let Some(prev) = &previous_raw {
        file_ops.merge(&FileOps::from_summary(prev));
    }

    Some(Preparation {
        first_kept,
        messages_to_summarize,
        turn_prefix,
        is_split_turn: cut.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
    })
}

pub fn repair_tool_calls(messages: &mut Vec<ChatMessage>) {
    let mut declared: HashMap<String, bool> = HashMap::new();
    for m in messages.iter() {
        if m.role == Role::Assistant {
            for c in &m.tool_calls {
                if !c.id.is_empty() {
                    declared.entry(c.id.clone()).or_insert(false);
                }
            }
        }
    }
    for m in messages.iter() {
        if m.role == Role::Tool {
            if let Some(id) = &m.tool_call_id {
                if let Some(answered) = declared.get_mut(id) {
                    *answered = true;
                }
            }
        }
    }

    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for m in messages.drain(..) {
        if m.role == Role::Tool {
            let known = m
                .tool_call_id
                .as_ref()
                .map(|id| declared.contains_key(id))
                .unwrap_or(false);
            if !known {
                continue; 
            }
        }
        if m.role == Role::Assistant && !m.tool_calls.is_empty() {
            let calls = m.tool_calls.clone();
            out.push(m);
            for c in calls {
                if declared.get(&c.id) == Some(&false) {
                    declared.insert(c.id.clone(), true);
                    out.push(ChatMessage::tool_result(
                        c.id,
                        "tool call was not completed (interrupted)",
                    ));
                }
            }
            continue;
        }
        out.push(m);
    }
    *messages = out;
}

pub fn apply_summary(messages: &mut Vec<ChatMessage>, first_kept: usize, summary: &str) {
    let start = system_prefix(messages);
    if first_kept <= start {
        return;
    }
    let kept: Vec<ChatMessage> = messages.split_off(first_kept);
    messages.truncate(start);
    messages.push(ChatMessage::system(format!(
        "{SUMMARY_OPEN}\n{summary}\n{SUMMARY_CLOSE}"
    )));
    messages.extend(kept);
}


pub fn branch_summary_input(branch: &[ChatMessage], budget_tokens: u64, cap: usize) -> Option<String> {
    if branch.is_empty() {
        return None;
    }
    let mut acc = 0u64;
    let mut start = branch.len();
    while start > 0 {
        let t = message_tokens(&branch[start - 1]);
        if acc + t > budget_tokens && start < branch.len() {
            break;
        }
        acc += t;
        start -= 1;
    }
    Some(serialize_conversation(&branch[start..], cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ToolCall;

    fn user(s: &str) -> ChatMessage {
        ChatMessage::user(s)
    }
    fn asst(s: &str) -> ChatMessage {
        ChatMessage::assistant(s, vec![])
    }
    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), arguments: args.into() }
    }

    #[test]
    fn repair_adds_missing_tool_outputs_and_drops_orphans() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            user("q"),
            ChatMessage::assistant("", vec![call("c1", "bash", "{}")]),
        ];
        repair_tool_calls(&mut msgs);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[3].role, Role::Tool);
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("c1"));
        let before = msgs.clone();
        repair_tool_calls(&mut msgs);
        assert_eq!(msgs, before);

        let mut msgs = vec![
            ChatMessage::system("sys"),
            user("q"),
            ChatMessage::tool_result("ghost", "x"),
        ];
        repair_tool_calls(&mut msgs);
        assert_eq!(msgs.len(), 2);

        let mut msgs = vec![
            user("q"),
            ChatMessage::assistant("", vec![call("c1", "read", "{}")]),
            ChatMessage::tool_result("c1", "data"),
        ];
        let before = msgs.clone();
        repair_tool_calls(&mut msgs);
        assert_eq!(msgs, before);
    }

    #[test]
    fn trigger_matches_pi_formula() {
        let s = CompactionSettings::default();
        assert!(should_compact(20_000, 30_000, &s)); 
        assert!(!should_compact(10_000, 30_000, &s));
        assert!(!should_compact(1, 0, &s));
    }

    #[test]
    fn cut_point_lands_on_valid_boundary_and_flags_split_turn() {
        let mut msgs = vec![ChatMessage::system("sys"), user("q")];
        msgs.push(ChatMessage::assistant("", vec![call("c", "read", "{\"path\":\"/a\"}")]));
        msgs.push(ChatMessage::tool_result("c", "data ".repeat(200)));
        msgs.push(asst("final"));
        let cut = find_cut_point(&msgs, 1, msgs.len(), 10);
        assert_ne!(msgs[cut.first_kept].role, Role::Tool);
    }

    #[test]
    fn prepare_collects_span_previous_summary_and_files() {
        let mut msgs = vec![ChatMessage::system("sys")];
        for i in 0..8 {
            msgs.push(user(&format!("q{i} {}", "x".repeat(300))));
            msgs.push(ChatMessage::assistant(
                "edit",
                vec![call(&format!("e{i}"), "edit", "{\"path\":\"/f.rs\"}")],
            ));
            msgs.push(ChatMessage::tool_result(format!("e{i}"), "ok"));
        }
        let settings = CompactionSettings { reserve_tokens: 0, keep_recent_tokens: 300, tool_result_cap: 2000 };
        let prep = prepare(&msgs, &settings).expect("should prepare");
        assert!(prep.tokens_before > 0);
        assert!(!prep.messages_to_summarize.is_empty());
        assert!(prep
            .file_ops
            .compute_lists()
            .1
            .contains(&"/f.rs".to_string()));
    }

    #[test]
    fn iterative_previous_summary_is_folded_and_boundary_advances() {
        let mut msgs = vec![ChatMessage::system("sys")];
        msgs.push(ChatMessage::system(format!(
            "{SUMMARY_OPEN}\nOLD\n<read-files>\n/old.rs\n</read-files>\n{SUMMARY_CLOSE}"
        )));
        for i in 0..6 {
            msgs.push(user(&format!("q{i} {}", "y".repeat(300))));
        }
        let settings = CompactionSettings { reserve_tokens: 0, keep_recent_tokens: 200, tool_result_cap: 2000 };
        let prep = prepare(&msgs, &settings).expect("prepare");
        assert_eq!(prep.previous_summary.as_deref(), Some("OLD"));
        assert!(prep.file_ops.read.contains("/old.rs"));
        assert!(!prep.messages_to_summarize.iter().any(is_summary));
    }

    #[test]
    fn apply_summary_preserves_prefix_and_tail() {
        let mut msgs = vec![ChatMessage::system("sys"), user("a"), user("b"), user("c")];
        apply_summary(&mut msgs, 3, "SUM");
        assert_eq!(msgs[0].text, "sys");
        assert!(is_summary(&msgs[1]));
        assert_eq!(msgs.last().unwrap().text, "c");
        assert_eq!(system_prefix(&msgs), 1);
    }

    #[test]
    fn file_ops_compute_lists_excludes_modified_from_read() {
        let mut ops = FileOps::default();
        ops.read.insert("/a".into());
        ops.read.insert("/b".into());
        ops.edited.insert("/b".into());
        ops.written.insert("/c".into());
        let (read, modified) = ops.compute_lists();
        assert_eq!(read, vec!["/a".to_string()]);
        assert_eq!(modified, vec!["/b".to_string(), "/c".to_string()]);
        let formatted = ops.format();
        assert!(formatted.contains("<read-files>\n/a\n</read-files>"));
        assert!(formatted.contains("/b") && formatted.contains("/c"));
    }

    #[test]
    fn serialization_uses_pi_shape_and_truncates_tool_results() {
        let msgs = vec![
            user("do it"),
            ChatMessage::assistant("", vec![call("c", "bash", "{\"command\":\"ls\"}")]),
            ChatMessage::tool_result("c", "z".repeat(5000)),
        ];
        let text = serialize_conversation(&msgs, 100);
        assert!(text.contains("[User]: do it"));
        assert!(text.contains("[Assistant tool calls]: bash(command=\"ls\")"));
        assert!(text.contains("more characters truncated"));
    }

    #[test]
    fn update_prompt_wraps_previous_summary() {
        let msg = summarization_user_message("[User]: hi", Some("OLD"), None);
        assert!(msg.contains("<conversation>"));
        assert!(msg.contains("<previous-summary>\nOLD\n</previous-summary>"));
        assert!(msg.contains("NEW conversation messages"));
        let initial = summarization_user_message("[User]: hi", None, None);
        assert!(initial.contains("structured context checkpoint"));
        assert!(!initial.contains("previous-summary"));
        assert_eq!(summary_max_tokens(16_384), SUMMARY_MAX_TOKENS_CAP);
        assert_eq!(summary_max_tokens(256), 256);
    }

    #[test]
    fn split_turn_is_detected_for_one_huge_turn() {
        let mut msgs = vec![ChatMessage::system("sys")];
        msgs.push(user("do a very long thing"));
        for i in 0..12 {
            msgs.push(ChatMessage::assistant(
                "a".repeat(400),
                vec![call(&format!("c{i}"), "bash", "{\"command\":\"go\"}")],
            ));
            msgs.push(ChatMessage::tool_result(format!("c{i}"), "r".repeat(400)));
        }
        let settings = CompactionSettings { reserve_tokens: 0, keep_recent_tokens: 200, tool_result_cap: 2000 };
        let prep = prepare(&msgs, &settings).expect("prepare");
        assert!(prep.is_split_turn, "cut landed mid-turn");
        assert!(prep.messages_to_summarize.is_empty(), "no complete turns to summarize");
        assert!(!prep.turn_prefix.is_empty(), "turn prefix is summarized separately");
        assert!(matches!(msgs[prep.first_kept].role, Role::User | Role::Assistant));
    }

    #[test]
    fn branch_input_is_budgeted() {
        let branch: Vec<ChatMessage> = (0..20).map(|i| user(&format!("s{i} {}", "z".repeat(300)))).collect();
        let text = branch_summary_input(&branch, 500, 2000).unwrap();
        assert!(text.contains("s19"));
        assert!(!text.contains("s0"));
        assert!(branch_summary_input(&[], 500, 2000).is_none());
    }


    fn tool_out(id: &str, size: usize) -> ChatMessage {
        ChatMessage::tool_result(id, "z".repeat(size))
    }

    #[test]
    fn prune_elides_old_tool_output_but_keeps_recent() {
        let mut msgs = vec![ChatMessage::system("sys")];
        // Four past turns, each with a big tool result.
        for i in 0..4 {
            msgs.push(user(&format!("turn {i}")));
            msgs.push(tool_out(&format!("t{i}"), 40_000));
        }
        // A fifth, in-flight turn: its output must survive.
        msgs.push(user("current turn"));
        msgs.push(tool_out("t-live", 40_000));

        let s = PruneSettings { enabled: true, protect_tokens: 20_000, minimum_tokens: 1_000 };
        let report = prune_tool_outputs(&mut msgs, &s);

        assert!(report.did_something());
        assert!(report.pruned_tokens > 0);
        // Oldest output is gone...
        assert!(msgs.iter().any(|m| m.text.starts_with(PRUNED_MARKER)));
        assert!(!msgs.iter().any(|m| m.text.contains(&"z".repeat(1000)) && m.text.starts_with(PRUNED_MARKER)));
        // ...and the current turn's output is untouched.
        let live = msgs
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("t-live"))
            .unwrap();
        assert!(!live.text.starts_with(PRUNED_MARKER), "current turn is protected");
        assert_eq!(live.text.len(), 40_000);
    }

    #[test]
    fn prune_is_a_no_op_below_the_minimum() {
        let mut msgs = vec![ChatMessage::system("sys")];
        for i in 0..3 {
            msgs.push(user(&format!("t{i}")));
            msgs.push(tool_out(&format!("c{i}"), 400));
        }
        msgs.push(user("live"));
        msgs.push(tool_out("live", 400));
        let before = msgs.clone();
        // Tiny outputs: nothing worth rewriting.
        let s = PruneSettings { enabled: true, protect_tokens: 10, minimum_tokens: 20_000 };
        let report = prune_tool_outputs(&mut msgs, &s);
        assert_eq!(report, PruneReport::default());
        assert_eq!(msgs, before);
    }

    #[test]
    fn prune_respects_the_protect_budget() {
        let mut msgs = vec![ChatMessage::system("sys")];
        msgs.push(user("old"));
        msgs.push(tool_out("old", 40_000));
        msgs.push(user("recent"));
        msgs.push(tool_out("recent", 4_000));
        // A generous budget covers everything, so nothing is pruned even
        // though the minimum is met by the oldest result.
        let s = PruneSettings { enabled: true, protect_tokens: 1_000_000, minimum_tokens: 1 };
        assert_eq!(prune_tool_outputs(&mut msgs, &s), PruneReport::default());
        assert!(!msgs.iter().any(|m| m.text.starts_with(PRUNED_MARKER)));
    }

    #[test]
    fn pruning_is_idempotent_and_keeps_tool_ids() {
        let mut msgs = vec![ChatMessage::system("sys")];
        for i in 0..4 {
            msgs.push(user(&format!("t{i}")));
            msgs.push(tool_out(&format!("c{i}"), 40_000));
        }
        msgs.push(user("live"));
        msgs.push(tool_out("live", 40_000));
        let s = PruneSettings { enabled: true, protect_tokens: 20_000, minimum_tokens: 1_000 };

        let first = prune_tool_outputs(&mut msgs, &s);
        assert!(first.did_something());
        // Every pruned message still answers its call, so the request stays
        // valid for strict providers.
        for m in msgs.iter().filter(|m| m.role == Role::Tool) {
            assert!(m.tool_call_id.is_some(), "a tool result must keep its call id");
        }
        // Already-pruned text is not offered as a candidate again.
        let second = prune_tool_outputs(&mut msgs, &s);
        assert_eq!(second.pruned_tokens, 0);
    }

    #[test]
    fn prune_stops_at_a_compaction_boundary() {
        let mut msgs = vec![ChatMessage::system("sys")];
        msgs.push(user("ancient"));
        msgs.push(tool_out("ancient", 40_000));
        // A summary stands in for everything above it.
        msgs.push(ChatMessage::system(format!("{SUMMARY_OPEN}\nSUM\n{SUMMARY_CLOSE}")));
        for i in 0..4 {
            msgs.push(user(&format!("t{i}")));
            msgs.push(tool_out(&format!("c{i}"), 40_000));
        }
        msgs.push(user("live"));
        msgs.push(tool_out("live", 40_000));
        let s = PruneSettings { enabled: true, protect_tokens: 20_000, minimum_tokens: 1_000 };
        prune_tool_outputs(&mut msgs, &s);
        let above = msgs
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("ancient"))
            .unwrap();
        assert!(
            !above.text.starts_with(PRUNED_MARKER),
            "history above a summary is already accounted for"
        );
    }

    #[test]
    fn prune_can_be_disabled() {
        let mut msgs = vec![ChatMessage::system("sys"), user("a")];
        msgs.push(tool_out("big", 40_000));
        msgs.push(user("b"));
        msgs.push(tool_out("big2", 40_000));
        let before = msgs.clone();
        let s = PruneSettings { enabled: false, protect_tokens: 0, minimum_tokens: 0 };
        assert_eq!(prune_tool_outputs(&mut msgs, &s), PruneReport::default());
        assert_eq!(msgs, before);
    }

    #[test]
    fn adaptive_keep_recent_scales_with_the_window() {
        // An explicit budget always wins.
        let explicit = CompactionSettings { keep_recent_tokens: 7_000, ..Default::default() };
        assert_eq!(effective_keep_recent(1_000_000, &explicit), 7_000);

        let auto = CompactionSettings { keep_recent_tokens: 0, ..Default::default() };
        // A quarter of the usable window, clamped at both ends.
        assert_eq!(effective_keep_recent(80_000, &auto), KEEP_RECENT_MAX);
        assert_eq!(effective_keep_recent(1_000_000, &auto), KEEP_RECENT_MAX);
        assert_eq!(effective_keep_recent(4_000, &auto), KEEP_RECENT_MIN);
    }

    #[test]
    fn resolve_settings_per_model() {
        let base = CompactionSettings::default();
        let mut ov = ModelOverrides::new();
        ov.insert("openai/gpt-4o".into(), (Some(400_000), None));
        assert_eq!(resolve_settings(base, &ov, "openai/gpt-4o").reserve_tokens, 400_000);
        assert_eq!(resolve_settings(base, &ov, "other").reserve_tokens, base.reserve_tokens);
    }
}
