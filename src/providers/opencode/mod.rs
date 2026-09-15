//! OpenCode adapter: the only place that knows OpenCode specifics.
//!
//! It implements the provider-neutral [`AgentProvider`] interface and converts
//! native OpenCode bus events into [`HarnessEvent`]s.

use crate::harness::transcript::{PartKind, Role, ToolStatus, TranscriptUpdate};
use crate::harness::{HarnessEvent, Question, QuestionChoice, QuestionPrompt};
use crate::opencode::{Client, ModelRef};
use crate::providers::{
    AgentProvider, ModelId, ProviderError, ProviderKind, ProviderSession, SessionConfig,
};

pub struct OpenCodeProvider {
    client: Client,
    directory: String,
}

impl OpenCodeProvider {
    pub fn new(client: Client, directory: impl Into<String>) -> Self {
        Self {
            client,
            directory: directory.into(),
        }
    }
}

fn transport(e: anyhow::Error) -> ProviderError {
    let s = e.to_string();
    let l = s.to_lowercase();
    if l.contains("not found") || l.contains("404") {
        ProviderError::SessionNotFound(s)
    } else if l.contains("connect")
        || l.contains("timeout")
        || l.contains("timed out")
        || l.contains("connection")
    {
        ProviderError::Transport(s)
    } else if l.contains("401")
        || l.contains("403")
        || l.contains("auth")
        || l.contains("unauthorized")
    {
        ProviderError::Auth(s)
    } else if l.contains("cannot start") || l.contains("no such file") || l.contains("unavailable")
    {
        ProviderError::Unavailable(s)
    } else {
        ProviderError::Protocol(s)
    }
}

impl AgentProvider for OpenCodeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenCode
    }

    async fn create_session(
        &self,
        config: SessionConfig,
    ) -> Result<ProviderSession, ProviderError> {
        let title = if config.title.trim().is_empty() {
            "session"
        } else {
            config.title.trim()
        };
        let s = self.client.create_session(title).await.map_err(transport)?;
        Ok(ProviderSession {
            provider: ProviderKind::OpenCode,
            id: s.id,
            directory: if s.directory.is_empty() {
                if config.directory.is_empty() {
                    self.directory.clone()
                } else {
                    config.directory
                }
            } else {
                s.directory
            },
        })
    }

    async fn resume_session(&self, provider_id: &str) -> Result<ProviderSession, ProviderError> {
        match self.client.get_session(provider_id).await {
            Ok(s) => Ok(ProviderSession {
                provider: ProviderKind::OpenCode,
                id: s.id,
                directory: if s.directory.is_empty() {
                    self.directory.clone()
                } else {
                    s.directory
                },
            }),
            Err(e) => Err(ProviderError::SessionNotFound(e.to_string())),
        }
    }

    async fn send_message(
        &self,
        session: &ProviderSession,
        text: &str,
        model: Option<ModelId>,
        agent: Option<String>,
    ) -> Result<(), ProviderError> {
        let model_ref = model.map(|m| ModelRef {
            provider_id: m.provider,
            model_id: m.model,
        });
        self.client
            .prompt_async(&session.id, text, model_ref.as_ref(), agent.as_deref())
            .await
            .map_err(transport)
    }

    async fn interrupt(&self, session: &ProviderSession) -> Result<(), ProviderError> {
        self.client.abort(&session.id).await.map_err(transport)
    }

    async fn fork(
        &self,
        session: &ProviderSession,
        at: Option<&str>,
    ) -> Result<ProviderSession, ProviderError> {
        if !self.capabilities().native_fork {
            return Err(ProviderError::Unsupported("fork".into()));
        }
        let s = self.client.fork(&session.id, at).await.map_err(transport)?;
        Ok(ProviderSession {
            provider: ProviderKind::OpenCode,
            id: s.id,
            directory: if s.directory.is_empty() {
                self.directory.clone()
            } else {
                s.directory
            },
        })
    }
}

/// A raw OpenCode bus event. Private to the adapter — nothing outside
/// `providers::opencode` should ever see it.
#[derive(Debug, Clone)]
struct NativeEvent {
    typ: String,
    properties: serde_json::Value,
}

impl NativeEvent {
    fn parse(v: serde_json::Value) -> Option<Self> {
        let typ = v.get("type")?.as_str()?.to_string();
        Some(Self {
            typ,
            properties: v.get("properties").cloned().unwrap_or_default(),
        })
    }

    fn session_id(&self) -> Option<String> {
        self.properties
            .get("sessionID")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    }
}

/// A provider-neutral event plus the native session id it belongs to, used by
/// the manager to route it to the right Theta session.
pub struct RoutedEvent {
    pub session_id: Option<String>,
    pub event: HarnessEvent,
}

/// Parse a raw SSE `data:` payload and translate it into provider-neutral
/// events. Returns an empty vec for unparseable data.
pub fn convert_native(data: &str) -> Vec<RoutedEvent> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return Vec::new();
    };
    let Some(ev) = NativeEvent::parse(v) else {
        return Vec::new();
    };
    let sid = ev.session_id();
    native_events(&ev)
        .into_iter()
        .map(|event| RoutedEvent {
            session_id: sid.clone(),
            event,
        })
        .collect()
}

/// Translate a native event into one or more neutral events. A single native
/// event can carry both a transcript change and a lifecycle change (e.g. a
/// tool part update), so this returns a list.
fn native_events(ev: &NativeEvent) -> Vec<HarnessEvent> {
    match ev.typ.as_str() {
        "session.idle" => vec![HarnessEvent::SessionIdle],
        "session.status" => {
            let typ = ev
                .properties
                .get("status")
                .and_then(|s| s.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("idle");
            vec![match typ {
                "busy" => HarnessEvent::SessionWorking,
                "retry" => HarnessEvent::SessionRetrying(
                    ev.properties
                        .get("status")
                        .and_then(|s| s.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("retrying")
                        .to_string(),
                ),
                _ => HarnessEvent::SessionIdle,
            }]
        }
        "session.error" => {
            let name = ev
                .properties
                .get("error")
                .and_then(|e| e.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            if name.contains("Aborted") {
                vec![HarnessEvent::SessionInterrupted]
            } else {
                let msg = ev
                    .properties
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        ev.properties
                            .get("error")
                            .and_then(|e| e.get("data"))
                            .and_then(|d| d.get("message"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| name.to_string());
                vec![HarnessEvent::SessionError(format!("{name}: {msg}"))]
            }
        }
        "permission.asked" => vec![HarnessEvent::PermissionAsked {
            id: ev
                .properties
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            kind: ev
                .properties
                .get("permission")
                .and_then(|v| v.as_str())
                .unwrap_or("permission")
                .to_string(),
            detail: permission_detail(&ev.properties),
        }],
        "permission.replied" => vec![HarnessEvent::PermissionReplied],
        "question.asked" | "question.v2.asked" => {
            match crate::opencode::parse_question_request(&ev.properties) {
                Some(q) => vec![HarnessEvent::QuestionAsked(question_prompt(&q))],
                None => vec![HarnessEvent::ProviderSpecific {
                    kind: ev.typ.clone(),
                }],
            }
        }
        "question.replied"
        | "question.rejected"
        | "question.v2.replied"
        | "question.v2.rejected" => vec![HarnessEvent::QuestionReplied],
        "message.updated" => {
            let mut out = Vec::new();
            if let Some(msg) = ev
                .properties
                .get("info")
                .and_then(crate::opencode::parse_message)
            {
                out.push(HarnessEvent::Transcript(TranscriptUpdate::MessageMeta(
                    msg.clone(),
                )));
                if msg.role == Role::Assistant {
                    if let Some(err) = &msg.error {
                        out.push(if err.contains("Aborted") {
                            HarnessEvent::SessionInterrupted
                        } else {
                            HarnessEvent::SessionError(err.clone())
                        });
                    } else if msg.completed.is_some() {
                        out.push(HarnessEvent::AssistantFinished);
                    } else {
                        out.push(HarnessEvent::SessionWorking);
                    }
                }
            }
            out
        }
        "message.part.updated" => {
            let Some(part) = ev
                .properties
                .get("part")
                .and_then(crate::opencode::parse_part)
            else {
                return vec![HarnessEvent::ProviderSpecific {
                    kind: ev.typ.clone(),
                }];
            };
            let mut out = vec![HarnessEvent::Transcript(TranscriptUpdate::Part(
                part.clone(),
            ))];
            match &part.kind {
                PartKind::Reasoning { running: true, .. } => {
                    out.push(HarnessEvent::SessionThinking)
                }
                PartKind::Tool(t) => match t.status {
                    ToolStatus::Pending | ToolStatus::Running => {
                        out.push(HarnessEvent::ToolStarted {
                            tool: t.tool.clone(),
                            title: t.display_title(),
                        })
                    }
                    ToolStatus::Completed => out.push(HarnessEvent::ToolFinished {
                        tool: t.tool.clone(),
                        ok: true,
                    }),
                    ToolStatus::Error => out.push(HarnessEvent::ToolFinished {
                        tool: t.tool.clone(),
                        ok: false,
                    }),
                },
                _ => {}
            }
            out
        }
        "message.part.removed" => {
            let message_id = ev
                .properties
                .get("messageID")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let part_id = ev
                .properties
                .get("partID")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            vec![HarnessEvent::Transcript(TranscriptUpdate::PartRemoved {
                message_id,
                part_id,
            })]
        }
        "file.watcher.updated" | "file.edited" => vec![HarnessEvent::FilesChanged],
        "vcs.branch.updated" => vec![HarnessEvent::BranchChanged],
        _ => vec![HarnessEvent::ProviderSpecific {
            kind: ev.typ.clone(),
        }],
    }
}

fn permission_detail(props: &serde_json::Value) -> String {
    if let Some(meta) = props.get("metadata") {
        for key in ["command", "filePath", "file", "path", "url", "description"] {
            if let Some(v) = meta.get(key).and_then(|v| v.as_str()) {
                return v.to_string();
            }
        }
    }
    if let Some(pats) = props.get("patterns").and_then(|p| p.as_array()) {
        return pats
            .iter()
            .take(2)
            .filter_map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    String::new()
}

/// Map a parsed OpenCode question request into the neutral prompt type.
pub fn question_prompt(q: &crate::opencode::QuestionRequest) -> QuestionPrompt {
    QuestionPrompt {
        id: q.id.clone(),
        questions: q
            .questions
            .iter()
            .map(|info| Question {
                question: info.question.clone(),
                header: info.header.clone(),
                options: info
                    .options
                    .iter()
                    .map(|o| QuestionChoice {
                        label: o.label.clone(),
                        description: o.description.clone(),
                    })
                    .collect(),
                multiple: info.multiple,
                custom: info.custom,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::TranscriptUpdate;
    use serde_json::json;

    fn ev(typ: &str, properties: serde_json::Value) -> NativeEvent {
        NativeEvent {
            typ: typ.to_string(),
            properties,
        }
    }

    fn one(typ: &str, properties: serde_json::Value) -> HarnessEvent {
        let v = native_events(&ev(typ, properties));
        assert_eq!(v.len(), 1, "expected exactly one event, got {v:?}");
        v.into_iter().next().unwrap()
    }

    #[test]
    fn session_status_maps() {
        assert_eq!(
            one("session.status", json!({"status": {"type": "busy"}})),
            HarnessEvent::SessionWorking
        );
        assert_eq!(
            one("session.status", json!({"status": {"type": "idle"}})),
            HarnessEvent::SessionIdle
        );
        assert_eq!(
            one(
                "session.status",
                json!({"status": {"type": "retry", "message": "quota"}})
            ),
            HarnessEvent::SessionRetrying("quota".into())
        );
    }

    #[test]
    fn idle_error_and_interrupt() {
        assert_eq!(one("session.idle", json!({})), HarnessEvent::SessionIdle);
        assert_eq!(
            one("session.error", json!({"error": {"name": "AbortedError"}})),
            HarnessEvent::SessionInterrupted
        );
        assert_eq!(
            one(
                "session.error",
                json!({"error": {"name": "ProviderError", "message": "boom"}})
            ),
            HarnessEvent::SessionError("ProviderError: boom".into())
        );
    }

    #[test]
    fn permission_asked_and_replied() {
        assert_eq!(
            one(
                "permission.asked",
                json!({"id": "per_1", "permission": "bash", "metadata": {"command": "rm -rf /tmp/x"}})
            ),
            HarnessEvent::PermissionAsked {
                id: "per_1".into(),
                kind: "bash".into(),
                detail: "rm -rf /tmp/x".into(),
            }
        );
        assert_eq!(
            one("permission.replied", json!({})),
            HarnessEvent::PermissionReplied
        );
    }

    #[test]
    fn question_asked_maps_to_prompt() {
        match one(
            "question.asked",
            json!({
                "id": "que_1",
                "questions": [{
                    "question": "Which?",
                    "header": "Pick",
                    "options": [{"label": "A", "description": "first"}],
                    "multiple": false,
                    "custom": true
                }]
            }),
        ) {
            HarnessEvent::QuestionAsked(p) => {
                assert_eq!(p.id, "que_1");
                assert_eq!(p.questions.len(), 1);
                assert_eq!(p.questions[0].header, "Pick");
                assert_eq!(p.questions[0].options[0].label, "A");
                assert!(p.questions[0].custom);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_parts_emit_transcript_and_lifecycle() {
        let running = native_events(&ev(
            "message.part.updated",
            json!({"part": {"id": "p1", "messageID": "m1", "type": "tool", "tool": "bash",
                "state": {"status": "running", "input": {"command": "cargo test"}, "title": "Running tests"}}}),
        ));
        assert!(running
            .iter()
            .any(|e| matches!(e, HarnessEvent::Transcript(TranscriptUpdate::Part(_)))));
        assert!(running
            .iter()
            .any(|e| matches!(e, HarnessEvent::ToolStarted { .. })));

        let done = native_events(&ev(
            "message.part.updated",
            json!({"part": {"id": "p1", "messageID": "m1", "type": "tool", "tool": "bash",
                "state": {"status": "completed", "input": {}}}}),
        ));
        assert!(done
            .iter()
            .any(|e| matches!(e, HarnessEvent::ToolFinished { ok: true, .. })));

        let reasoning = native_events(&ev(
            "message.part.updated",
            json!({"part": {"id": "p2", "messageID": "m1", "type": "reasoning", "text": "hmm",
                "time": {"start": 1}}}),
        ));
        assert!(reasoning
            .iter()
            .any(|e| matches!(e, HarnessEvent::SessionThinking)));
    }

    #[test]
    fn part_removed_and_files_and_unknown() {
        assert!(matches!(
            one(
                "message.part.removed",
                json!({"messageID": "m1", "partID": "p1"})
            ),
            HarnessEvent::Transcript(TranscriptUpdate::PartRemoved { .. })
        ));
        assert_eq!(one("file.edited", json!({})), HarnessEvent::FilesChanged);
        assert_eq!(
            one("vcs.branch.updated", json!({})),
            HarnessEvent::BranchChanged
        );
        assert_eq!(
            one("mystery.event", json!({})),
            HarnessEvent::ProviderSpecific {
                kind: "mystery.event".into()
            }
        );
    }

    #[test]
    fn assistant_completion_is_transcript_plus_lifecycle() {
        let events = native_events(&ev(
            "message.updated",
            json!({"info": {"id": "m1", "role": "assistant", "time": {"completed": 5}, "cost": 0.1}}),
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            HarnessEvent::Transcript(TranscriptUpdate::MessageMeta(_))
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, HarnessEvent::AssistantFinished)));
    }

    #[test]
    fn convert_native_parses_and_routes() {
        let data = r#"{"type":"session.status","properties":{"sessionID":"ses_x","status":{"type":"busy"}}}"#;
        let routed = convert_native(data);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].session_id.as_deref(), Some("ses_x"));
        assert_eq!(routed[0].event, HarnessEvent::SessionWorking);
        assert!(convert_native("not json").is_empty());
    }

    /// Opt-in end-to-end check against a real OpenCode server. Never runs in
    /// normal `cargo test`, so it cannot cost tokens or break CI.
    #[tokio::test]
    #[ignore = "spawns a real opencode server; run with --ignored"]
    async fn real_opencode_roundtrip() {
        use std::process::Stdio;
        let which = std::process::Command::new("which").arg("opencode").output();
        let Ok(which) = which else {
            eprintln!("skipping: no `which`");
            return;
        };
        let bin = String::from_utf8_lossy(&which.stdout).trim().to_string();
        if bin.is_empty() {
            eprintln!("skipping: opencode not on PATH");
            return;
        }
        let base = "http://127.0.0.1:4599";
        let mut child = match tokio::process::Command::new(&bin)
            .args(["serve", "--port", "4599", "--hostname", "127.0.0.1"])
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping: could not spawn opencode: {e}");
                return;
            }
        };
        let client = Client::new(base.to_string());
        let mut up = false;
        for _ in 0..100 {
            if client.health().await.is_ok() {
                up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if !up {
            let _ = child.kill().await;
            eprintln!("skipping: opencode server did not become healthy");
            return;
        }
        let provider = OpenCodeProvider::new(client, "/tmp".to_string());
        let session = provider
            .create_session(SessionConfig {
                directory: std::env::temp_dir().to_string_lossy().to_string(),
                title: "theta-integration".into(),
            })
            .await
            .expect("create session");
        assert!(session.id.starts_with("ses"), "got {}", session.id);
        let _ = child.kill().await;
    }
}
