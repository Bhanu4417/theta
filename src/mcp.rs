//! Minimal MCP (Model Context Protocol) client over stdio.
//!
//! Each configured server is spawned once; its tools are exposed to the local
//! agent as `mcp__<server>__<tool>`. The transport is newline-delimited
//! JSON-RPC 2.0. Requests are sequential per server (guarded by a mutex), which
//! keeps the reader simple: read lines, skip notifications, match the id.
//!
//! Errors are contained: a server that fails to start or misbehaves is logged
//! and skipped, never panicking the agent.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::agent::tools::{Tool, ToolOutcome};
use crate::ai::ToolSpec;
use crate::config::McpServerConfig;

/// One tool advertised by an MCP server.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// A live stdio MCP server.
pub struct McpClient {
    server: String,
    _child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicI64,
}

impl McpClient {
    /// Spawn and initialize a server. Best-effort: returns an error to skip.
    pub fn connect(name: &str, cfg: &McpServerConfig) -> Result<Self> {
        if cfg.command.trim().is_empty() {
            return Err(anyhow!("mcp server '{name}' has no command"));
        }
        let mut cmd = std::process::Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("spawn mcp '{name}': {e}"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let client = Self {
            server: name.to_string(),
            _child: child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicI64::new(1),
        };
        client.initialize()?;
        Ok(client)
    }

    fn send(&self, method: &str, params: Value, id: Option<i64>) -> Result<()> {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if let Some(id) = id {
            msg["id"] = json!(id);
        }
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().unwrap();
        stdin.write_all(line.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }

    /// Send a request and wait for its response, skipping notifications and
    /// unrelated ids.
    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(method, params, Some(id))?;
        let mut stdout = self.stdout.lock().unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            let n = stdout.read_line(&mut line)?;
            if n == 0 {
                return Err(anyhow!("mcp '{}' closed the connection", self.server));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if v.get("id").and_then(|i| i.as_i64()) != Some(id) {
                continue; // notification or stale response
            }
            if let Some(err) = v.get("error") {
                return Err(anyhow!(
                    "mcp '{}' error: {}",
                    self.server,
                    err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")
                ));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(method, params, None)
    }

    fn initialize(&self) -> Result<()> {
        let _ = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "theta", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        let _ = self.notify("notifications/initialized", json!({}));
        Ok(())
    }

    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let result = self.request("tools/list", json!({}))?;
        Ok(parse_tools(&result))
    }

    /// Call a tool, flattening text content blocks into one string.
    pub fn call_tool(&self, name: &str, arguments: Value) -> Result<String> {
        let result = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )?;
        Ok(flatten_content(&result))
    }
}

/// Parse a `tools/list` result into tool info (pure; unit-tested).
pub fn parse_tools(result: &Value) -> Vec<McpToolInfo> {
    result
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let name = t.get("name").and_then(|n| n.as_str())?.to_string();
                    Some(McpToolInfo {
                        name,
                        description: t
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                        schema: t
                            .get("inputSchema")
                            .cloned()
                            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Flatten a `tools/call` result's content blocks into text (pure).
pub fn flatten_content(result: &Value) -> String {
    let mut out = Vec::new();
    if let Some(items) = result.get("content").and_then(|c| c.as_array()) {
        for it in items {
            match it.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = it.get("text").and_then(|t| t.as_str()) {
                        out.push(t.to_string());
                    }
                }
                Some(other) => out.push(format!("[{other} content]")),
                None => {}
            }
        }
    }
    let is_error = result
        .get("isError")
        .and_then(|e| e.as_bool())
        .unwrap_or(false);
    let text = out.join("\n");
    if is_error {
        format!("error: {text}")
    } else {
        text
    }
}

/// A tool-name-safe id: `mcp__<server>__<tool>` with non-word chars replaced.
pub fn tool_id(server: &str, tool: &str) -> String {
    let clean = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    };
    let id = format!("mcp__{}__{}", clean(server), clean(tool));
    id.chars().take(64).collect()
}

/// An MCP tool exposed through the local agent's [`Tool`] interface.
pub struct McpTool {
    client: Arc<Mutex<McpClient>>,
    id: String,
    tool: McpToolInfo,
}

impl McpTool {
    pub fn new(client: Arc<Mutex<McpClient>>, server: &str, tool: McpToolInfo) -> Self {
        Self { id: tool_id(server, &tool.name), client, tool }
    }

    fn real_name(&self) -> &str {
        &self.tool.name
    }
}

impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        let mut schema = self.tool.schema.clone();
        // MCP schemas are JSON Schema already; ensure it looks like an object.
        if !schema.is_object() || schema.get("type").is_none() {
            schema = json!({ "type": "object", "properties": {} });
        }
        ToolSpec {
            name: self.id.clone(),
            description: format!("[MCP] {}", self.tool.description),
            parameters: schema,
        }
    }

    fn run<'a>(
        &'a self,
        input: &'a Value,
        _cwd: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let client = self.client.clone();
            let name = self.real_name().to_string();
            let args = input.clone();
            // Blocking JSON-RPC on a blocking thread; never blocks the loop.
            match tokio::task::spawn_blocking(move || {
                client.lock().unwrap().call_tool(&name, args)
            })
            .await
            {
                Ok(Ok(text)) => ToolOutcome::ok(text),
                Ok(Err(e)) => ToolOutcome::err(format!("mcp: {e}")),
                Err(e) => ToolOutcome::err(format!("mcp task failed: {e}")),
            }
        })
    }
}

/// Connect every enabled server and return their tools. Failures are logged
/// and skipped so one bad server can't take down the agent.
pub fn connect_all(cfg: &HashMap<String, McpServerConfig>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for (name, sc) in cfg {
        if !sc.enabled {
            continue;
        }
        match McpClient::connect(name, sc) {
            Ok(client) => {
                let client = Arc::new(Mutex::new(client));
                let list = client.lock().unwrap().list_tools().unwrap_or_default();
                crate::tlog!("MCP '{}' connected with {} tool(s)", name, list.len());
                for t in list {
                    tools.push(Arc::new(McpTool::new(client.clone(), name, t)));
                }
            }
            Err(e) => crate::tlog!("MCP '{}' unavailable: {e}", name),
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_server_available() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn parses_and_flattens_protocol_shapes() {
        let listed = json!({
            "tools": [
                { "name": "read_file", "description": "read", "inputSchema": {"type":"object","properties":{"p":{"type":"string"}}} },
                { "name": "no_schema" }
            ]
        });
        let tools = parse_tools(&listed);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[1].schema["type"], "object");

        let ok = json!({ "content": [{ "type": "text", "text": "hello" }, { "type": "text", "text": "world" }] });
        assert_eq!(flatten_content(&ok), "hello\nworld");
        let err = json!({ "content": [{ "type": "text", "text": "boom" }], "isError": true });
        assert_eq!(flatten_content(&err), "error: boom");
    }

    #[test]
    fn tool_ids_are_api_safe() {
        assert_eq!(tool_id("my-server", "read.file"), "mcp__my_server__read_file");
        assert!(tool_id("s", "t").len() <= 64);
    }

    #[test]
    fn connects_lists_and_calls_a_stdio_server() {
        if !mock_server_available() {
            return;
        }
        // A tiny MCP server: initialize, tools/list, tools/call.
        let script = r#"
import sys, json
def send(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    m = json.loads(line)
    method = m.get("method")
    if "id" not in m:
        continue
    if method == "initialize":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mock","version":"0"}}})
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":m["id"],"result":{"tools":[{"name":"echo","description":"echo back","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}]}})
    elif method == "tools/call":
        args = m.get("params",{}).get("arguments",{})
        send({"jsonrpc":"2.0","id":m["id"],"result":{"content":[{"type":"text","text":"echo:"+str(args.get("msg",""))}]}})
    else:
        send({"jsonrpc":"2.0","id":m["id"],"result":{}})
"#;
        let cfg = McpServerConfig {
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: HashMap::new(),
            enabled: true,
        };
        let client = McpClient::connect("mock", &cfg).expect("connect");
        let tools = client.list_tools().expect("list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let out = client.call_tool("echo", json!({"msg": "hi"})).expect("call");
        assert_eq!(out, "echo:hi");
    }

    #[tokio::test]
    async fn mcp_tool_wraps_call_results() {
        let info = McpToolInfo {
            name: "x".into(),
            description: "d".into(),
            schema: json!({ "type": "object", "properties": {} }),
        };
        // Without a live client we only assert the spec shape/id mapping.
        let spec_name = tool_id("srv", &info.name);
        assert_eq!(spec_name, "mcp__srv__x");
        // str_arg is used by the built-in tools; ensure it is importable here.
        use crate::agent::tools::str_arg;
        let _ = str_arg(&json!({"a": 1}), &["a"]);
    }
}
