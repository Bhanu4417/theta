use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::ai::ToolSpec;

pub const MAX_TOOL_BYTES: usize = 100 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub output: String,
    /// Extra fields for the UI. Used to carry a diff for an edit, which is what
    /// lets the transcript show what changed rather than only which file did.
    pub metadata: Value,
}

impl ToolOutcome {
    pub fn ok(output: impl Into<String>) -> Self {
        Self { ok: true, output: output.into(), metadata: json!({}) }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self { ok: false, output: message.into(), metadata: json!({}) }
    }

    /// Attach a unified diff, so the UI can render the change.
    pub fn with_diff(mut self, diff: impl Into<String>) -> Self {
        let diff = diff.into();
        if !diff.trim().is_empty() {
            self.metadata["diff"] = Value::String(diff);
        }
        self
    }
}

/// A compact unified diff between two versions of a file's text.
///
/// Trims the shared prefix and suffix so the result is one hunk covering only
/// the region that changed. A general diff algorithm is not needed: every tool
/// here replaces one contiguous region and knows it.
///
/// Returns `None` when nothing changed, so a caller does not report an empty
/// diff as a change.
pub fn unified_diff(path: &str, before: &str, after: &str) -> Option<String> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();

    let mut pre = 0usize;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < a.len().saturating_sub(pre)
        && suf < b.len().saturating_sub(pre)
        && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    if pre == a.len() && pre == b.len() {
        return None;
    }

    const CTX: usize = 3;
    let ctx_start = pre.saturating_sub(CTX);
    let trailing = suf.min(CTX);
    let old_mid = a.len().saturating_sub(suf) - pre;
    let new_mid = b.len().saturating_sub(suf) - pre;

    let mut out = String::new();
    out.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
    let lead = pre - ctx_start;
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        ctx_start + 1,
        lead + old_mid + trailing,
        ctx_start + 1,
        lead + new_mid + trailing
    ));
    for l in &a[ctx_start..pre] {
        out.push(' ');
        out.push_str(l);
        out.push('\n');
    }
    for l in &a[pre..a.len() - suf] {
        out.push('-');
        out.push_str(l);
        out.push('\n');
    }
    for l in &b[pre..b.len() - suf] {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    for l in &a[a.len() - suf..a.len() - suf + trailing] {
        out.push(' ');
        out.push_str(l);
        out.push('\n');
    }
    Some(out)
}

pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn run<'a>(
        &'a self,
        input: &'a Value,
        cwd: &'a Path,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

pub trait PermissionGate: Send + Sync {
    fn check(&self, tool: &str, input: &Value, cwd: &Path) -> PermissionDecision;
}

pub struct AllowAll;
impl PermissionGate for AllowAll {
    fn check(&self, _tool: &str, _input: &Value, _cwd: &Path) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

/// Allows anything that stays inside the session's own folder, and asks for
/// anything else.
///
/// This is what makes several sessions usable at once: each one is opened in a
/// folder and has that folder pre-approved, so working in your own project never
/// interrupts you. Reaching into another folder — someone else's session, a
/// config directory, a path outside the project — asks, because that is the
/// point where a mistake becomes someone else's problem.
///
/// A shell command cannot be judged by its arguments, so it always asks: the
/// path check has nothing to inspect, and a command can touch anything the user
/// can.
pub struct ScopedGate;
impl PermissionGate for ScopedGate {
    fn check(&self, tool: &str, input: &Value, cwd: &Path) -> PermissionDecision {
        if tool_acts_within(tool, input, cwd) {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Ask
        }
    }
}

/// The scoped policy when there is no one to ask: inside the folder is allowed,
/// outside is refused.
///
/// Used headless. Silently allowing instead would widen the permission the user
/// chose without telling them; a refusal at least appears in the output.
pub struct ScopedStrict;
impl PermissionGate for ScopedStrict {
    fn check(&self, tool: &str, input: &Value, cwd: &Path) -> PermissionDecision {
        if tool_acts_within(tool, input, cwd) {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny
        }
    }
}

pub struct DenyAll;
impl PermissionGate for DenyAll {
    fn check(&self, _tool: &str, _input: &Value, _cwd: &Path) -> PermissionDecision {
        PermissionDecision::Deny
    }
}

pub struct ReadOnly;
impl PermissionGate for ReadOnly {
    fn check(&self, tool: &str, _input: &Value, _cwd: &Path) -> PermissionDecision {
        match tool {
            "read" | "grep" | "glob" | "webfetch" => PermissionDecision::Allow,
            _ => PermissionDecision::Deny,
        }
    }
}

/// The path arguments a tool operates on, if any.
///
/// Multi-edit carries one path per edit, and every one of them has to be inside
/// the folder for the call to be pre-approved — approving on the first would let
/// the rest write anywhere.
fn target_paths<'a>(tool: &str, input: &'a Value) -> Vec<&'a str> {
    let single = |keys: &[&str]| -> Vec<&'a str> {
        keys.iter()
            .find_map(|k| input.get(*k).and_then(Value::as_str))
            .filter(|p| !p.trim().is_empty())
            .into_iter()
            .collect()
    };
    match tool {
        "read" | "grep" | "glob" | "write" => single(&[
            "path",
            "file_path",
            "filePath",
            "dir",
        ]),
        "edit" => single(&["path", "file_path", "filePath"]),
        "multiedit" => input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| {
                edits
                    .iter()
                    .filter_map(|e| {
                        ["path", "file_path", "filePath"]
                            .iter()
                            .find_map(|k| e.get(*k).and_then(Value::as_str))
                    })
                    .filter(|p| !p.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Whether a tool acts only inside the session folder.
///
/// A tool with no path argument that still touches the filesystem is not
/// pre-approved: `bash` can do anything, so there is nothing to check and the
/// safe answer is to ask.
fn tool_acts_within(tool: &str, input: &Value, cwd: &Path) -> bool {
    match tool {
        // Asking the user a question, or reading the network, touches no folder.
        "webfetch" | "ask" => true,
        "read" | "grep" | "glob" | "write" | "edit" | "multiedit" => {
            let paths = target_paths(tool, input);
            if paths.is_empty() {
                // A read with no path defaults to the folder; a write with no
                // path is malformed and the tool will reject it.
                return matches!(tool, "read" | "grep" | "glob");
            }
            paths.iter().all(|p| path_within(cwd, p))
        }
        // `task` delegates, and the sub-agent inherits this same gate.
        "task" => true,
        // `bash` and anything unknown: nothing to inspect, so ask.
        _ => false,
    }
}

/// Whether `p`, interpreted relative to `cwd`, stays inside `cwd`.
///
/// Resolved in two steps, because either alone is wrong:
///
/// - `canonicalize` follows symlinks, which lexical normalisation cannot, but it
///   fails for a path that does not exist yet — the common case for a file about
///   to be written.
/// - Lexical normalisation always works, and is what stops `..` escaping.
///
/// A plain `starts_with` on the un-normalised join is not enough: the string
/// `/work/proj/../sibling` starts with `/work/proj` while pointing outside it, so
/// a `..` would have been pre-approved.
fn path_within(cwd: &Path, p: &str) -> bool {
    let base = normalize_lexically(&cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()));
    let joined = resolve(cwd, p);

    // A path that exists can be fully resolved, following symlinks.
    if let Ok(real) = joined.canonicalize() {
        return real.starts_with(&base);
    }

    // Otherwise judge the directory it would land in, so a symlinked parent is
    // still followed, then normalise the leaf lexically.
    let leaf = joined.file_name().map(|n| n.to_owned());
    let parent = joined.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let resolved_parent = parent.canonicalize().unwrap_or(parent);
    let mut target = normalize_lexically(&resolved_parent);
    if let Some(leaf) = leaf {
        target.push(leaf);
    }
    normalize_lexically(&target).starts_with(&base)
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalize_lexically(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the root is a no-op, as the OS treats it.
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}


pub(crate) fn str_arg(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| input.get(*k).and_then(|v| v.as_str()).map(|s| s.to_string()))
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn cap(mut s: String, limit: usize) -> String {
    if s.len() > limit {
        let mut end = limit;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n… [output truncated]");
    }
    s
}

pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => {
                if p.get(1) == Some(&b'*') {
                    (0..=t.len()).any(|i| m(&p[2..], &t[i..]))
                } else {
                    (0..=t.len()).any(|i| !t[..i].contains(&b'/') && m(&p[1..], &t[i..]))
                }
            }
            Some(b'?') => t.first().is_some_and(|c| *c != b'/') && m(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && m(&p[1..], &t[1..]),
        }
    }
    m(pattern.as_bytes(), text.as_bytes())
}


pub struct ReadTool;
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a file's contents.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(path) = str_arg(input, &["path", "file_path", "filePath"]) else {
                return ToolOutcome::err("missing required argument: path");
            };
            match tokio::fs::read_to_string(resolve(cwd, &path)).await {
                Ok(text) => ToolOutcome::ok(cap(text, MAX_TOOL_BYTES)),
                Err(e) => ToolOutcome::err(format!("read {path}: {e}")),
            }
        })
    }
}

pub struct WriteTool;
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write (create or overwrite) a file.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
                "required": ["path", "content"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(path) = str_arg(input, &["path", "file_path", "filePath"]) else {
                return ToolOutcome::err("missing required argument: path");
            };
            let content = str_arg(input, &["content", "text"]).unwrap_or_default();
            let full = resolve(cwd, &path);
            if let Some(parent) = full.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return ToolOutcome::err(format!("mkdir {}: {e}", parent.display()));
                }
            }
            // What the file held before, when it existed, so overwriting shows
            // what was lost rather than only that something was written.
            let before = tokio::fs::read_to_string(&full).await.unwrap_or_default();
            match tokio::fs::write(&full, content.as_bytes()).await {
                Ok(()) => {
                    let outcome =
                        ToolOutcome::ok(format!("wrote {} ({} bytes)", full.display(), content.len()));
                    match unified_diff(&path, &before, &content) {
                        Some(d) => outcome.with_diff(d),
                        None => outcome,
                    }
                }
                Err(e) => ToolOutcome::err(format!("write {path}: {e}")),
            }
        })
    }
}

pub struct EditTool;
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace the first occurrence of `old` with `new` in a file.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old": { "type": "string" },
                    "new": { "type": "string" }
                },
                "required": ["path", "old", "new"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(path) = str_arg(input, &["path", "file_path", "filePath"]) else {
                return ToolOutcome::err("missing required argument: path");
            };
            let old = str_arg(input, &["old", "old_string", "oldString"]).unwrap_or_default();
            let new = str_arg(input, &["new", "new_string", "newString"]).unwrap_or_default();
            if old.is_empty() {
                return ToolOutcome::err("missing required argument: old");
            }
            let full = resolve(cwd, &path);
            let text = match tokio::fs::read_to_string(&full).await {
                Ok(t) => t,
                Err(e) => return ToolOutcome::err(format!("edit {path}: {e}")),
            };
            let Some(pos) = text.find(&old) else {
                return ToolOutcome::err(format!("pattern not found in {path}"));
            };
            let mut updated = String::with_capacity(text.len() + new.len());
            updated.push_str(&text[..pos]);
            updated.push_str(&new);
            updated.push_str(&text[pos + old.len()..]);
            match tokio::fs::write(&full, updated.as_bytes()).await {
                Ok(()) => {
                    // Carry the change itself, not just the file name: the
                    // transcript shows the diff, so the reader sees what moved.
                    let outcome = ToolOutcome::ok(format!("edited {path}"));
                    match unified_diff(&path, &text, &updated) {
                        Some(d) => outcome.with_diff(d),
                        None => outcome,
                    }
                }
                Err(e) => ToolOutcome::err(format!("edit {path}: {e}")),
            }
        })
    }
}

pub struct MultiEditTool;
impl Tool for MultiEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "multiedit".into(),
            description: "Apply multiple edits atomically (all or nothing).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "old": { "type": "string" },
                                "new": { "type": "string" }
                            },
                            "required": ["path", "old", "new"]
                        }
                    }
                },
                "required": ["edits"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(edits) = input.get("edits").and_then(|e| e.as_array()) else {
                return ToolOutcome::err("missing required argument: edits");
            };
            if edits.is_empty() {
                return ToolOutcome::err("edits is empty");
            }
            let mut applied = 0usize;
            let mut diffs: Vec<String> = Vec::new();
            for e in edits {
                let Some(path) = str_arg(e, &["path", "file_path", "filePath"]) else {
                    return ToolOutcome::err("each edit needs a path");
                };
                let old = str_arg(e, &["old", "old_string", "oldString"]).unwrap_or_default();
                let new = str_arg(e, &["new", "new_string", "newString"]).unwrap_or_default();
                let full = resolve(cwd, &path);
                let text = match tokio::fs::read_to_string(&full).await {
                    Ok(t) => t,
                    Err(err) => return ToolOutcome::err(format!("multiedit {path}: {err}")),
                };
                let Some(pos) = text.find(&old) else {
                    return ToolOutcome::err(format!("multiedit {path}: pattern not found"));
                };
                let mut updated = String::with_capacity(text.len() + new.len());
                updated.push_str(&text[..pos]);
                updated.push_str(&new);
                updated.push_str(&text[pos + old.len()..]);
                if let Err(err) = tokio::fs::write(&full, updated.as_bytes()).await {
                    return ToolOutcome::err(format!("multiedit {path}: {err}"));
                }
                // Keep each file's change, so the transcript shows all of them
                // rather than a bare count.
                if let Some(d) = unified_diff(&path, &text, &updated) {
                    diffs.push(d);
                }
                applied += 1;
            }
            let outcome = ToolOutcome::ok(format!("applied {applied} edit(s)"));
            if diffs.is_empty() {
                outcome
            } else {
                outcome.with_diff(diffs.join(""))
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub before: Option<String>,
}

pub fn snapshot_paths(tool_name: &str, input: &Value, cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match tool_name {
        "write" | "edit" => {
            if let Some(p) = str_arg(input, &["path", "file_path", "filePath"]) {
                out.push(resolve(cwd, &p));
            }
        }
        "multiedit" => {
            if let Some(edits) = input.get("edits").and_then(|e| e.as_array()) {
                for e in edits {
                    if let Some(p) = str_arg(e, &["path", "file_path", "filePath"]) {
                        out.push(resolve(cwd, &p));
                    }
                }
            }
        }
        _ => {}
    }
    out
}

pub type SubAgentBuilder = Arc<
    dyn Fn(&str) -> Result<crate::agent::AgentLoop, crate::providers::ProviderError> + Send + Sync,
>;

pub struct TaskTool {
    build: SubAgentBuilder,
}

impl TaskTool {
    pub fn new(build: SubAgentBuilder) -> Self {
        Self { build }
    }
}

impl Tool for TaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "task".into(),
            description: "Delegate a self-contained sub-task to a sub-agent and get its report. \
                          The sub-agent has the same tools but no interactive prompts."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "Short 3-5 word description" },
                    "prompt": { "type": "string", "description": "Self-contained instructions for the sub-agent" },
                    "subagent_type": {
                        "type": "string",
                        "description": "Which sub-agent to run (default general)",
                        "enum": crate::agent::agents::subagent_names()
                    }
                },
                "required": ["prompt"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(prompt) = str_arg(input, &["prompt", "description"]) else {
                return ToolOutcome::err("task: `prompt` is required");
            };
            if prompt.trim().is_empty() {
                return ToolOutcome::err("task: `prompt` is empty");
            }
            let ty = str_arg(input, &["subagent_type", "subagentType"])
                .unwrap_or_else(|| "general".to_string());
            if crate::agent::agents::find(&ty).is_none() {
                return ToolOutcome::err(format!("task: unknown subagent_type '{ty}'"));
            }
            let agent = match (self.build)(&ty) {
                Ok(a) => a,
                Err(e) => return ToolOutcome::err(format!("task: could not start sub-agent: {e}")),
            };
            let mut history = Vec::new();
            let mut emit = |_e: crate::harness::HarnessEvent| {};
            if let Err(e) = agent.run_turn(&mut history, &prompt, cwd, &mut emit).await {
                return ToolOutcome::err(format!("task: sub-agent failed: {e}"));
            }
            let report = history
                .iter()
                .filter(|m| m.role == crate::ai::Role::Assistant && !m.text.trim().is_empty())
                .map(|m| m.text.trim().to_string())
                .collect::<Vec<_>>()
                .join("\n\n");
            if report.is_empty() {
                ToolOutcome::err("task: sub-agent produced no output")
            } else {
                ToolOutcome::ok(cap(report, MAX_TOOL_BYTES))
            }
        })
    }
}

pub struct AskTool;
impl Tool for AskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask".into(),
            description: "Ask the user a question with selectable options.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": { "type": "string" },
                                "header": { "type": "string" },
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" }
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "multiple": { "type": "boolean" },
                                "custom": { "type": "boolean" }
                            },
                            "required": ["question"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        }
    }
    fn run<'a>(&'a self, _input: &'a Value, _cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move { ToolOutcome::err("ask is handled by the agent loop") })
    }
}

pub struct BashTool;
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command in the project directory.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_ms": { "type": "integer" }
                },
                "required": ["command"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(cmd) = str_arg(input, &["command", "cmd"]) else {
                return ToolOutcome::err("missing required argument: command");
            };
            let timeout = input
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(120_000)
                .min(600_000);
            let fut = tokio::process::Command::new("bash")
                .arg("-lc")
                .arg(&cmd)
                .current_dir(cwd)
                .stdin(std::process::Stdio::null())
                .output();
            match tokio::time::timeout(Duration::from_millis(timeout), fut).await {
                Err(_) => ToolOutcome::err(format!("command timed out after {timeout}ms")),
                Ok(Err(e)) => {
                    let hint = if cfg!(windows) {
                        " (a `bash` is required on Windows: install Git for Windows or use WSL, or run commands with the `!cmd` escape)"
                    } else {
                        ""
                    };
                    ToolOutcome::err(format!("could not run bash: {e}{hint}"))
                }
                Ok(Ok(out)) => {
                    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
                    let err = String::from_utf8_lossy(&out.stderr);
                    if !err.trim().is_empty() {
                        s.push_str("\n[stderr]\n");
                        s.push_str(&err);
                    }
                    let body = cap(s, MAX_TOOL_BYTES);
                    if out.status.success() {
                        ToolOutcome::ok(body)
                    } else {
                        ToolOutcome::err(format!("exit {}: {body}", out.status.code().unwrap_or(-1)))
                    }
                }
            }
        })
    }
}

pub struct GrepTool;
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents for a substring.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(pat) = str_arg(input, &["pattern", "query"]) else {
                return ToolOutcome::err("missing required argument: pattern");
            };
            let root = resolve(cwd, &str_arg(input, &["path"]).unwrap_or_else(|| ".".into()));
            let cwd = cwd.to_path_buf();
            let hits = tokio::task::spawn_blocking(move || {
                let _ = &cwd;
                crate::fsx::search_local(&root, &pat, 200)
            })
            .await
            .unwrap_or_default();
            if hits.is_empty() {
                return ToolOutcome::ok("no matches");
            }
            let body: Vec<String> = hits
                .iter()
                .map(|m| format!("{}:{}: {}", m.path, m.line, m.text.trim()))
                .collect();
            ToolOutcome::ok(cap(body.join("\n"), MAX_TOOL_BYTES))
        })
    }
}

pub struct GlobTool;
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files by glob pattern (e.g. `src/**/*.rs`).".into(),
            parameters: json!({
                "type": "object",
                "properties": { "pattern": { "type": "string" } },
                "required": ["pattern"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(pat) = str_arg(input, &["pattern", "query"]) else {
                return ToolOutcome::err("missing required argument: pattern");
            };
            let root = cwd.to_path_buf();
            let files = tokio::task::spawn_blocking(move || {
                let mut out = Vec::new();
                let walker = ignore::WalkBuilder::new(&root)
                    .hidden(true)
                    .git_ignore(true)
                    .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "target")
                    .build();
                for e in walker.flatten() {
                    if out.len() >= 200 {
                        break;
                    }
                    if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        let rel = crate::fsx::rel_slash(e.path(), &root);
                        if glob_match(&pat, &rel) {
                            out.push(rel);
                        }
                    }
                }
                out
            })
            .await
            .unwrap_or_default();
            if files.is_empty() {
                ToolOutcome::ok("no files matched")
            } else {
                ToolOutcome::ok(cap(files.join("\n"), MAX_TOOL_BYTES))
            }
        })
    }
}

pub struct WebFetchTool;
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "webfetch".into(),
            description: "Fetch a URL and return its text body.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "url": { "type": "string" } },
                "required": ["url"]
            }),
        }
    }
    fn run<'a>(&'a self, input: &'a Value, _cwd: &'a Path) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let Some(url) = str_arg(input, &["url"]) else {
                return ToolOutcome::err("missing required argument: url");
            };
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return ToolOutcome::err("url must start with http:// or https://");
            }
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
            {
                Ok(c) => c,
                Err(e) => return ToolOutcome::err(format!("client: {e}")),
            };
            match client.get(&url).send().await {
                Err(e) => ToolOutcome::err(format!("fetch {url}: {e}")),
                Ok(resp) => {
                    let status = resp.status();
                    match resp.text().await {
                        Ok(text) => {
                            let body = cap(text, MAX_TOOL_BYTES);
                            if status.is_success() {
                                ToolOutcome::ok(body)
                            } else {
                                ToolOutcome::err(format!("fetch {url}: HTTP {status}\n{body}"))
                            }
                        }
                        Err(e) => ToolOutcome::err(format!("fetch {url}: {e}")),
                    }
                }
            }
        })
    }
}

pub fn default_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(MultiEditTool),
        Arc::new(BashTool),
        Arc::new(GrepTool),
        Arc::new(GlobTool),
        Arc::new(WebFetchTool),
        Arc::new(AskTool),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matcher_handles_common_patterns() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"), "* does not cross /");
        assert!(glob_match("src/**/*.rs", "src/a/b/main.rs"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[tokio::test]
    async fn write_read_edit_roundtrip() {
        let dir = std::env::temp_dir().join(format!("theta-tool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let w = WriteTool;
        let r = w.run(&json!({"path": "nested/x.txt", "content": "hello world"}), &dir).await;
        assert!(r.ok, "{r:?}");
        let g = ReadTool;
        let got = g.run(&json!({"path": "nested/x.txt"}), &dir).await;
        assert_eq!(got.output, "hello world");

        let e = EditTool;
        let ed = e.run(&json!({"path": "nested/x.txt", "old": "world", "new": "there"}), &dir).await;
        assert!(ed.ok, "{ed:?}");
        assert_eq!(g.run(&json!({"path": "nested/x.txt"}), &dir).await.output, "hello there");

        assert!(!e.run(&json!({"path": "nested/x.txt", "old": "zzz", "new": "y"}), &dir).await.ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The bash tool shells out to `bash`, which does not exist on Windows
    // unless Git for Windows or WSL is installed. The tool itself is fine —
    // there is simply nothing to run here.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_runs_in_cwd_and_caps() {
        let dir = std::env::temp_dir();
        let out = BashTool.run(&json!({"command": "echo hi"}), &dir).await;
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("hi"));
        let bad = BashTool.run(&json!({"command": "exit 3"}), &dir).await;
        assert!(!bad.ok);
    }

    #[tokio::test]
    async fn multiedit_applies_in_order() {
        let dir = std::env::temp_dir().join(format!("theta-me-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = WriteTool;
        w.run(&json!({"path": "a.txt", "content": "one two three"}), &dir).await;
        let r = MultiEditTool
            .run(
                &json!({"edits": [
                    {"path": "a.txt", "old": "one", "new": "1"},
                    {"path": "a.txt", "old": "three", "new": "3"}
                ]}),
                &dir,
            )
            .await;
        assert!(r.ok, "{r:?}");
        assert_eq!(ReadTool.run(&json!({"path": "a.txt"}), &dir).await.output, "1 two 3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn task_tool_runs_a_subagent_and_returns_its_report() {
        use crate::ai::{AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent};
        struct Reply(String);
        impl Provider for Reply {
            fn id(&self) -> &'static str {
                "reply"
            }
            fn stream<'a>(
                &'a self,
                _r: ChatRequest,
                on: &'a mut (dyn FnMut(ProviderEvent) + Send),
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<AssistantTurn, crate::providers::ProviderError>> + Send + 'a>,
            > {
                Box::pin(async move {
                    on(ProviderEvent::TextDelta(self.0.clone()));
                    Ok(AssistantTurn {
                        text: self.0.clone(),
                        tool_calls: vec![],
                        finish: Some(FinishReason::Stop),
                    })
                })
            }
        }
        let builder = Arc::new(|_ty: &str| {
            Ok(crate::agent::AgentLoop::new(Box::new(Reply("SUBAGENT-REPORT".into())), "m"))
        });
        let dir = std::env::temp_dir();
        let out = TaskTool::new(builder)
            .run(&json!({"description": "look", "prompt": "investigate"}), &dir)
            .await;
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("SUBAGENT-REPORT"), "{}", out.output);

        assert!(!TaskTool::new(Arc::new(|_ty: &str| {
            Ok(crate::agent::AgentLoop::new(Box::new(Reply("x".into())), "m"))
        }))
        .run(&json!({"prompt": "   "}), &dir)
        .await
        .ok);
    }

    #[test]
    fn snapshot_paths_finds_mutation_targets() {
        let cwd = Path::new("/tmp/theta-snap");
        assert_eq!(snapshot_paths("write", &json!({"path": "a.txt"}), cwd).len(), 1);
        assert_eq!(snapshot_paths("edit", &json!({"filePath": "b.txt"}), cwd).len(), 1);
        assert_eq!(
            snapshot_paths(
                "multiedit",
                &json!({"edits": [{"path": "a"}, {"path": "b"}]}),
                cwd
            )
            .len(),
            2
        );
        assert!(snapshot_paths("bash", &json!({"command": "rm -rf x"}), cwd).is_empty());
        assert!(snapshot_paths("read", &json!({"path": "a"}), cwd).is_empty());
    }

    #[test]
    fn permission_gates() {
        let cwd = Path::new("/work/proj");
        assert_eq!(AllowAll.check("bash", &json!({}), cwd), PermissionDecision::Allow);
        assert_eq!(ReadOnly.check("read", &json!({}), cwd), PermissionDecision::Allow);
        assert_eq!(ReadOnly.check("bash", &json!({}), cwd), PermissionDecision::Deny);
    }

    #[test]
    fn the_scoped_gate_allows_the_whole_session_folder() {
        // The contract changed: a write inside the session's own folder used to
        // ask. That made several concurrent sessions unusable — every edit
        // interrupted its own pane — so the folder is now pre-approved and only
        // a reach outside it asks.
        let cwd = Path::new("/work/proj");
        for (tool, input) in [
            ("read", json!({"path": "src/main.rs"})),
            ("grep", json!({"path": "src"})),
            ("glob", json!({})),
            ("webfetch", json!({"url": "https://x"})),
            ("ask", json!({"question": "which?"})),
            ("task", json!({"prompt": "go"})),
            // Writes inside the folder: allowed.
            ("write", json!({"path": "a"})),
            ("write", json!({"path": "src/deep/new.rs"})),
            ("edit", json!({"path": "a"})),
            ("multiedit", json!({"edits": [{"path": "a"}, {"path": "b"}]})),
        ] {
            assert_eq!(
                ScopedGate.check(tool, &input, cwd),
                PermissionDecision::Allow,
                "{tool} inside the folder should be pre-approved: {input}"
            );
        }
    }

    #[test]
    fn the_scoped_gate_asks_outside_the_session_folder() {
        let cwd = Path::new("/work/proj");
        for (tool, input) in [
            ("read", json!({"path": "/etc/passwd"})),
            ("write", json!({"path": "/tmp/elsewhere"})),
            ("edit", json!({"path": "../sibling/file"})),
            // A multi-edit with one path outside is not pre-approved, even
            // though the other path is inside.
            (
                "multiedit",
                json!({"edits": [{"path": "inside"}, {"path": "/etc/outside"}]}),
            ),
            // A shell command cannot be judged by its arguments.
            ("bash", json!({"command": "ls"})),
        ] {
            assert_eq!(
                ScopedGate.check(tool, &input, cwd),
                PermissionDecision::Ask,
                "{tool} outside the folder should ask: {input}"
            );
        }
    }

    #[test]
    fn walking_out_with_dot_dot_is_not_pre_approved() {
        // A path that merely *looks* relative must not slip through.
        let cwd = Path::new("/work/proj");
        for p in ["../secret", "sub/../../secret", "./../secret"] {
            assert_eq!(
                ScopedGate.check("write", &json!({"path": p}), cwd),
                PermissionDecision::Ask,
                "{p} escapes the folder and must ask"
            );
        }
    }

    #[test]
    fn the_strict_gate_refuses_instead_of_asking() {
        // Headless: there is nobody to ask, so outside-the-folder is refused
        // rather than silently allowed.
        let cwd = Path::new("/work/proj");
        assert_eq!(
            ScopedStrict.check("write", &json!({"path": "inside"}), cwd),
            PermissionDecision::Allow
        );
        assert_eq!(
            ScopedStrict.check("write", &json!({"path": "/etc/outside"}), cwd),
            PermissionDecision::Deny
        );
    }

    #[test]
    fn specs_are_valid_json_schema_objects() {
        for t in default_tools() {
            let s = t.spec();
            assert!(!s.name.is_empty());
            assert_eq!(s.parameters["type"], "object", "{}", s.name);
        }
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    #[test]
    fn unified_diff_shows_only_the_changed_region() {
        let before = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";
        let after = "fn main() {\n    let x = 42;\n    println!(\"{x}\");\n}\n";
        let d = unified_diff("src/main.rs", before, after).expect("a change");

        // The changed line appears both ways, and only once each.
        assert!(d.contains("-    let x = 1;"), "{d}");
        assert!(d.contains("+    let x = 42;"), "{d}");
        assert_eq!(d.matches("-    let x = 1;").count(), 1, "one removal: {d}");
        assert!(!d.contains("-fn main"), "unchanged lines are context, not removals: {d}");
        // Git-style headers, which the diff viewer keys off.
        assert!(d.starts_with("--- a/src/main.rs"), "{d}");
        assert!(d.contains("+++ b/src/main.rs"), "{d}");
        assert!(d.contains("@@"), "a hunk header is required to render: {d}");
    }

    #[test]
    fn unified_diff_is_none_when_nothing_changed() {
        let text = "same\n";
        assert!(unified_diff("f", text, text).is_none());
        // The write tool relies on this: rewriting a file with identical content
        // must not claim a change.
    }

    #[test]
    fn unified_diff_handles_a_new_file() {
        let d = unified_diff("new.rs", "", "one\ntwo\n").expect("a change");
        assert!(d.contains("+one"), "{d}");
        assert!(d.contains("+two"), "{d}");
        // A hunk header with an empty original side. `-1,0` and `-0,0` are both
        // valid unified-diff spellings, so match the shape, not one form.
        let header = d.lines().find(|l| l.starts_with("@@")).expect("a hunk header");
        assert!(header.contains(",0 "), "no original lines: {header}");
        assert!(header.contains("+1,2"), "two added lines from line 1: {header}");
        // No removal lines, ignoring the `--- a/...` file header.
        let removals = d
            .lines()
            .filter(|l| l.starts_with('-') && !l.starts_with("---"))
            .count();
        assert_eq!(removals, 0, "nothing was removed: {d}");
    }

    #[test]
    fn unified_diff_handles_deletions() {
        let d = unified_diff("f.rs", "a\nb\nc\n", "a\nc\n").expect("a change");
        assert!(d.contains("-b"), "{d}");
        assert!(!d.contains("+b"), "b was not re-added: {d}");
    }

    #[test]
    fn an_edit_tool_outcome_carries_a_usable_diff() {
        // The whole point: the transcript renders `metadata["diff"]`, so the
        // tool has to put it there.
        let out = ToolOutcome::ok("edited src/a.rs")
            .with_diff("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n");
        assert!(out.ok);
        assert_eq!(
            out.metadata.get("diff").and_then(|d| d.as_str()),
            Some("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n")
        );

        // An empty diff is not attached, so a no-op does not render as a change.
        let plain = ToolOutcome::ok("x").with_diff("");
        assert!(plain.metadata.get("diff").is_none());
    }
}

#[cfg(test)]
mod edit_diff_tests {
    use super::*;

    #[tokio::test]
    async fn the_edit_tool_reports_what_it_changed() {
        // End to end through the tool: a real file, a real edit, and a diff on
        // the outcome that the transcript can render.
        let dir = std::env::temp_dir().join(format!("theta-edit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("a.rs");
        std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();

        let input = json!({
            "path": "a.rs",
            "old": "let x = 1;",
            "new": "let x = 42;",
        });
        let out = EditTool.run(&input, &dir).await;
        assert!(out.ok, "{:?}", out.output);

        let diff = out
            .metadata
            .get("diff")
            .and_then(|d| d.as_str())
            .expect("the outcome must carry a diff for the UI");
        assert!(diff.contains("-    let x = 1;"), "{diff}");
        assert!(diff.contains("+    let x = 42;"), "{diff}");
        assert!(diff.contains("@@"), "a hunk header, which the viewer needs: {diff}");

        // And the file really changed.
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(after.contains("let x = 42;"));

        // A failing edit reports an error and no diff.
        let miss = EditTool.run(&json!({"path": "a.rs", "old": "nope", "new": "x"}), &dir).await;
        assert!(!miss.ok);
        assert!(miss.metadata.get("diff").is_none(), "no diff for a failed edit");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_write_tool_reports_a_new_file_as_added_lines() {
        let dir = std::env::temp_dir().join(format!("theta-write-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = WriteTool
            .run(&json!({"path": "new.txt", "content": "one\ntwo\n"}), &dir)
            .await;
        assert!(out.ok, "{:?}", out.output);
        let diff = out.metadata.get("diff").and_then(|d| d.as_str()).expect("a diff");
        assert!(diff.contains("+one"), "{diff}");
        assert!(diff.contains("+two"), "{diff}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
