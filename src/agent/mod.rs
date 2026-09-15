//! The local agent loop: provider → tool calls → tool results → repeat.
//!
//! It owns the loop, tools, permission checks and context management, and
//! emits the *same* provider-neutral [`HarnessEvent`]s the OpenCode adapter
//! emits — so the UI renders local turns with no changes.

pub mod context;
pub mod permissions;
pub mod tools;

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::ai::{
    catalog::Catalog, AssistantTurn, ChatMessage, ChatRequest, FinishReason, Provider,
    ProviderEvent, ToolCall,
};
use crate::harness::transcript::{Message, Part, PartKind, Role as TRole, ToolInfo, ToolStatus, TokenUsage};
use crate::harness::transcript::TranscriptUpdate;
use crate::harness::HarnessEvent;
use crate::providers::ProviderError;

use tools::{PermissionDecision, PermissionGate, Tool};

/// A configured local agent: one LLM provider plus tools and policy.
pub struct AgentLoop {
    provider: Box<dyn Provider>,
    catalog: Catalog,
    tools: Vec<Arc<dyn Tool>>,
    permission: Box<dyn PermissionGate>,
    model: String,
    system_prompt: String,
    max_turns: usize,
    compaction: context::CompactionSettings,
    compaction_enabled: bool,
    /// Answers interactive `Ask` permission decisions (local backend).
    broker: Option<std::sync::Arc<permissions::Broker>>,
}

impl AgentLoop {
    pub fn new(provider: Box<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            catalog: Catalog::builtin(),
            tools: tools::default_tools(),
            permission: Box::new(tools::AllowAll),
            model: model.into(),
            system_prompt: default_system_prompt(),
            max_turns: 24,
            compaction: context::CompactionSettings::default(),
            compaction_enabled: true,
            broker: None,
        }
    }

    /// Configure auto-compaction (Pi-style reserve/keep token budgets).
    pub fn with_compaction(mut self, settings: context::CompactionSettings, enabled: bool) -> Self {
        self.compaction = settings;
        self.compaction_enabled = enabled;
        self
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_permission(mut self, gate: Box<dyn PermissionGate>) -> Self {
        self.permission = gate;
        self
    }

    /// Wire the interactive permission broker (local backend).
    pub fn with_broker(mut self, broker: std::sync::Arc<permissions::Broker>) -> Self {
        self.broker = Some(broker);
        self
    }

    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Append a skills/context block to the system prompt.
    pub fn with_system_appendix(mut self, extra: impl Into<String>) -> Self {
        let extra = extra.into();
        if !extra.trim().is_empty() {
            self.system_prompt.push_str("\n\n");
            self.system_prompt.push_str(&extra);
        }
        self
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Run one user turn to completion, mutating `history` in place and
    /// emitting neutral harness events. `history` should contain the system
    /// message; it is created if missing.
    pub async fn run_turn<F>(
        &self,
        history: &mut Vec<ChatMessage>,
        user_text: &str,
        cwd: &Path,
        emit: &mut F,
    ) -> Result<(), ProviderError>
    where
        F: FnMut(HarnessEvent) + Send,
    {
        let never = std::sync::atomic::AtomicBool::new(false);
        self.run_turn_cancellable(history, user_text, cwd, &never, emit).await
    }

    /// Like [`Self::run_turn`], but observes a cancellation flag (set by the
    /// provider's `interrupt`) between turns and before each tool call.
    pub async fn run_turn_cancellable<F>(
        &self,
        history: &mut Vec<ChatMessage>,
        user_text: &str,
        cwd: &Path,
        cancel: &std::sync::atomic::AtomicBool,
        emit: &mut F,
    ) -> Result<(), ProviderError>
    where
        F: FnMut(HarnessEvent) + Send,
    {
        let mut journal = Vec::new();
        self.run_turn_journaled(history, user_text, cwd, cancel, emit, &mut journal)
            .await
    }

    /// Run a turn and record every assistant/tool message appended to
    /// `journal`, so a caller (the session tree) can persist them as nodes.
    pub async fn run_turn_journaled<F>(
        &self,
        history: &mut Vec<ChatMessage>,
        user_text: &str,
        cwd: &Path,
        cancel: &std::sync::atomic::AtomicBool,
        emit: &mut F,
        journal: &mut Vec<ChatMessage>,
    ) -> Result<(), ProviderError>
    where
        F: FnMut(HarnessEvent) + Send,
    {
        if history.is_empty() {
            history.push(ChatMessage::system(self.system_prompt.clone()));
        }
        history.push(ChatMessage::user(user_text));
        emit(HarnessEvent::SessionWorking);

        for turn in 0..self.max_turns {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                emit(HarnessEvent::SessionInterrupted);
                emit(HarnessEvent::SessionIdle);
                return Ok(());
            }
            // Compact the prompt if it is nearing the model's limit, using
            // Pi's algorithm: prepare a cut, summarize the span (with split
            // turns handled separately), fold the previous summary forward,
            // then rebuild the prompt as system + summary + retained messages.
            let limit = self.catalog.context_limit(&self.model);
            let context_tokens = context::estimate_context_tokens(history);
            if self.compaction_enabled && context::should_compact(context_tokens, limit, &self.compaction)
            {
                if let Some(prep) = context::prepare(history, &self.compaction) {
                    emit(HarnessEvent::CompactionStarted);
                    match self.summarize_prep(&prep).await {
                        Some(summary) => {
                            context::apply_summary(history, prep.first_kept, &summary);
                            emit(HarnessEvent::CompactionFinished {
                                tokens_before: prep.tokens_before,
                            });
                            emit(part_compaction(prep.tokens_before));
                        }
                        None => {
                            // Pi aborts the run on a failed compaction; we keep
                            // the full context and surface the error instead of
                            // losing information.
                            emit(HarnessEvent::SessionError(
                                "context compaction failed; keeping full history".into(),
                            ));
                        }
                    }
                }
            }

            let request = ChatRequest {
                model: self.model.clone(),
                messages: history.clone(),
                tools: self.tools.iter().map(|t| t.spec()).collect(),
                temperature: None,
                max_tokens: None,
            };

            let msg_id = format!("local-{turn}");
            let mut acc_text = String::new();
            let mut turn_usage: Option<crate::harness::transcript::TokenUsage> = None;
            let turn_result = {
                let mut on_event = |ev: ProviderEvent| match ev {
                    ProviderEvent::TextDelta(t) => {
                        acc_text.push_str(&t);
                        emit(part_text(&msg_id, &acc_text));
                    }
                    ProviderEvent::ReasoningDelta(t) => {
                        emit(part_reasoning(&msg_id, &t));
                    }
                    ProviderEvent::Usage { input, output } => {
                        turn_usage = Some(crate::harness::transcript::TokenUsage {
                            input,
                            output,
                            ..Default::default()
                        });
                        emit(part_meta(&msg_id, input, output));
                    }
                    ProviderEvent::Done(_) | ProviderEvent::ToolCall(_) => {}
                };
                self.provider.stream(request, &mut on_event).await?
            };

            let mut assistant_msg = ChatMessage::assistant(
                turn_result.text.clone(),
                turn_result.tool_calls.clone(),
            );
            // Record real usage so the next context estimate is accurate.
            assistant_msg.tokens = turn_usage;
            history.push(assistant_msg.clone());
            journal.push(assistant_msg);

            if turn_result.tool_calls.is_empty() {
                emit(HarnessEvent::AssistantFinished);
                emit(HarnessEvent::SessionIdle);
                return Ok(());
            }

            for call in &turn_result.tool_calls {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    emit(HarnessEvent::SessionInterrupted);
                    emit(HarnessEvent::SessionIdle);
                    return Ok(());
                }
                self.run_tool_call(&msg_id, call, cwd, history, emit, journal).await;
            }
        }

        // Safety stop: never loop forever.
        emit(HarnessEvent::SessionError(format!(
            "stopped after {} turns without completion",
            self.max_turns
        )));
        emit(HarnessEvent::SessionIdle);
        Ok(())
    }

    /// Produce the final summary for a prepared compaction, mirroring Pi's
    /// `compact()`: split turns get a history summary and a merged turn-prefix
    /// summary; file operations are cumulative and appended in Pi's format.
    async fn summarize_prep(&self, prep: &context::Preparation) -> Option<String> {
        let cap = self.compaction.tool_result_cap;
        let mut ops = prep.file_ops.clone();
        let raw = if prep.is_split_turn && !prep.turn_prefix.is_empty() {
            let history_text = if prep.messages_to_summarize.is_empty() {
                "No prior history.".to_string()
            } else {
                self.summarize_span(&prep.messages_to_summarize, prep.previous_summary.as_deref())
                    .await?
            };
            let conversation = context::serialize_conversation(&prep.turn_prefix, cap);
            let user = format!(
                "<conversation>\n{conversation}\n</conversation>\n\n{}",
                context::TURN_PREFIX_SUMMARIZATION_PROMPT
            );
            let prefix = self
                .complete(context::SUMMARIZATION_SYSTEM_PROMPT, &user)
                .await
                .ok()?;
            format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix}")
        } else {
            self.summarize_span(&prep.messages_to_summarize, prep.previous_summary.as_deref())
                .await?
        };
        // Keep the model's file lists and merge ours, cumulatively.
        ops.merge(&context::FileOps::from_summary(&raw));
        let prose = context::strip_summary_markers(&raw);
        Some(if ops.is_empty() {
            prose
        } else {
            format!("{prose}{}", ops.format())
        })
    }

    async fn summarize_span(&self, messages: &[ChatMessage], previous: Option<&str>) -> Option<String> {
        let conversation = context::serialize_conversation(messages, self.compaction.tool_result_cap);
        let user = context::summarization_user_message(&conversation, previous, None);
        self.complete(context::SUMMARIZATION_SYSTEM_PROMPT, &user)
            .await
            .ok()
            .filter(|s| !s.trim().is_empty())
    }

    /// Summarize an abandoned branch for tree navigation (Pi's branch summary).
    /// Force a compaction pass regardless of the budget (the `/compact`
    /// command). Returns the summary, the pre-compaction token count, and the
    /// messages retained after the cut so callers can rebuild a tree.
    pub async fn force_compact(
        &self,
        messages: &mut Vec<ChatMessage>,
    ) -> Option<(String, u64, Vec<ChatMessage>)> {
        let prep = context::prepare(messages, &self.compaction)?;
        let tail = messages[prep.first_kept..].to_vec();
        let summary = self.summarize_prep(&prep).await?;
        context::apply_summary(messages, prep.first_kept, &summary);
        Some((summary, prep.tokens_before, tail))
    }

    pub async fn summarize_branch(&self, text: &str) -> Result<String, ProviderError> {
        let user = format!(
            "<conversation>\n{text}\n</conversation>\n\n{}",
            context::BRANCH_SUMMARY_PROMPT
        );
        self.complete(context::SUMMARIZATION_SYSTEM_PROMPT, &user).await
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage::system(system.to_string()),
                ChatMessage::user(user.to_string()),
            ],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(context::summary_max_tokens(self.compaction.reserve_tokens) as u32),
        };
        let mut acc = String::new();
        let mut finish: Option<FinishReason> = None;
        {
            let mut on_event = |ev: ProviderEvent| match ev {
                ProviderEvent::TextDelta(t) => acc.push_str(&t),
                ProviderEvent::Done(f) => finish = Some(f),
                _ => {}
            };
            let turn = self.provider.stream(request, &mut on_event).await?;
            if !turn.text.is_empty() {
                acc = turn.text;
            }
            if finish.is_none() {
                finish = turn.finish;
            }
        }
        // A truncated summary must not become a checkpoint (Pi's
        // getSummarizationFailure).
        if matches!(finish, Some(FinishReason::Length)) {
            return Err(ProviderError::Protocol(
                "summarization hit the token cap and is incomplete".into(),
            ));
        }
        if acc.trim().is_empty() {
            return Err(ProviderError::Protocol("summarization returned no text".into()));
        }
        Ok(acc)
    }

    async fn run_tool_call<F>(
        &self,
        msg_id: &str,
        call: &ToolCall,
        cwd: &Path,
        history: &mut Vec<ChatMessage>,
        emit: &mut F,
        journal: &mut Vec<ChatMessage>,
    ) where
        F: FnMut(HarnessEvent),
    {
        // (permission `Ask` is resolved inside via the broker)
        let input: Value = serde_json::from_str(&call.arguments).unwrap_or(json!({}));
        emit(HarnessEvent::ToolStarted { tool: call.name.clone(), title: call.name.clone() });

        let tool = self.tools.iter().find(|t| t.spec().name == call.name);
        let outcome = match tool {
            None => tools::ToolOutcome::err(format!("unknown tool: {}", call.name)),
            Some(tool) => {
                let mut decision = self.permission.check(&call.name, &input);
                if decision == PermissionDecision::Ask {
                    match &self.broker {
                        Some(broker) => {
                            let id = format!("{msg_id}-perm-{}", call.id);
                            let rx = broker.register(id.clone());
                            emit(HarnessEvent::PermissionAsked {
                                id: id.clone(),
                                kind: call.name.clone(),
                                detail: truncate_detail(&call.arguments),
                            });
                            decision = rx.await.unwrap_or(PermissionDecision::Deny);
                            emit(HarnessEvent::PermissionReplied);
                        }
                        None => decision = PermissionDecision::Deny,
                    }
                }
                match decision {
                    PermissionDecision::Allow => tool.run(&input, cwd).await,
                    PermissionDecision::Deny | PermissionDecision::Ask => {
                        tools::ToolOutcome::err(format!("permission denied for {}", call.name))
                    }
                }
            }
        };

        let status = if outcome.ok { ToolStatus::Completed } else { ToolStatus::Error };
        let info = ToolInfo {
            tool: call.name.clone(),
            call_id: call.id.clone(),
            status,
            title: Some(call.name.clone()),
            input: input.clone(),
            output: Some(outcome.output.clone()),
            error: if outcome.ok { None } else { Some(outcome.output.clone()) },
            metadata: json!({}),
            start_ms: None,
        };
        emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
            id: format!("{msg_id}-call-{}", call.id),
            message_id: msg_id.to_string(),
            kind: PartKind::Tool(info),
        })));
        emit(HarnessEvent::ToolFinished { tool: call.name.clone(), ok: outcome.ok });

        let result_msg = ChatMessage::tool_result(
            call.id.clone(),
            if outcome.output.is_empty() { "(no output)".into() } else { outcome.output },
        );
        history.push(result_msg.clone());
        journal.push(result_msg);
    }
}

fn part_text(msg_id: &str, text: &str) -> HarnessEvent {
    HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
        id: format!("{msg_id}-text"),
        message_id: msg_id.to_string(),
        kind: PartKind::Text { text: text.to_string(), synthetic: false },
    }))
}

fn part_reasoning(msg_id: &str, text: &str) -> HarnessEvent {
    HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
        id: format!("{msg_id}-reasoning"),
        message_id: msg_id.to_string(),
        kind: PartKind::Reasoning { text: text.to_string(), running: true, start: None, end: None },
    }))
}

fn truncate_detail(args: &str) -> String {
    args.chars().take(200).collect()
}

fn part_meta(msg_id: &str, input: u64, output: u64) -> HarnessEvent {
    HarnessEvent::Transcript(TranscriptUpdate::MessageMeta(Message {
        id: msg_id.to_string(),
        role: TRole::Assistant,
        error: None,
        completed: None,
        created: None,
        cost: None,
        tokens: Some(TokenUsage { input, output, ..Default::default() }),
        parts: Vec::new(),
    }))
}

/// A transcript marker for a compaction boundary (rendered as a divider).
pub(crate) fn part_compaction(tokens_before: u64) -> HarnessEvent {
    HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
        id: format!("compaction-{tokens_before}"),
        message_id: "compaction".to_string(),
        kind: PartKind::Compaction { tokens_before },
    }))
}

pub fn default_system_prompt() -> String {
    "You are Theta, a terminal coding agent. Use the available tools to inspect \
     and modify the project. Prefer reading files before editing. Keep answers \
     concise."
        .to_string()
}

/// Helper for tests and callers: the provider-neutral turn shape the loop sees.
pub fn assistant_turn(text: &str, calls: Vec<ToolCall>) -> AssistantTurn {
    AssistantTurn { text: text.to_string(), tool_calls: calls, finish: None }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
