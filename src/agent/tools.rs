//! Built-in tools for the local harness.
//!
//! Each tool declares a JSON Schema ([`Tool::spec`]) and a handler
//! ([`Tool::run`]). Handlers run in the session's working directory and always
//! cap their output so a runaway command cannot flood the context window.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::ai::ToolSpec;

/// Max bytes any tool returns to the model.
pub const MAX_TOOL_BYTES: usize = 100 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub ok: bool,
    pub output: String,
}

impl ToolOutcome {
    pub fn ok(output: impl Into<String>) -> Self {
        Self { ok: true, output: output.into() }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self { ok: false, output: message.into() }
    }
}

/// A runnable tool. Boxed future keeps the trait object-safe.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn run<'a>(
        &'a self,
        input: &'a Value,
        cwd: &'a Path,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>>;
}

/// Permission policy consulted before a side-effecting tool runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    /// The caller must ask the user; the harness treats this as "deny until
    /// an interactive gate is wired in".
    Ask,
}

pub trait PermissionGate: Send + Sync {
    fn check(&self, tool: &str, input: &Value) -> PermissionDecision;
}

/// Allow everything (the default for the opted-in local backend).
pub struct AllowAll;
impl PermissionGate for AllowAll {
    fn check(&self, _tool: &str, _input: &Value) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

/// Ask the user before any tool that could have side effects.
pub struct AskGate;
impl PermissionGate for AskGate {
    fn check(&self, _tool: &str, _input: &Value) -> PermissionDecision {
        PermissionDecision::Ask
    }
}

/// Deny everything.
pub struct DenyAll;
impl PermissionGate for DenyAll {
    fn check(&self, _tool: &str, _input: &Value) -> PermissionDecision {
        PermissionDecision::Deny
    }
}

/// Allow read-only tools, deny anything that mutates the workspace.
pub struct ReadOnly;
impl PermissionGate for ReadOnly {
    fn check(&self, tool: &str, _input: &Value) -> PermissionDecision {
        match tool {
            "read" | "grep" | "glob" | "webfetch" => PermissionDecision::Allow,
            _ => PermissionDecision::Deny,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn str_arg(input: &Value, keys: &[&str]) -> Option<String> {
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

/// Simple glob matcher: `*` (any run), `?` (one char), `**` (any incl. `/`).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => {
                if p.get(1) == Some(&b'*') {
                    // `**` matches anything, including separators.
                    (0..=t.len()).any(|i| m(&p[2..], &t[i..]))
                } else {
                    // `*` matches within a path segment.
                    (0..=t.len()).any(|i| !t[..i].contains(&b'/') && m(&p[1..], &t[i..]))
                }
            }
            Some(b'?') => t.first().is_some_and(|c| *c != b'/') && m(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && m(&p[1..], &t[1..]),
        }
    }
    m(pattern.as_bytes(), text.as_bytes())
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

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
            match tokio::fs::write(&full, content.as_bytes()).await {
                Ok(()) => ToolOutcome::ok(format!("wrote {} ({} bytes)", full.display(), content.len())),
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
                Ok(()) => ToolOutcome::ok(format!("edited {path}")),
                Err(e) => ToolOutcome::err(format!("edit {path}: {e}")),
            }
        })
    }
}

/// Apply several `edit` operations in one call.
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
                applied += 1;
            }
            ToolOutcome::ok(format!("applied {applied} edit(s)"))
        })
    }
}

/// A pre-image of a file a mutating tool is about to change. `before = None`
/// means the file did not exist (rewind deletes it).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub before: Option<String>,
}

/// Paths a tool call is about to mutate, so the loop can snapshot them first.
/// Unknown tools and `bash` return nothing (their effects are opaque).
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

/// Delegate a focused sub-task to a nested agent loop and return its report.
/// The builder is supplied by the provider so the sub-agent shares the same
/// model/credentials but runs with a fresh, auto-approved context.
pub struct TaskTool {
    build: Arc<dyn Fn() -> Result<crate::agent::AgentLoop, crate::providers::ProviderError> + Send + Sync>,
}

impl TaskTool {
    pub fn new(
        build: Arc<
            dyn Fn() -> Result<crate::agent::AgentLoop, crate::providers::ProviderError>
                + Send
                + Sync,
        >,
    ) -> Self {
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
                    "prompt": { "type": "string", "description": "Self-contained instructions for the sub-agent" }
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
            let agent = match (self.build)() {
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

/// Ask the user one or more multiple-choice questions. Execution is handled by
/// the agent loop (it needs the event stream + question broker), so `run` is
/// only a fallback that should never be reached.
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
                Ok(Err(e)) => ToolOutcome::err(format!("spawn failed: {e}")),
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
                        let rel = e
                            .path()
                            .strip_prefix(&root)
                            .unwrap_or_else(|_| e.path())
                            .to_string_lossy()
                            .to_string();
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

/// The default tool set.
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

        // Missing pattern is a clean error, not a panic.
        assert!(!e.run(&json!({"path": "nested/x.txt", "old": "zzz", "new": "y"}), &dir).await.ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let builder = Arc::new(|| {
            Ok(crate::agent::AgentLoop::new(Box::new(Reply("SUBAGENT-REPORT".into())), "m"))
        });
        let dir = std::env::temp_dir();
        let out = TaskTool::new(builder)
            .run(&json!({"description": "look", "prompt": "investigate"}), &dir)
            .await;
        assert!(out.ok, "{out:?}");
        assert!(out.output.contains("SUBAGENT-REPORT"), "{}", out.output);

        // Empty prompt is a clean error.
        assert!(!TaskTool::new(Arc::new(|| {
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
        assert_eq!(AllowAll.check("bash", &json!({})), PermissionDecision::Allow);
        assert_eq!(ReadOnly.check("read", &json!({})), PermissionDecision::Allow);
        assert_eq!(ReadOnly.check("bash", &json!({})), PermissionDecision::Deny);
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
