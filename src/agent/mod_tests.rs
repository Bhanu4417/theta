//! Tests for the local agent loop, driven by a scripted provider (no network).

use std::collections::VecDeque;
use std::sync::Mutex;

use super::{assistant_turn, AgentLoop};
use crate::ai::{AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent, ToolCall};
use crate::harness::HarnessEvent;
use crate::providers::ProviderError;

/// Returns pre-scripted turns in order. Emits text/tool-call deltas so the
/// loop's streaming path is exercised, not just the assembled turn.
struct ScriptedProvider {
    turns: Mutex<VecDeque<AssistantTurn>>,
}

impl ScriptedProvider {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        Self { turns: Mutex::new(turns.into()) }
    }
}

impl Provider for ScriptedProvider {
    fn id(&self) -> &'static str {
        "scripted"
    }

    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let turn = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| AssistantTurn {
                    text: String::new(),
                    tool_calls: Vec::new(),
                    finish: Some(FinishReason::Stop),
                });
            if !turn.text.is_empty() {
                on_event(ProviderEvent::TextDelta(turn.text.clone()));
            }
            for call in &turn.tool_calls {
                on_event(ProviderEvent::ToolCall(call.clone()));
            }
            Ok(turn)
        })
    }
}

fn call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall { id: id.into(), name: name.into(), arguments: args.into() }
}

#[tokio::test]
async fn ask_gate_waits_for_broker_then_runs() {
    let dir = std::env::temp_dir().join(format!("theta-perm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![call("c1", "write", r#"{"path":"p.txt","content":"ok"}"#)],
            finish: Some(FinishReason::ToolCalls),
        },
        AssistantTurn { text: "done".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));

    let broker = std::sync::Arc::new(crate::agent::permissions::Broker::new());
    let agent = AgentLoop::new(provider, "test")
        .with_permission(Box::new(crate::agent::tools::AskGate))
        .with_broker(broker.clone());

    let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
    let run_dir = dir.clone();
    let handle = tokio::spawn(async move {
        let mut history = Vec::new();
        let mut emit = move |e: HarnessEvent| {
            let _ = etx.send(e);
        };
        agent.run_turn(&mut history, "go", &run_dir, &mut emit).await
    });

    // The loop must emit PermissionAsked and block until answered.
    let perm_id = loop {
        match erx.recv().await.unwrap() {
            HarnessEvent::PermissionAsked { id, .. } => break id,
            _ => continue,
        }
    };
    assert!(broker.reply(&perm_id, crate::agent::tools::PermissionDecision::Allow));

    let result = handle.await.unwrap();
    assert!(result.is_ok());
    assert_eq!(std::fs::read_to_string(dir.join("p.txt")).unwrap(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ask_gate_denied_blocks_the_tool() {
    let dir = std::env::temp_dir().join(format!("theta-permdeny-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![call("c1", "write", r#"{"path":"no.txt","content":"x"}"#)],
            finish: Some(FinishReason::ToolCalls),
        },
        AssistantTurn { text: "done".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));
    let agent = AgentLoop::new(provider, "test")
        .with_permission(Box::new(crate::agent::tools::DenyAll));

    let mut history = Vec::new();
    let mut emit = |_e: HarnessEvent| {};
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();
    assert!(!dir.join("no.txt").exists(), "denied tool must not write");
    // The denial is reported back to the model as a tool result.
    assert!(history.iter().any(|m| m.text.contains("denied")), "{history:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fails the first `fail_times` attempts with a transient error, then replies.
struct FlakyProvider {
    fail_times: u32,
    attempts: std::sync::Arc<Mutex<u32>>,
    error: fn() -> ProviderError,
}

impl Provider for FlakyProvider {
    fn id(&self) -> &'static str {
        "flaky"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let n = {
                let mut a = self.attempts.lock().unwrap();
                *a += 1;
                *a
            };
            if n <= self.fail_times {
                return Err((self.error)());
            }
            let text = "recovered".to_string();
            on_event(ProviderEvent::TextDelta(text.clone()));
            Ok(AssistantTurn {
                text,
                tool_calls: Vec::new(),
                finish: Some(FinishReason::Stop),
            })
        })
    }
}

#[tokio::test]
async fn retries_transient_provider_errors() {
    let dir = std::env::temp_dir();
    let attempts = std::sync::Arc::new(Mutex::new(0));
    let provider = Box::new(FlakyProvider {
        fail_times: 2,
        attempts: attempts.clone(),
        error: || ProviderError::Transport("connection reset".into()),
    });
    let agent = AgentLoop::new(provider, "m").with_retry(3, 1);
    let mut events = Vec::new();
    let mut history = Vec::new();
    let mut emit = |e: HarnessEvent| events.push(e);
    agent.run_turn(&mut history, "hi", &dir, &mut emit).await.unwrap();
    assert_eq!(*attempts.lock().unwrap(), 3, "two failures then success");
    assert!(events
        .iter()
        .any(|e| matches!(e, HarnessEvent::SessionRetrying(_))));
    assert!(history.iter().any(|m| m.text == "recovered" && m.role == crate::ai::Role::Assistant));
}

#[tokio::test]
async fn does_not_retry_auth_errors() {
    let dir = std::env::temp_dir();
    let attempts = std::sync::Arc::new(Mutex::new(0));
    let provider = Box::new(FlakyProvider {
        fail_times: 99,
        attempts: attempts.clone(),
        error: || ProviderError::Auth("bad key".into()),
    });
    let agent = AgentLoop::new(provider, "m").with_retry(3, 1);
    let mut history = Vec::new();
    let mut emit = |_e: HarnessEvent| {};
    let err = agent.run_turn(&mut history, "hi", &dir, &mut emit).await;
    assert!(err.is_err());
    assert_eq!(*attempts.lock().unwrap(), 1, "auth errors are not retried");
}

#[tokio::test]
async fn ask_tool_round_trips_a_question() {
    let dir = std::env::temp_dir().join(format!("theta-ask-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![call(
                "c1",
                "ask",
                r#"{"questions":[{"question":"Pick one","header":"pick","options":[{"label":"A"},{"label":"B"}]}]}"#,
            )],
            finish: Some(FinishReason::ToolCalls),
        },
        AssistantTurn { text: "thanks".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));
    let broker = std::sync::Arc::new(crate::agent::permissions::QuestionBroker::new());
    let agent = AgentLoop::new(provider, "m").with_question_broker(broker.clone());

    let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel();
    let run_dir = dir.clone();
    let handle = tokio::spawn(async move {
        let mut history = Vec::new();
        let mut emit = move |e: HarnessEvent| {
            let _ = etx.send(e);
        };
        let result = agent.run_turn(&mut history, "go", &run_dir, &mut emit).await;
        (result, history)
    });

    let qid = loop {
        match erx.recv().await.unwrap() {
            HarnessEvent::QuestionAsked(p) => break p.id,
            _ => continue,
        }
    };
    assert!(broker.reply(&qid, vec![vec!["A".to_string()]]));

    let (result, history) = handle.await.unwrap();
    assert!(result.is_ok());
    assert!(
        history.iter().any(|m| m.role == crate::ai::Role::Tool && m.text.contains('A')),
        "the answer reaches the model: {history:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ask_tool_without_a_broker_errors_instead_of_hanging() {
    let dir = std::env::temp_dir();
    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![call("c1", "ask", r#"{"questions":[{"question":"Pick"}]}"#)],
            finish: Some(FinishReason::ToolCalls),
        },
        AssistantTurn { text: "done".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));
    let agent = AgentLoop::new(provider, "m");
    let mut history = Vec::new();
    let mut emit = |_e: HarnessEvent| {};
    // No broker: the tool returns an error rather than blocking forever.
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();
    assert!(history.iter().any(|m| m.text.contains("no interactive session")));
}

#[tokio::test]
async fn loop_runs_a_tool_then_finishes() {
    let dir = std::env::temp_dir().join(format!("theta-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: "writing".into(),
            tool_calls: vec![call("c1", "write", r#"{"path":"out.txt","content":"hi"}"#)],
            finish: None,
        },
        AssistantTurn { text: "done".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));
    let agent = AgentLoop::new(provider, "gpt-4o");

    let (tx, mut rx_vec) = (std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new())), ());
    let _ = rx_vec;
    let sink = tx.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);

    let mut history = Vec::new();
    agent
        .run_turn(&mut history, "make a file", &dir, &mut emit)
        .await
        .unwrap();

    let events = tx.lock().unwrap().clone();
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::SessionWorking)));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::ToolStarted { tool, .. } if tool == "write")));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::ToolFinished { ok: true, .. })));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::AssistantFinished)));
    assert!(events.last().unwrap() == &HarnessEvent::SessionIdle);

    // The tool actually wrote the file, and the result was fed back.
    assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "hi");
    assert!(history.iter().any(|m| {
        m.role == crate::ai::Role::Tool && m.text.contains("wrote") && m.text.contains("out.txt")
    }));
    assert!(history.iter().any(|m| m.text == "done"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn permission_gate_denies_side_effects() {
    let dir = std::env::temp_dir();
    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![call("c1", "write", r#"{"path":"nope.txt","content":"x"}"#)],
            finish: None,
        },
        AssistantTurn { text: "ok".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));
    let agent = AgentLoop::new(provider, "gpt-4o")
        .with_permission(Box::new(crate::agent::tools::ReadOnly));

    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    let mut history = Vec::new();
    agent.run_turn(&mut history, "write", &dir, &mut emit).await.unwrap();

    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::ToolFinished { ok: false, .. })));
    assert!(!dir.join("nope.txt").exists());
}

#[tokio::test]
async fn loop_stops_at_max_turns_instead_of_spinning() {
    let dir = std::env::temp_dir();
    // A provider that never stops requesting a tool.
    let mut turns = Vec::new();
    for i in 0..40 {
        turns.push(AssistantTurn {
            text: String::new(),
            tool_calls: vec![call(&format!("c{i}"), "read", r#"{"path":"x"}"#)],
            finish: None,
        });
    }
    let provider = Box::new(ScriptedProvider::new(turns));
    let mut agent = AgentLoop::new(provider, "gpt-4o");
    agent.max_turns = 3;

    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    let mut history = Vec::new();
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let events = events.lock().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, HarnessEvent::SessionError(m) if m.contains("without completion"))));
    assert!(events.last().unwrap() == &HarnessEvent::SessionIdle);
}

#[test]
fn helper_turn_shape() {
    let t = assistant_turn("hi", vec![call("1", "read", "{}")]);
    assert_eq!(t.text, "hi");
    assert_eq!(t.tool_calls.len(), 1);
}

/// Emits a fixed summary when asked to summarize, otherwise pops the script.
struct CompactProvider {
    turns: Mutex<VecDeque<AssistantTurn>>,
    summarized: Mutex<usize>,
}

impl Provider for CompactProvider {
    fn id(&self) -> &'static str {
        "compact"
    }
    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let summarizing = request
                .messages
                .first()
                .map(|m| m.text.contains("summarization") || m.text.contains("Summarize"))
                .unwrap_or(false);
            if summarizing {
                *self.summarized.lock().unwrap() += 1;
                let s = "GOAL: finish. PROGRESS: mostly done.".to_string();
                on_event(ProviderEvent::TextDelta(s.clone()));
                return Ok(AssistantTurn { text: s, tool_calls: vec![], finish: Some(FinishReason::Stop) });
            }
            let t = self.turns.lock().unwrap().pop_front().unwrap_or(AssistantTurn {
                text: "final".into(),
                tool_calls: vec![],
                finish: Some(FinishReason::Stop),
            });
            if !t.text.is_empty() {
                on_event(ProviderEvent::TextDelta(t.text.clone()));
            }
            Ok(t)
        })
    }
}

#[tokio::test]
async fn compaction_summarizes_with_the_model_and_rebuilds_the_prompt() {
    use crate::ai::catalog::{Catalog, ModelSpec};
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let dir = std::env::temp_dir();
    let provider = CompactProvider {
        turns: Mutex::new(VecDeque::new()),
        summarized: Mutex::new(0),
    };
    // Small fallback context so the trigger fires; tight keep budget so a span exists.
    let catalog = Catalog::builtin().with_fallback(ModelSpec {
        id: "m".into(),
        provider: "test".into(),
        context_limit: 3_000,
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        tools: true,
    });
    let agent = AgentLoop::new(Box::new(provider), "m")
        .with_catalog(catalog)
        .with_compaction(
            crate::agent::context::CompactionSettings {
                reserve_tokens: 0,
                keep_recent_tokens: 500,
                tool_result_cap: 2_000,
            },
            true,
        );

    // A long conversation (system + 30 pairs of big messages).
    let mut history = vec![crate::ai::ChatMessage::system("sys")];
    for _ in 0..30 {
        history.push(crate::ai::ChatMessage::user("x".repeat(400)));
        history.push(crate::ai::ChatMessage::assistant("ok", vec![]));
    }

    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent
        .run_turn(&mut history, "final question", &dir, &mut emit)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::CompactionStarted)));
    assert!(events.iter().any(|e| matches!(e, HarnessEvent::CompactionFinished { .. })));
    assert!(events.iter().any(|e| matches!(
        e,
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) if matches!(p.kind, PartKind::Compaction { .. })
    )));
    // The prompt was rebuilt with a summary and the system prefix preserved.
    assert_eq!(history[0].text, "sys");
    assert!(history
        .iter()
        .any(|m| m.text.contains("conversation-summary") && m.text.contains("GOAL: finish")));
}

/// Echoes the requested model in the summary so tests can tell which provider
/// handled the summarization request.
struct ModelTagProvider;

impl Provider for ModelTagProvider {
    fn id(&self) -> &'static str {
        "model-tag"
    }
    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let summarizing = request
                .messages
                .first()
                .map(|m| m.text.contains("summarization"))
                .unwrap_or(false);
            let text = if summarizing {
                format!("SUMMARY model={}", request.model)
            } else {
                "final".to_string()
            };
            if summarizing {
                on_event(ProviderEvent::TextDelta(text.clone()));
            }
            Ok(AssistantTurn { text, tool_calls: vec![], finish: Some(FinishReason::Stop) })
        })
    }
}

#[tokio::test]
async fn dedicated_compaction_model_writes_the_summary() {
    use crate::ai::catalog::{Catalog, ModelSpec};
    let dir = std::env::temp_dir();
    let catalog = Catalog::builtin().with_fallback(ModelSpec {
        id: "session-model".into(),
        provider: "test".into(),
        context_limit: 3_000,
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        tools: true,
    });
    let agent = AgentLoop::new(Box::new(ModelTagProvider), "session-model")
        .with_catalog(catalog)
        .with_compaction(
            crate::agent::context::CompactionSettings {
                reserve_tokens: 0,
                keep_recent_tokens: 500,
                tool_result_cap: 2_000,
            },
            true,
        )
        .with_compaction_model(Box::new(ModelTagProvider), "cheap-model");

    let mut history = vec![crate::ai::ChatMessage::system("sys")];
    for _ in 0..30 {
        history.push(crate::ai::ChatMessage::user("x".repeat(400)));
        history.push(crate::ai::ChatMessage::assistant("ok", vec![]));
    }
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent
        .run_turn(&mut history, "final question", &dir, &mut emit)
        .await
        .unwrap();

    let summary = history
        .iter()
        .find(|m| m.text.contains("conversation-summary"))
        .expect("a summary was written");
    assert!(summary.text.contains("cheap-model"), "used the compaction model");
    assert!(!summary.text.contains("session-model"), "did not use the session model");
}

/// Returns a truncated (length-capped) summarization, which must be rejected.
struct LengthFailProvider;
impl Provider for LengthFailProvider {
    fn id(&self) -> &'static str {
        "length-fail"
    }
    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let summarizing = request
                .messages
                .first()
                .map(|m| m.text.contains("summarization"))
                .unwrap_or(false);
            if summarizing {
                on_event(ProviderEvent::Done(FinishReason::Length));
                return Ok(AssistantTurn { text: String::new(), tool_calls: vec![], finish: Some(FinishReason::Length) });
            }
            Ok(AssistantTurn { text: "ok".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) })
        })
    }
}

#[tokio::test]
async fn failed_summarization_keeps_full_history() {
    use crate::ai::catalog::{Catalog, ModelSpec};
    let dir = std::env::temp_dir();
    let catalog = Catalog::builtin().with_fallback(ModelSpec {
        id: "m".into(),
        provider: "t".into(),
        context_limit: 1_000,
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        tools: true,
    });
    let agent = AgentLoop::new(Box::new(LengthFailProvider), "m")
        .with_catalog(catalog)
        .with_compaction(
            crate::agent::context::CompactionSettings { reserve_tokens: 0, keep_recent_tokens: 200, tool_result_cap: 2_000 },
            true,
        );
    let mut history = vec![crate::ai::ChatMessage::system("sys")];
    for _ in 0..20 {
        history.push(crate::ai::ChatMessage::user(&"x".repeat(400)));
    }
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "q", &dir, &mut emit).await.unwrap();
    let events = events.lock().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, HarnessEvent::SessionError(m) if m.contains("compaction failed"))));
    // No summary was written; the original messages are intact.
    assert!(!history
        .iter()
        .any(|m| crate::agent::context::is_summary(m)));
    assert!(history.iter().filter(|m| m.role == crate::ai::Role::User).count() >= 20);
}

#[tokio::test]
async fn compaction_folds_previous_summary_and_file_ops() {
    use crate::agent::context::{SUMMARY_CLOSE, SUMMARY_OPEN};
    use crate::ai::catalog::{Catalog, ModelSpec};
    use crate::ai::{ChatMessage, ToolCall};

    let dir = std::env::temp_dir();
    let provider = CompactProvider { turns: Mutex::new(VecDeque::new()), summarized: Mutex::new(0) };
    let catalog = Catalog::builtin().with_fallback(ModelSpec {
        id: "m".into(),
        provider: "test".into(),
        context_limit: 1_000,
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        tools: true,
    });
    let agent = AgentLoop::new(Box::new(provider), "m")
        .with_catalog(catalog)
        .with_compaction(
            crate::agent::context::CompactionSettings {
                reserve_tokens: 0,
                keep_recent_tokens: 200,
                tool_result_cap: 2_000,
            },
            true,
        );

    // A previous summary already tracking /old.rs, followed by new work that
    // edits /new.rs. The next compaction must fold both forward.
    let mut history = vec![ChatMessage::system("sys")];
    history.push(ChatMessage::system(format!(
        "{SUMMARY_OPEN}\nOLD GOAL\n<read-files>\n/old.rs\n</read-files>\n<modified-files>\n</modified-files>\n{SUMMARY_CLOSE}"
    )));
    for _ in 0..20 {
        history.push(ChatMessage::user(&"x".repeat(300)));
        history.push(ChatMessage::assistant(
            "editing",
            vec![ToolCall { id: "e1".into(), name: "edit".into(), arguments: "{\"path\":\"/new.rs\"}".into() }],
        ));
        history.push(ChatMessage::tool_result("e1", "ok"));
    }

    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let summary = history
        .iter()
        .find(|m| crate::agent::context::is_summary(m))
        .expect("a summary message");
    assert!(summary.text.contains("/old.rs"), "cumulative read file kept: {}", summary.text);
    assert!(summary.text.contains("/new.rs"), "new modified file tracked: {}", summary.text);
}

#[tokio::test]
async fn branch_summarization_uses_the_model() {
    let provider = CompactProvider { turns: Mutex::new(VecDeque::new()), summarized: Mutex::new(0) };
    let agent = AgentLoop::new(Box::new(provider), "gpt-4o");
    let out = agent
        .summarize_branch("[User]: did work\n[Assistant]: ok")
        .await
        .unwrap();
    assert!(out.contains("GOAL: finish"), "branch summary came from the model: {out}");
}

#[tokio::test]
async fn compaction_disabled_leaves_history_alone() {
    use crate::ai::catalog::{Catalog, ModelSpec};
    let dir = std::env::temp_dir();
    let provider = CompactProvider { turns: Mutex::new(VecDeque::new()), summarized: Mutex::new(0) };
    let catalog = Catalog::builtin().with_fallback(ModelSpec {
        id: "m".into(),
        provider: "test".into(),
        context_limit: 1_000,
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        tools: true,
    });
    let agent = AgentLoop::new(Box::new(provider), "m")
        .with_catalog(catalog)
        .with_compaction(crate::agent::context::CompactionSettings::default(), false);
    let mut history = vec![crate::ai::ChatMessage::system("sys")];
    for _ in 0..20 {
        history.push(crate::ai::ChatMessage::user("y".repeat(400)));
    }
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "q", &dir, &mut emit).await.unwrap();
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, HarnessEvent::CompactionStarted)));
    assert!(!history.iter().any(|m| m.text.contains("conversation-summary")));
}


/// Flips a shared cancel flag while streaming, then returns a tool call — this
/// reproduces interrupting the run just before tools execute.
struct CancelMidStream {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fired: Mutex<bool>,
}

impl Provider for CancelMidStream {
    fn id(&self) -> &'static str {
        "cancel-mid-stream"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut fired = self.fired.lock().unwrap();
            if !*fired {
                *fired = true;
                self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(AssistantTurn {
                text: String::new(),
                tool_calls: vec![call("c1", "bash", r#"{"command":"echo hi"}"#)],
                finish: Some(FinishReason::ToolCalls),
            })
        })
    }
}

#[tokio::test]
async fn interrupt_closes_pending_tool_calls_in_history() {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider = Box::new(CancelMidStream { cancel: cancel.clone(), fired: Mutex::new(false) });
    let agent = AgentLoop::new(provider, "test");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let mut emit = |_e: HarnessEvent| {};
    agent
        .run_turn_cancellable(&mut history, "go", &dir, &cancel, &mut emit)
        .await
        .unwrap();

    // The assistant asked for c1; the interrupt must leave a matching output so
    // the next request is valid for every provider.
    let has_call = history
        .iter()
        .any(|m| m.tool_calls.iter().any(|c| c.id == "c1"));
    let has_output = history
        .iter()
        .any(|m| m.role == crate::ai::Role::Tool && m.tool_call_id.as_deref() == Some("c1"));
    assert!(has_call, "history should keep the assistant tool call");
    assert!(has_output, "interrupt must synthesize a tool output: {history:?}");
}

/// Streams reasoning as multiple deltas, then a text answer.
struct ReasoningProvider;

impl Provider for ReasoningProvider {
    fn id(&self) -> &'static str {
        "reasoning"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            on_event(ProviderEvent::ReasoningDelta("thi".into()));
            on_event(ProviderEvent::ReasoningDelta("nking".into()));
            on_event(ProviderEvent::TextDelta("hi".into()));
            Ok(AssistantTurn {
                text: "hi".into(),
                tool_calls: vec![],
                finish: Some(FinishReason::Stop),
            })
        })
    }
}

#[tokio::test]
async fn reasoning_deltas_accumulate_into_one_stable_part() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let agent = AgentLoop::new(Box::new(ReasoningProvider), "test");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let reasoning: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => match &p.kind {
                PartKind::Reasoning { text, running, start, end } => {
                    Some((text.clone(), *running, *start, *end))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    // Each delta updates the same part with the accumulated text (not the
    // chunk alone, which made the UI flicker).
    assert!(reasoning.iter().any(|(t, ..)| t == "thi"), "{reasoning:?}");
    let (text, running, start, end) = reasoning.last().unwrap();
    assert_eq!(text, "thinking");
    assert!(!*running, "finalized");
    assert!(start.is_some() && end.is_some(), "timer is bracketed");
}

#[tokio::test]
async fn tool_rows_show_what_they_did() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![call("c1", "read", r#"{"filePath":"src/app.rs"}"#)],
            finish: Some(FinishReason::ToolCalls),
        },
        AssistantTurn {
            text: "done".into(),
            tool_calls: vec![],
            finish: Some(FinishReason::Stop),
        },
    ]));
    let agent = AgentLoop::new(provider, "test");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let title = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => match &p.kind {
                PartKind::Tool(t) if t.tool == "read" => Some(t.display_title()),
                _ => None,
            },
            _ => None,
        })
        .expect("a read tool part was emitted");
    assert!(
        title.contains("src/app.rs"),
        "tool row must say what it did, got {title:?}"
    );
}

/// Assembles the answer in `turn.text` without emitting any TextDelta — the
/// loop must still surface it to the transcript (regression: it was invisible
/// live and only appeared after a restart).
struct SilentTextProvider;

impl Provider for SilentTextProvider {
    fn id(&self) -> &'static str {
        "silent-text"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            Ok(AssistantTurn {
                text: "the assembled answer".into(),
                tool_calls: vec![],
                finish: Some(FinishReason::Stop),
            })
        })
    }
}

#[tokio::test]
async fn non_streamed_answer_still_reaches_the_transcript() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let agent = AgentLoop::new(Box::new(SilentTextProvider), "test");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let shown = events.lock().unwrap().iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, synthetic: false } if text == "the assembled answer"
        ),
        _ => false,
    });
    assert!(shown, "assistant text must be emitted even without deltas");
}

#[tokio::test]
async fn local_message_ids_are_unique_across_turns() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let provider = Box::new(ScriptedProvider::new(vec![
        AssistantTurn { text: "first".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
        AssistantTurn { text: "second".into(), tool_calls: vec![], finish: Some(FinishReason::Stop) },
    ]));
    let agent = AgentLoop::new(provider, "test");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let mut emit = move |e: HarnessEvent| {
            if let HarnessEvent::Transcript(TranscriptUpdate::Part(p)) = e {
                if matches!(&p.kind, PartKind::Text { synthetic: false, .. }) {
                    sink.lock().unwrap().push(p.message_id.clone());
                }
            }
        };
        agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();
        ids.push(seen.lock().unwrap().clone());
    }
    assert!(!ids[0].is_empty() && !ids[1].is_empty(), "{ids:?}");
    assert_ne!(
        ids[0][0], ids[1][0],
        "a later turn must not reuse an earlier message id (it would overwrite the reply)"
    );
}
