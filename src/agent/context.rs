//! Context compaction, mirroring Pi's `core/compaction` architecture.
//!
//! - `should_compact` triggers at `contextWindow - reserveTokens`.
//! - `find_cut_point` walks backwards accumulating estimated message sizes,
//!   cutting only at user/assistant messages (never a tool result), and
//!   detects a **split turn** when the cut lands mid-turn.
//! - `prepare` collects `messagesToSummarize` + `turnPrefixMessages`,
//!   the previous summary, `tokensBefore` and file operations.
//! - summaries use Pi's structured prompt (initial vs iterative update) with
//!   the conversation wrapped in `<conversation>` and the prior summary in
//!   `<previous-summary>`.
//! - file tracking is cumulative (`read` minus `written`/`edited`).
//! - branch summarization reuses the same serialization for tree navigation.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use crate::ai::{ChatMessage, Role};
use crate::harness::transcript::TokenUsage;

pub const SUMMARY_OPEN: &str = "<conversation-summary>";
pub const SUMMARY_CLOSE: &str = "</conversation-summary>";

/// Maximum characters for a tool result in serialized summaries (Pi's value).
pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
    pub tool_result_cap: usize,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            tool_result_cap: TOOL_RESULT_MAX_CHARS,
        }
    }
}

pub type ModelOverrides = HashMap<String, (Option<u64>, Option<u64>)>;

/// Resolve effective settings for `model` (Pi's independent per-field fallback).
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

// ---------------------------------------------------------------------------
// Token accounting (Pi: calculateContextTokens / estimateTokens)
// ---------------------------------------------------------------------------

/// Prompt-side context size for an assistant usage.
pub fn calculate_context_tokens(usage: TokenUsage) -> u64 {
    usage.context()
}

pub fn estimate_tokens(text: &str) -> u64 {
    ((text.chars().count() as u64) + 3) / 4 + 1
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

/// Best available context size. Without a cached provider usage this is the
/// estimate over all messages (Pi uses the last assistant usage + tail).
pub fn estimate_context_tokens(messages: &[ChatMessage]) -> u64 {
    total_tokens(messages)
}

/// Pi's trigger: `contextTokens > contextWindow - reserveTokens`.
pub fn should_compact(context_tokens: u64, context_window: u64, s: &CompactionSettings) -> bool {
    if context_window == 0 {
        return false;
    }
    context_tokens > context_window.saturating_sub(s.reserve_tokens)
}

// ---------------------------------------------------------------------------
// Cut points and turn detection (Pi: findCutPoint / isTurnStart)
// ---------------------------------------------------------------------------

pub fn is_summary(m: &ChatMessage) -> bool {
    m.role == Role::System && m.text.trim_start().starts_with(SUMMARY_OPEN)
}

/// System messages before any prior summary are preserved verbatim.
pub fn system_prefix(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .take_while(|m| m.role == Role::System && !is_summary(m))
        .count()
}

/// A message a cut may land on (user/assistant; never a tool result or system).
fn is_cut_point(m: &ChatMessage) -> bool {
    matches!(m.role, Role::User | Role::Assistant)
}

/// A user message starts a turn.
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
    /// First message to keep.
    pub first_kept: usize,
    /// User message that starts the turn being split (when splitting).
    pub turn_start: Option<usize>,
    pub is_split_turn: bool,
}

/// Walk backwards keeping `keep_recent_tokens`, cutting only at valid points.
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

// ---------------------------------------------------------------------------
// File operations (Pi: FileOperations / computeFileLists / formatFileOperations)
// ---------------------------------------------------------------------------

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

    /// Pi: only `read`/`write`/`edit` tool calls contribute.
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

    /// Parse `<read-files>` / `<modified-files>` from a previous summary.
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

    /// (files only read, modified files) — sorted; read excludes modified.
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

    /// Pi's `formatFileOperations`: leading blank lines, sections only if
    /// non-empty.
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

// ---------------------------------------------------------------------------
// Serialization (Pi: serializeConversation)
// ---------------------------------------------------------------------------

fn truncate_for_summary(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated = text.chars().count() - max;
    let head: String = text.chars().take(max).collect();
    format!("{head}\n\n[... {truncated} more characters truncated]")
}

/// Serialize a span as text so the summarizer treats it as data, not a
/// conversation to continue.
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

/// `k=v, k=v` rendering of a tool-call argument object (Pi uses `JSON.stringify`).
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

// ---------------------------------------------------------------------------
// Prompts (Pi verbatim)
// ---------------------------------------------------------------------------

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.\n\nUse this EXACT format:\n\n## Goal\n[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned by user]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Current work]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [Ordered list of what should happen next]\n\n## Critical Context\n- [Any data, examples, or references needed to continue]\n- [Or \"(none)\" if not applicable]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

pub const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\nUpdate the existing structured summary with new information. RULES:\n- PRESERVE all existing information from the previous summary\n- ADD new progress, decisions, and context from the new messages\n- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed\n- UPDATE \"Next Steps\" based on what was accomplished\n- PRESERVE exact file paths, function names, and error messages\n- If something is no longer relevant, you may remove it\n\nUse this EXACT format:\n\n## Goal\n[Preserve existing goals, add new ones if the task expanded]\n\n## Constraints & Preferences\n- [Preserve existing, add new ones discovered]\n\n## Progress\n### Done\n- [x] [Include previously done items AND newly completed items]\n\n### In Progress\n- [ ] [Current work - update based on progress]\n\n### Blocked\n- [Current blockers - remove if resolved]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale] (preserve all previous, add new)\n\n## Next Steps\n1. [Update based on current state]\n\n## Critical Context\n- [Preserve important context, add new if needed]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = "This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.\n\nSummarize the prefix to provide context for the retained suffix:\n\n## Original Request\n[What did the user ask for in this turn?]\n\n## Early Progress\n- [Key decisions and work done in the prefix]\n\n## Context for Suffix\n- [Information needed to understand the retained recent work]\n\nBe concise. Focus on what's needed to understand the kept suffix.";

pub const BRANCH_SUMMARY_PROMPT: &str = "Summarize the work on this branch so it can be carried into a different branch. Cover the goal, what was attempted, decisions made, dead ends, and any files touched. Do not continue the conversation.";

/// Build the summarization request text: `<conversation>` + optional
/// `<previous-summary>` + the initial/update prompt (+ custom focus).
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

/// Summary token cap: `min(0.8 * reserveTokens, model max)` (Pi).
pub fn summary_max_tokens(reserve_tokens: u64) -> u64 {
    (reserve_tokens * 4 / 5).max(256)
}

// ---------------------------------------------------------------------------
// Preparation
// ---------------------------------------------------------------------------

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

/// Pi's `prepareCompaction`: choose the cut, gather the summary spans, previous
/// summary, `tokensBefore` and cumulative file operations.
pub fn prepare(messages: &[ChatMessage], settings: &CompactionSettings) -> Option<Preparation> {
    let prefix = system_prefix(messages);
    // Boundary starts after the previous summary (iterative fold-forward).
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

/// Replace `messages[system_prefix..first_kept]` with a summary message,
/// preserving the leading system prefix and the kept tail.
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

// ---------------------------------------------------------------------------
// Branch summarization (Pi: branch-summarization)
// ---------------------------------------------------------------------------

/// Serialize the newest entries of an abandoned branch up to a token budget.
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
    fn trigger_matches_pi_formula() {
        let s = CompactionSettings::default();
        assert!(should_compact(20_000, 30_000, &s)); // > 30000-16384
        assert!(!should_compact(10_000, 30_000, &s));
        assert!(!should_compact(1, 0, &s));
    }

    #[test]
    fn cut_point_lands_on_valid_boundary_and_flags_split_turn() {
        // system + a turn: user, assistant(tool call), tool result, assistant.
        let mut msgs = vec![ChatMessage::system("sys"), user("q")];
        msgs.push(ChatMessage::assistant("", vec![call("c", "read", "{\"path\":\"/a\"}")]));
        msgs.push(ChatMessage::tool_result("c", "data ".repeat(200)));
        msgs.push(asst("final"));
        // Small budget keeps only the tail; the cut must never be the tool result.
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
        // The previous summary's file is carried forward.
        assert!(prep.file_ops.read.contains("/old.rs"));
        // The summarized span starts after the previous summary message.
        assert!(!prep.messages_to_summarize.iter().any(|m| is_summary(m)));
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
        assert_eq!(summary_max_tokens(16_384), 13_107);
    }

    #[test]
    fn split_turn_is_detected_for_one_huge_turn() {
        // A single turn whose assistant/tool work exceeds the keep budget.
        let mut msgs = vec![ChatMessage::system("sys")];
        msgs.push(user("do a very long thing"));
        for i in 0..12 {
            msgs.push(ChatMessage::assistant(
                &"a".repeat(400),
                vec![call(&format!("c{i}"), "bash", "{\"command\":\"go\"}")],
            ));
            msgs.push(ChatMessage::tool_result(format!("c{i}"), "r".repeat(400)));
        }
        let settings = CompactionSettings { reserve_tokens: 0, keep_recent_tokens: 200, tool_result_cap: 2000 };
        let prep = prepare(&msgs, &settings).expect("prepare");
        assert!(prep.is_split_turn, "cut landed mid-turn");
        assert!(prep.messages_to_summarize.is_empty(), "no complete turns to summarize");
        assert!(!prep.turn_prefix.is_empty(), "turn prefix is summarized separately");
        // The retained suffix still starts on a valid boundary.
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

    #[test]
    fn resolve_settings_per_model() {
        let base = CompactionSettings::default();
        let mut ov = ModelOverrides::new();
        ov.insert("openai/gpt-4o".into(), (Some(400_000), None));
        assert_eq!(resolve_settings(base, &ov, "openai/gpt-4o").reserve_tokens, 400_000);
        assert_eq!(resolve_settings(base, &ov, "other").reserve_tokens, base.reserve_tokens);
    }
}
