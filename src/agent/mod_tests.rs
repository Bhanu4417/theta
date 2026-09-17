use std::collections::VecDeque;
use std::sync::Mutex;

use super::{assistant_turn, AgentLoop};
use crate::ai::{AssistantTurn, ChatRequest, FinishReason, Provider, ProviderEvent, ToolCall};
use crate::harness::HarnessEvent;
use crate::providers::ProviderError;

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
    assert!(history.iter().any(|m| m.text.contains("denied")), "{history:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

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
        .any(|e| matches!(e, HarnessEvent::SessionRetrying { .. })));
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

    let tx = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
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
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let dir = std::env::temp_dir();
    let mut turns = Vec::new();
    for i in 0..40 {
        turns.push(AssistantTurn {
            text: String::new(),
            tool_calls: vec![call(&format!("c{i}"), "read", &format!(r#"{{"path":"x{i}"}}"#))],
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
    assert!(
        !events.iter().any(|e| matches!(e, HarnessEvent::SessionError(_))),
        "the turn limit must not surface as an error"
    );
    let told = events.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, synthetic: false } if text.contains("max_turns")
        ),
        _ => false,
    });
    assert!(told, "the user must be told why it stopped");
    let rounds = events
        .iter()
        .filter(|e| matches!(e, HarnessEvent::ToolStarted { .. }))
        .count();
    assert_eq!(rounds, 3, "stops at the cap, got {rounds}");
    assert!(events.last().unwrap() == &HarnessEvent::SessionIdle);
}

#[test]
fn helper_turn_shape() {
    let t = assistant_turn("hi", vec![call("1", "read", "{}")]);
    assert_eq!(t.text, "hi");
    assert_eq!(t.tool_calls.len(), 1);
}

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
                on_event(ProviderEvent::Usage { input: 1_200, output: 40 });
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
    assert_eq!(history[0].text, "sys");
    assert!(history
        .iter()
        .any(|m| m.text.contains("conversation-summary") && m.text.contains("GOAL: finish")));
}

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
        history.push(crate::ai::ChatMessage::user("x".repeat(400)));
    }
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "q", &dir, &mut emit).await.unwrap();
    let events = events.lock().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, HarnessEvent::SessionError(m) if m.contains("compaction failed"))));
    assert!(!history
        .iter()
        .any(crate::agent::context::is_summary));
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

    let mut history = vec![ChatMessage::system("sys")];
    history.push(ChatMessage::system(format!(
        "{SUMMARY_OPEN}\nOLD GOAL\n<read-files>\n/old.rs\n</read-files>\n<modified-files>\n</modified-files>\n{SUMMARY_CLOSE}"
    )));
    for _ in 0..20 {
        history.push(ChatMessage::user("x".repeat(300)));
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
                tool_calls: vec![call("c1", "read", r#"{"path":"/tmp/x"}"#)],
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

    let has_call = history
        .iter()
        .any(|m| m.tool_calls.iter().any(|c| c.id == "c1"));
    let has_output = history
        .iter()
        .any(|m| m.role == crate::ai::Role::Tool && m.tool_call_id.as_deref() == Some("c1"));
    assert!(has_call, "history should keep the assistant tool call");
    assert!(has_output, "interrupt must synthesize a tool output: {history:?}");
}

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

#[test]
fn resp_log_formats_turns_and_summaries_alike() {
    assert_eq!(
        super::resp_log("", "deepseek-v4.1-flash", 2166, 4, 0, Some((5269, 3))),
        "RESP model=deepseek-v4.1-flash ms=2166 chars=4 tool_calls=0 tokens=5269/3"
    );
    let summary = super::resp_log(" (summarize)", "muse-spark", 4400, 342, 0, Some((1_200, 40)));
    assert!(summary.starts_with("RESP (summarize)"), "{summary}");
    assert!(summary.contains("ms=4400") && summary.contains("tokens=1200/40"), "{summary}");
}

#[test]
fn resp_log_renders_missing_usage_as_a_dash() {
    let line = super::resp_log("", "m", 10, 0, 2, None);
    assert!(line.ends_with("tool_calls=2 tokens=-"), "{line}");
}

struct AlwaysToolProvider;

impl Provider for AlwaysToolProvider {
    fn id(&self) -> &'static str {
        "always-tool"
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
                text: String::new(),
                tool_calls: vec![call("c1", "no_such_tool", r#"{"x":1}"#)],
                finish: Some(FinishReason::ToolCalls),
            })
        })
    }
}

#[tokio::test]
async fn repeated_failing_tool_call_stops_the_loop_early() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let agent = AgentLoop::new(Box::new(AlwaysToolProvider), "test").with_max_turns(500);
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let evs = events.lock().unwrap();
    let stopped = evs.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, synthetic: false }
                if text.contains("failed") && text.contains("same arguments")
        ),
        _ => false,
    });
    assert!(stopped, "a repeated identical failure must stop the loop");
    let rounds = evs
        .iter()
        .filter(|e| matches!(e, HarnessEvent::ToolStarted { .. }))
        .count();
    assert!(rounds < 10, "should stop after a few repeats, got {rounds}");
}

#[tokio::test]
async fn turn_limit_stops_with_a_visible_notice() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    struct VaryingProvider {
        n: Mutex<usize>,
    }
    impl Provider for VaryingProvider {
        fn id(&self) -> &'static str {
            "varying"
        }
        fn stream<'a>(
            &'a self,
            _request: ChatRequest,
            _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
        > {
            Box::pin(async move {
                let mut n = self.n.lock().unwrap();
                *n += 1;
                // A distinct path each round: the turn limit, not a guard,
                // is what must stop this.
                let args = format!(r#"{{"path":"/tmp/{n}"}}"#);
                Ok(AssistantTurn {
                    text: String::new(),
                    tool_calls: vec![call("c1", "read", &args)],
                    finish: Some(FinishReason::ToolCalls),
                })
            })
        }
    }

    let agent = AgentLoop::new(Box::new(VaryingProvider { n: Mutex::new(0) }), "test")
        .with_max_turns(4)
        .with_tools(vec![std::sync::Arc::new(AlwaysOkTool { name: "read" })
            as std::sync::Arc<dyn crate::agent::tools::Tool>]);
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let evs = events.lock().unwrap();
    let noticed = evs.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, synthetic: false } if text.contains("max_turns")
        ),
        _ => false,
    });
    assert!(noticed, "hitting the turn limit must tell the user, not fail silently");
    assert!(
        !evs.iter().any(|e| matches!(e, HarnessEvent::SessionError(_))),
        "the limit is a notice, not an error"
    );
    assert!(evs.iter().any(|e| matches!(e, HarnessEvent::SessionIdle)));
}

#[test]
fn zero_max_turns_means_no_limit() {
    let agent = AgentLoop::new(Box::new(AlwaysToolProvider), "test").with_max_turns(0);
    assert_eq!(agent.max_turns, 0, "0 is the documented 'no limit' value");
}

/// Returns a large, deterministic payload. Used instead of `bash` in tests that
/// need bulk text in the transcript, so they behave identically on every
/// platform (Windows has no `bash`).
struct BulkTool;

impl crate::agent::tools::Tool for BulkTool {
    fn spec(&self) -> crate::ai::ToolSpec {
        crate::ai::ToolSpec {
            name: "read".into(),
            description: "returns bulk content".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }
    fn run<'a>(
        &'a self,
        input: &'a serde_json::Value,
        _cwd: &'a std::path::Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::agent::tools::ToolOutcome> + Send + 'a>,
    > {
        Box::pin(async move {
            let n = input.get("n").and_then(|v| v.as_u64()).unwrap_or(1);
            crate::agent::tools::ToolOutcome::ok(format!("payload-{n} {}", "x".repeat(20_000)))
        })
    }
}

/// Produces large tool output for a while, then answers plainly. Pruning only
/// touches *completed* turns (the in-flight turn is protected), so a test must
/// span at least two prompts.
struct BigOutputProvider {
    n: Mutex<usize>,
}

impl Provider for BigOutputProvider {
    fn id(&self) -> &'static str {
        "big-output"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            // Four big tool calls in the first turn, then plain answers.
            if *n > 4 {
                return Ok(AssistantTurn {
                    text: "done".into(),
                    tool_calls: vec![],
                    finish: Some(FinishReason::Stop),
                });
            }
            // Distinct arguments each round, so the repeated-failure guard
            // never trips and the loop reaches the prune path.
            Ok(AssistantTurn {
                text: String::new(),
                tool_calls: vec![call(&format!("c{n}"), "read", &format!(r#"{{"n":{n}}}"#))],
                finish: Some(FinishReason::ToolCalls),
            })
        })
    }
}

#[tokio::test]
async fn old_tool_output_is_pruned_from_the_prompt() {
    let agent = AgentLoop::new(Box::new(BigOutputProvider { n: Mutex::new(0) }), "test")
        .with_tools(vec![std::sync::Arc::new(BulkTool)
            as std::sync::Arc<dyn crate::agent::tools::Tool>])
        .with_prune(crate::agent::context::PruneSettings {
            enabled: true,
            protect_tokens: 2_000,
            minimum_tokens: 1_000,
        });
    let dir = std::env::temp_dir();
    let mut history = Vec::new();

    // The in-flight turn and the one before it are always protected, so the
    // third prompt is the first that can reclaim the first turn's output.
    let mut prune_event = None;
    for (i, prompt) in ["first", "second", "third"].iter().enumerate() {
        let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
        let sink = events.clone();
        let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
        agent.run_turn(&mut history, prompt, &dir, &mut emit).await.unwrap();
        let found = events.lock().unwrap().iter().find_map(|e| match e {
            HarnessEvent::ContextPruned { tokens, messages } => Some((*tokens, *messages)),
            _ => None,
        });
        if i < 2 {
            assert!(found.is_none(), "turn {i} must not prune (protected recent turns)");
        }
        if found.is_some() {
            prune_event = found;
        }
    }

    let (tokens, messages) = prune_event.expect("a later prompt should reclaim the first turn");
    assert!(tokens > 0 && messages > 0, "{messages} msgs, {tokens} tokens");

    // The prompt actually shrank, which is the whole point.
    let marker = crate::agent::context::PRUNED_MARKER;
    assert!(
        history.iter().filter(|m| m.text.starts_with(marker)).count() > 0,
        "some tool output should be elided"
    );
    // Every tool result still answers its call, so the request stays valid.
    for m in history.iter().filter(|m| m.role == crate::ai::Role::Tool) {
        assert!(m.tool_call_id.is_some(), "tool results keep their call id");
    }
}

#[tokio::test]
async fn pruning_can_be_turned_off() {
    let agent = AgentLoop::new(Box::new(BigOutputProvider { n: Mutex::new(0) }), "test")
        .with_tools(vec![std::sync::Arc::new(BulkTool)
            as std::sync::Arc<dyn crate::agent::tools::Tool>])
        .with_prune(crate::agent::context::PruneSettings {
            enabled: false,
            protect_tokens: 0,
            minimum_tokens: 0,
        });
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    for prompt in ["first", "second", "third"] {
        let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
        let sink = events.clone();
        let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
        agent.run_turn(&mut history, prompt, &dir, &mut emit).await.unwrap();
        assert!(
            !events.lock().unwrap().iter().any(|e| matches!(e, HarnessEvent::ContextPruned { .. })),
            "disabled prune must not emit"
        );
    }
    assert!(
        !history
            .iter()
            .any(|m| m.text.starts_with(crate::agent::context::PRUNED_MARKER)),
        "nothing should be elided when disabled"
    );
}

/// Rejects the first prompt as too large, then succeeds once the context has
/// been compacted — the shape of a real provider's context-limit error.
struct OverflowThenOk {
    calls: Mutex<usize>,
}

impl Provider for OverflowThenOk {
    fn id(&self) -> &'static str {
        "overflow-then-ok"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                return Err(ProviderError::Protocol(
                    "400: this model's maximum context length is 128000 tokens".into(),
                ));
            }
            let text = "recovered".to_string();
            on_event(ProviderEvent::TextDelta(text.clone()));
            Ok(AssistantTurn { text, tool_calls: vec![], finish: Some(FinishReason::Stop) })
        })
    }
}

#[tokio::test]
async fn a_context_overflow_compacts_and_retries_instead_of_failing() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let dir = std::env::temp_dir();
    let agent = AgentLoop::new(Box::new(OverflowThenOk { calls: Mutex::new(0) }), "test")
        .with_compaction(
            crate::agent::context::CompactionSettings {
                reserve_tokens: 0,
                keep_recent_tokens: 100,
                tool_result_cap: 2_000,
            },
            true,
        );

    // Enough history that a compaction has something to fold.
    let mut history = vec![crate::ai::ChatMessage::system("sys")];
    for i in 0..8 {
        history.push(crate::ai::ChatMessage::user(format!("q{i} {}", "x".repeat(400))));
        history.push(crate::ai::ChatMessage::assistant("ok", vec![]));
    }

    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    let result = agent.run_turn(&mut history, "go", &dir, &mut emit).await;

    // The turn succeeded rather than surfacing the provider error.
    assert!(result.is_ok(), "overflow must not fail the turn: {result:?}");
    let evs = events.lock().unwrap();
    assert!(
        evs.iter().any(|e| matches!(e, HarnessEvent::CompactionStarted)),
        "the overflow should trigger a compaction"
    );
    let answered = evs.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, .. } if text.contains("recovered")
        ),
        _ => false,
    });
    assert!(answered, "the retry's answer should reach the transcript");
}

#[test]
fn context_overflow_errors_are_recognized() {
    use crate::providers::ProviderError;
    for msg in [
        "400 this model's maximum context length is 128000 tokens",
        "prompt is too long: 250000 tokens > 200000 maximum",
        "Please reduce the length of the messages",
        "context_length_exceeded",
    ] {
        assert!(
            crate::ai::is_context_overflow_error(&ProviderError::Protocol(msg.into())),
            "should detect: {msg}"
        );
    }
    // Ordinary failures are not mistaken for overflow.
    for msg in ["429 rate limited", "invalid api key", "500 internal error"] {
        assert!(
            !crate::ai::is_context_overflow_error(&ProviderError::Protocol(msg.into())),
            "must not treat as overflow: {msg}"
        );
    }
}

/// Requests several read-only tools at once, so the concurrent batch runs.
struct ParallelReadProvider {
    n: Mutex<usize>,
}

impl Provider for ParallelReadProvider {
    fn id(&self) -> &'static str {
        "parallel-read"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            if *n > 1 {
                return Ok(AssistantTurn {
                    text: "done".into(),
                    tool_calls: vec![],
                    finish: Some(FinishReason::Stop),
                });
            }
            // Four reads in one turn: exactly the batch that benefits.
            let calls = (0..4)
                .map(|i| {
                    call(
                        &format!("c{i}"),
                        "read",
                        &format!(r#"{{"path":"/nonexistent/{i}.rs"}}"#),
                    )
                })
                .collect();
            Ok(AssistantTurn {
                text: String::new(),
                tool_calls: calls,
                finish: Some(FinishReason::ToolCalls),
            })
        })
    }
}

#[tokio::test]
async fn read_only_tools_in_one_turn_all_run_and_keep_their_order() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let agent = AgentLoop::new(Box::new(ParallelReadProvider { n: Mutex::new(0) }), "test");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    // Every call produced a result, in the order the model asked for them.
    let results: Vec<String> = history
        .iter()
        .filter(|m| m.role == crate::ai::Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    assert_eq!(results, vec!["c0", "c1", "c2", "c3"], "order must be preserved");

    // And each one was reported to the UI.
    let evs = events.lock().unwrap();
    let tool_parts = evs
        .iter()
        .filter(|e| match e {
            HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => {
                matches!(&p.kind, PartKind::Tool(_))
            }
            _ => false,
        })
        .count();
    assert_eq!(tool_parts, 4, "every tool must be reported");
}

#[test]
fn parallel_safe_is_limited_to_read_only_tools() {
    // Writes must never run concurrently: two edits to one file would race.
    for t in ["read", "grep", "glob", "webfetch"] {
        assert!(super::is_parallel_safe(t), "{t} should be parallel-safe");
    }
    for t in ["write", "edit", "multiedit", "bash", "ask", "task"] {
        assert!(!super::is_parallel_safe(t), "{t} must stay sequential");
    }
}

/// A `read`-named tool that stalls, so the concurrent batch is measurable.
struct SlowReadTool {
    delay: std::time::Duration,
}

impl crate::agent::tools::Tool for SlowReadTool {
    fn spec(&self) -> crate::ai::ToolSpec {
        crate::ai::ToolSpec {
            name: "read".into(),
            description: "slow".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }
    fn run<'a>(
        &'a self,
        input: &'a serde_json::Value,
        _cwd: &'a std::path::Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::agent::tools::ToolOutcome> + Send + 'a>,
    > {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            crate::agent::tools::ToolOutcome::ok(
                input.get("path").and_then(|p| p.as_str()).unwrap_or("?"),
            )
        })
    }
}

struct ThreeReadsProvider {
    n: Mutex<usize>,
}

impl Provider for ThreeReadsProvider {
    fn id(&self) -> &'static str {
        "three-reads"
    }
    fn stream<'a>(
        &'a self,
        _request: ChatRequest,
        _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut n = self.n.lock().unwrap();
            *n += 1;
            if *n > 1 {
                return Ok(AssistantTurn {
                    text: "done".into(),
                    tool_calls: vec![],
                    finish: Some(FinishReason::Stop),
                });
            }
            Ok(AssistantTurn {
                text: String::new(),
                tool_calls: (0..3)
                    .map(|i| call(&format!("c{i}"), "read", &format!(r#"{{"path":"p{i}"}}"#)))
                    .collect(),
                finish: Some(FinishReason::ToolCalls),
            })
        })
    }
}

#[tokio::test]
async fn read_only_tool_calls_run_concurrently_not_in_series() {
    const DELAY_MS: u64 = 200;
    const CALLS: u64 = 3;

    let tool = SlowReadTool { delay: std::time::Duration::from_millis(DELAY_MS) };
    let agent = AgentLoop::new(Box::new(ThreeReadsProvider { n: Mutex::new(0) }), "test")
        .with_tools(vec![
            std::sync::Arc::new(tool) as std::sync::Arc<dyn crate::agent::tools::Tool>
        ]);

    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let mut emit = |_e: HarnessEvent| {};

    let started = std::time::Instant::now();
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();
    let elapsed = started.elapsed();

    // Serial execution would take CALLS x DELAY_MS. Concurrent execution takes
    // about one delay; allow generous slack for CI scheduling.
    let serial = std::time::Duration::from_millis(DELAY_MS * CALLS);
    assert!(
        elapsed < serial.mul_f64(0.75),
        "3 x {DELAY_MS}ms tool calls took {elapsed:?}; expected concurrent (~{DELAY_MS}ms), \
         serial would be {serial:?}"
    );
    // And all results are still present, in order.
    let ids: Vec<String> = history
        .iter()
        .filter(|m| m.role == crate::ai::Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    assert_eq!(ids, vec!["c0", "c1", "c2"]);
}

/// Always succeeds, in-process. Used by tests that need a *successful* tool
/// call on every platform — `bash` does not exist on Windows, so a test driving
/// it there exercises the failure path instead.
struct AlwaysOkTool {
    name: &'static str,
}

impl crate::agent::tools::Tool for AlwaysOkTool {
    fn spec(&self) -> crate::ai::ToolSpec {
        crate::ai::ToolSpec {
            name: self.name.into(),
            description: "always succeeds".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }
    fn run<'a>(
        &'a self,
        _input: &'a serde_json::Value,
        _cwd: &'a std::path::Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::agent::tools::ToolOutcome> + Send + 'a>,
    > {
        Box::pin(async move { crate::agent::tools::ToolOutcome::ok("ok") })
    }
}

/// Always asks for the same *succeeding* tool call — a no-progress loop that
/// the failure-based guard would never catch.
struct SuccessfulLoopProvider;

impl Provider for SuccessfulLoopProvider {
    fn id(&self) -> &'static str {
        "successful-loop"
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
                text: String::new(),
                // Names the in-process tool the test installs, so the call
                // succeeds instead of failing as an unknown tool.
                tool_calls: vec![call("c1", "read", r#"{"path":"/tmp/x"}"#)],
                finish: Some(FinishReason::ToolCalls),
            })
        })
    }
}

#[tokio::test]
async fn repeated_succeeding_tool_call_is_stopped_as_a_loop() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    // An in-process tool, so the repeated call *succeeds* on every platform
    // and the identical-call guard is the one that fires.
    let agent = AgentLoop::new(Box::new(SuccessfulLoopProvider), "test")
        .with_max_turns(0)
        .with_tools(vec![std::sync::Arc::new(AlwaysOkTool { name: "read" })
            as std::sync::Arc<dyn crate::agent::tools::Tool>]);
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let evs = events.lock().unwrap();
    let noticed = evs.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, synthetic: false } if text.contains("likely a loop")
        ),
        _ => false,
    });
    assert!(noticed, "an endless repeat of a succeeding call must stop with a notice");
    let rounds = evs
        .iter()
        .filter(|e| matches!(e, HarnessEvent::ToolStarted { .. }))
        .count();
    assert!(rounds <= 6, "should stop quickly, got {rounds} rounds");
    assert!(evs.iter().any(|e| matches!(e, HarnessEvent::SessionIdle)));
}

/// Forwards to another provider, so a test can hold an `Arc` to inspect it
/// after the turn.
struct ProviderShim(std::sync::Arc<dyn Provider + Send + Sync>);

impl Provider for ProviderShim {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        self.0.stream(request, on_event)
    }
}

/// Returns an empty answer with `finish_reason: length` on the first call —
/// the "reasoning ate the whole budget" failure — then a real answer. It also
/// records the reasoning effort each request carried.
struct EmptyThenAnswer {
    calls: Mutex<usize>,
    efforts: Mutex<Vec<Option<String>>>,
}

impl Provider for EmptyThenAnswer {
    fn id(&self) -> &'static str {
        "empty-then-answer"
    }
    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        _on_event: &'a mut (dyn FnMut(ProviderEvent) + Send),
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AssistantTurn, ProviderError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.efforts.lock().unwrap().push(request.reasoning_effort.clone());
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                return Ok(AssistantTurn {
                    text: String::new(),
                    tool_calls: vec![],
                    finish: Some(FinishReason::Length),
                });
            }
            Ok(AssistantTurn {
                text: "here is the answer".into(),
                tool_calls: vec![],
                finish: Some(FinishReason::Stop),
            })
        })
    }
}

#[tokio::test]
async fn an_empty_answer_is_retried_with_less_thinking() {
    use crate::harness::transcript::PartKind;
    use crate::harness::TranscriptUpdate;

    let provider = std::sync::Arc::new(EmptyThenAnswer {
        calls: Mutex::new(0),
        efforts: Mutex::new(Vec::new()),
    });
    let agent = AgentLoop::new(
        Box::new(ProviderShim(provider.clone() as std::sync::Arc<dyn Provider + Send + Sync>)),
        "test",
    )
    .with_reasoning_effort("high");
    let dir = std::env::temp_dir();
    let mut history = Vec::new();
    let events = std::sync::Arc::new(Mutex::new(Vec::<HarnessEvent>::new()));
    let sink = events.clone();
    let mut emit = move |e: HarnessEvent| sink.lock().unwrap().push(e);
    agent.run_turn(&mut history, "go", &dir, &mut emit).await.unwrap();

    let efforts = provider.efforts.lock().unwrap().clone();
    assert_eq!(efforts.len(), 2, "the turn should be retried once");
    assert_eq!(efforts[0].as_deref(), Some("high"), "the first try keeps the setting");
    assert_eq!(
        efforts[1].as_deref(),
        Some("minimal"),
        "the retry must think less, or it will run out of room again"
    );

    // The user gets the answer, not an error about an empty response.
    let evs = events.lock().unwrap();
    let answered = evs.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, .. } if text.contains("here is the answer")
        ),
        _ => false,
    });
    assert!(answered, "the retry's answer reaches the transcript");
    let complained = evs.iter().any(|e| match e {
        HarnessEvent::Transcript(TranscriptUpdate::Part(p)) => matches!(
            &p.kind,
            PartKind::Text { text, .. } if text.contains("no output")
        ),
        _ => false,
    });
    assert!(!complained, "the user must not see the empty-response notice");
}
