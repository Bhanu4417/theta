pub mod agents;
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

pub struct AgentLoop {
    provider: Box<dyn Provider>,
    catalog: Catalog,
    tools: Vec<Arc<dyn Tool>>,
    permission: Box<dyn PermissionGate>,
    model: String,
    system_prompt: String,
    reasoning_effort: Option<String>,
    max_turns: usize,
    retry_max_ms: u64,
    compaction: context::CompactionSettings,
    prune: context::PruneSettings,
    compaction_enabled: bool,
    compaction_provider: Option<Box<dyn Provider>>,
    compaction_model: Option<String>,
    broker: Option<std::sync::Arc<permissions::Broker>>,
    question_broker: Option<std::sync::Arc<permissions::QuestionBroker>>,
    max_retries: u32,
    retry_base_ms: u64,
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
            reasoning_effort: None,
            max_turns: 0,
            retry_max_ms: 30_000,
            compaction: context::CompactionSettings::default(),
            prune: context::PruneSettings::default(),
            compaction_enabled: true,
            compaction_provider: None,
            compaction_model: None,
            broker: None,
            question_broker: None,
            max_retries: 0,
            retry_base_ms: 500,
        }
    }

    pub fn with_compaction(mut self, settings: context::CompactionSettings, enabled: bool) -> Self {
        self.compaction = settings;
        self.compaction_enabled = enabled;
        self
    }

    pub fn with_compaction_model(
        mut self,
        provider: Box<dyn Provider>,
        model: impl Into<String>,
    ) -> Self {
        self.compaction_provider = Some(provider);
        self.compaction_model = Some(model.into());
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

    pub fn with_extra_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Cap the backoff between retries. A long outage still retries, but never
    /// waits longer than this.
    pub fn with_retry_cap(mut self, max_ms: u64) -> Self {
        self.retry_max_ms = max_ms;
        self
    }

    /// Configure the rolling window of recent tool output kept in the prompt.
    pub fn with_prune(mut self, settings: context::PruneSettings) -> Self {
        self.prune = settings;
        self
    }

    /// Compaction settings for this model, with the recent-token budget
    /// resolved: an explicit `keep_recent_tokens` wins, otherwise it adapts to
    /// the model's window.
    fn compaction_settings(&self) -> context::CompactionSettings {
        let limit = self.catalog.context_limit(&self.model);
        let mut s = self.compaction;
        s.keep_recent_tokens = context::effective_keep_recent(limit, &self.compaction);
        s
    }

    pub fn with_retry(mut self, max_retries: u32, base_ms: u64) -> Self {
        self.max_retries = max_retries;
        self.retry_base_ms = base_ms;
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn with_broker(mut self, broker: std::sync::Arc<permissions::Broker>) -> Self {
        self.broker = Some(broker);
        self
    }

    pub fn with_question_broker(
        mut self,
        broker: std::sync::Arc<permissions::QuestionBroker>,
    ) -> Self {
        self.question_broker = Some(broker);
        self
    }

    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        let e = effort.into();
        self.reasoning_effort = (!e.trim().is_empty()).then_some(e);
        self
    }

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

    fn cost_for(&self, usage: Option<crate::harness::transcript::TokenUsage>) -> Option<f64> {
        let u = usage?;
        let spec = self.catalog.find(&self.model)?;
        Some(spec.cost(u.input, u.output))
    }

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
        let mut snapshots = Vec::new();
        self.run_turn_journaled_snapshots(history, user_text, &[], cwd, cancel, emit, journal, &mut snapshots)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_turn_journaled_snapshots<F>(
        &self,
        history: &mut Vec<ChatMessage>,
        user_text: &str,
        images: &[crate::ai::ImagePart],
        cwd: &Path,
        cancel: &std::sync::atomic::AtomicBool,
        emit: &mut F,
        journal: &mut Vec<ChatMessage>,
        snapshots: &mut Vec<tools::FileSnapshot>,
    ) -> Result<(), ProviderError>
    where
        F: FnMut(HarnessEvent) + Send,
    {
        if history.is_empty() {
            history.push(ChatMessage::system(self.system_prompt.clone()));
        }
        history.push(ChatMessage::user_with_images(user_text, images.to_vec()));
        context::repair_tool_calls(history);
        emit(HarnessEvent::SessionWorking);
        let run_id = local_run_id();
        let mut failed: std::collections::HashMap<(String, String), usize> =
            std::collections::HashMap::new();
        // Consecutive identical tool calls, successful or not: a real loop must
        // be stopped or the agent "keeps going on the same thing forever".
        let mut last_call_sig: Option<String> = None;
        let mut same_call_repeats: usize = 0;
        let mut last_msg_id = String::new();
        // Set after an empty answer, so the retry of that turn thinks less.
        let mut effort_override: Option<String> = None;
        // Only one automatic retry per turn, so a provider that always returns
        // nothing cannot spin.
        let mut empty_retried = false;

        let turn_limit = if self.max_turns == 0 { usize::MAX } else { self.max_turns };
        for turn in 0..turn_limit {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                emit(HarnessEvent::SessionInterrupted);
                emit(HarnessEvent::SessionIdle);
                return Ok(());
            }
            let limit = self.catalog.context_limit(&self.model);

            // Roll old tool output out of the prompt first: it costs no model
            // call, and it recovers the bulk of the context in a session that
            // has run a lot of commands. Doing it before compaction also means
            // many sessions never need summarizing at all.
            let pruned = context::prune_tool_outputs(history, &self.prune);
            if pruned.did_something() {
                crate::tlog!(
                    "PRUNE dropped {} tokens across {} tool result(s)",
                    pruned.pruned_tokens,
                    pruned.messages
                );
                emit(HarnessEvent::ContextPruned {
                    tokens: pruned.pruned_tokens,
                    messages: pruned.messages,
                });
            }

            // The last recorded usage predates this prune, so it would
            // overstate the prompt about to be sent and could trigger a
            // needless (and expensive) compaction. Recompute from content.
            let context_tokens = if pruned.did_something() {
                context::total_tokens(history)
            } else {
                context::estimate_context_tokens(history)
            };
            let settings = self.compaction_settings();
            if self.compaction_enabled && context::should_compact(context_tokens, limit, &settings)
            {
                if let Some(prep) = context::prepare(history, &settings) {
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
                // Lowered for one retry when the previous attempt reasoned its
                // way through the whole budget and produced no answer.
                reasoning_effort: effort_override
                    .clone()
                    .or_else(|| self.reasoning_effort.clone()),
            };

            let msg_id = format!("local-{run_id}-{turn}");
            last_msg_id = msg_id.clone();
            let mut acc_text = String::new();
            let mut acc_reasoning = String::new();
            let mut reasoning_start: Option<i64> = None;
            let mut turn_usage: Option<crate::harness::transcript::TokenUsage> = None;
            crate::tlog!(
                "REQ provider={} model={} msgs={} tools={}",
                self.provider.id(),
                request.model,
                request.messages.len(),
                request.tools.len()
            );
            let started = std::time::Instant::now();
            // Set when the provider rejects the prompt as too large. The turn
            // loop then compacts and retries rather than failing the turn.
            let mut overflowed = false;
            let turn_result = {
                let mut attempt = 0u32;
                loop {
                    let result = {
                        let mut on_event = |ev: ProviderEvent| match ev {
                            ProviderEvent::TextDelta(t) => {
                                acc_text.push_str(&t);
                                emit(part_text(&msg_id, &acc_text));
                            }
                            ProviderEvent::ReasoningDelta(t) => {
                                if reasoning_start.is_none() {
                                    reasoning_start = Some(now_ms());
                                }
                                acc_reasoning.push_str(&t);
                                emit(part_reasoning_span(
                                    &msg_id,
                                    &acc_reasoning,
                                    reasoning_start,
                                    None,
                                ));
                            }
                            ProviderEvent::Usage { input, output } => {
                                turn_usage = Some(crate::harness::transcript::TokenUsage {
                                    input,
                                    output,
                                    ..Default::default()
                                });
                                emit(part_meta(
                                    &msg_id,
                                    input,
                                    output,
                                    self.cost_for(Some(
                                        crate::harness::transcript::TokenUsage {
                                            input,
                                            output,
                                            ..Default::default()
                                        },
                                    )),
                                ));
                            }
                            ProviderEvent::Done(_) | ProviderEvent::ToolCall(_) => {}
                        };
                        self.provider.stream(request.clone(), &mut on_event).await
                    };
                    match result {
                        Ok(turn) => {
                            crate::tlog!(
                                "{}",
                                resp_log(
                                    "",
                                    &request.model,
                                    started.elapsed().as_millis(),
                                    turn.text.chars().count(),
                                    turn.tool_calls.len(),
                                    turn_usage.map(|u| (u.input, u.output)),
                                )
                            );
                            if acc_text.trim().is_empty() && !turn.text.trim().is_empty() {
                                acc_text = turn.text.clone();
                                emit(part_text(&msg_id, &acc_text));
                            }
                            if !acc_reasoning.is_empty() {
                                emit(part_reasoning_span(
                                    &msg_id,
                                    &acc_reasoning,
                                    reasoning_start,
                                    Some(now_ms()),
                                ));
                            }
                            break Some(turn);
                        }
                        Err(e) => {
                            // `max_retries == 0` keeps retrying. A busy
                            // gateway can be unavailable for minutes, and
                            // giving up mid-turn loses the work; the user can
                            // always interrupt.
                            let more_attempts =
                                self.max_retries == 0 || attempt < self.max_retries;
                            if more_attempts && is_retryable(&e) {
                                attempt += 1;
                                // A busy model behind a shared gateway mostly
                                // fails with 500 or 429. Wait as long as the
                                // server asked (Retry-After) or back off
                                // exponentially, capped so a long outage does
                                // not park the session for minutes.
                                let delay = crate::ai::retry_delay(
                                    &e,
                                    attempt,
                                    self.retry_base_ms,
                                    self.retry_max_ms,
                                );
                                crate::tlog!(
                                    "RETRY attempt {}{} in {}ms: {e}",
                                    attempt,
                                    if self.max_retries == 0 {
                                        String::new()
                                    } else {
                                        format!("/{}", self.max_retries)
                                    },
                                    delay.as_millis()
                                );
                                emit(HarnessEvent::SessionRetrying {
                                    attempt,
                                    max_attempts: self.max_retries,
                                    reason: short_reason(&e),
                                    delay_ms: delay.as_millis() as u64,
                                });
                                acc_text.clear();
                                turn_usage = None;
                                // Waiting must be interruptible. A plain sleep
                                // ignores Ctrl+C, so a long backoff looked like
                                // a frozen session — and with unlimited retries
                                // that would be the only way out.
                                if !sleep_cancellable(delay, cancel).await {
                                    crate::tlog!("RETRY cancelled during backoff");
                                    emit(HarnessEvent::SessionInterrupted);
                                    emit(HarnessEvent::SessionIdle);
                                    return Ok(());
                                }
                                continue;
                            }
                            crate::tlog!(
                                "ERR provider={} model={} msgs={} {e}",
                                self.provider.id(),
                                request.model,
                                request.messages.len()
                            );
                            // Retrying verbatim cannot help: the prompt is too
                            // large. The turn loop compacts and tries again.
                            if self.compaction_enabled && crate::ai::is_context_overflow_error(&e) {
                                crate::tlog!("OVERFLOW compacting and retrying");
                                overflowed = true;
                                break None;
                            }
                            return Err(e);
                        }
                    }
                }
            };

            // Reclaim context and retry the same iteration.
            if overflowed {
                emit(HarnessEvent::SessionError(
                    "prompt hit the model's context limit; compacting and retrying".into(),
                ));
                let settings = self.compaction_settings();
                if let Some(prep) = context::prepare(history, &settings) {
                    emit(HarnessEvent::CompactionStarted);
                    if let Some(summary) = self.summarize_prep(&prep).await {
                        context::apply_summary(history, prep.first_kept, &summary);
                        emit(HarnessEvent::CompactionFinished {
                            tokens_before: prep.tokens_before,
                        });
                        emit(part_compaction(prep.tokens_before));
                        continue;
                    }
                }
                // Nothing left to compact: give up rather than loop forever.
                emit(HarnessEvent::SessionError(
                    "context overflow, and nothing further to compact".into(),
                ));
                emit(HarnessEvent::SessionIdle);
                return Ok(());
            }

            let turn_result = match turn_result {
                Some(t) => t,
                None => {
                    emit(HarnessEvent::SessionIdle);
                    return Ok(());
                }
            };

            let mut assistant_msg = ChatMessage::assistant(
                turn_result.text.clone(),
                turn_result.tool_calls.clone(),
            );
            assistant_msg.tokens = turn_usage;
            assistant_msg.cost = self.cost_for(turn_usage);
            history.push(assistant_msg.clone());
            journal.push(assistant_msg);

            if turn_result.tool_calls.is_empty() {
                if turn_result.text.trim().is_empty() {
                    // A reasoning model will happily spend the entire output
                    // budget thinking and emit no answer. Retrying once with
                    // minimal effort is the difference between a usable turn
                    // and an error, so try that before giving up.
                    // An empty answer is retried whatever the finish reason
                    // says. Requiring `Length` was wrong: gateways report an
                    // exhausted reasoning budget as `stop`, or omit the reason
                    // entirely, and then the user got an error for a condition
                    // a retry fixes. The only cost of retrying is one extra
                    // request.
                    if !empty_retried {
                        empty_retried = true;
                        effort_override = Some("minimal".to_string());
                        crate::tlog!(
                            "RETRY empty answer after {turn} turns (finish={:?}); retrying \
                             with minimal effort",
                            turn_result.finish
                        );
                        emit(HarnessEvent::SessionRetrying {
                            attempt: 1,
                            max_attempts: 1,
                            reason: "no answer within the output limit".into(),
                            delay_ms: 0,
                        });
                        continue;
                    }
                    let why = match turn_result.finish.as_ref() {
                        Some(FinishReason::Length) => {
                            "the model hit its output limit before replying"
                        }
                        _ => "the model returned an empty response",
                    };
                    emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                        id: format!("{msg_id}-empty"),
                        message_id: msg_id.to_string(),
                        kind: PartKind::Text {
                            text: format!(
                                "(no output — {why}, and the retry with less thinking was \
                                 also empty. Try `/reasoning minimal`, or another model.)"
                            ),
                            synthetic: false,
                        },
                    })));
                }
                emit(HarnessEvent::AssistantFinished);
                emit(HarnessEvent::SessionIdle);
                return Ok(());
            }

            // Start the read-only calls together before walking the list.
            // A turn that asks for several reads or searches would otherwise
            // wait on each in series; these touch nothing, so running them at
            // once is safe and strictly faster.
            let mut ready: Vec<Option<tools::ToolOutcome>> =
                (0..turn_result.tool_calls.len()).map(|_| None).collect();
            {
                let batch: Vec<(usize, String, Value)> = turn_result
                    .tool_calls
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        if !is_parallel_safe(&c.name) {
                            return false;
                        }
                        // Only when no permission prompt is needed: a prompt
                        // must be asked on the sequential path.
                        let input: Value =
                            serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                        self.permission.check(&c.name, &input, cwd)
                            == PermissionDecision::Allow
                    })
                    .map(|(i, c)| {
                        (
                            i,
                            c.name.clone(),
                            serde_json::from_str(&c.arguments).unwrap_or(json!({})),
                        )
                    })
                    .collect();

                if batch.len() > 1 {
                    let tools = &self.tools;
                    let futs = batch.iter().map(|(i, name, input)| {
                        let i = *i;
                        async move {
                            let outcome = match tools.iter().find(|t| t.spec().name == *name) {
                                Some(t) => t.run(input, cwd).await,
                                None => tools::ToolOutcome::err(format!("unknown tool: {name}")),
                            };
                            (i, outcome)
                        }
                    });
                    for (i, outcome) in futures::future::join_all(futs).await {
                        ready[i] = Some(outcome);
                    }
                }
            }

            for (i, call) in turn_result.tool_calls.iter().enumerate() {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    for pending in &turn_result.tool_calls[i..] {
                        let info = ToolInfo {
                            tool: pending.name.clone(),
                            call_id: pending.id.clone(),
                            status: ToolStatus::Error,
                            title: None,
                            input: serde_json::from_str(&pending.arguments).unwrap_or(json!({})),
                            output: Some("interrupted by user".into()),
                            error: Some("interrupted by user".into()),
                            // No tool ran, so there is nothing to report.
                            metadata: json!({}),
                            start_ms: None,
                        };
                        emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                            id: format!("{msg_id}-call-{}", pending.id),
                            message_id: msg_id.to_string(),
                            kind: PartKind::Tool(info),
                        })));
                        emit(HarnessEvent::ToolFinished {
                            tool: pending.name.clone(),
                            ok: false,
                        });
                        let msg = ChatMessage::tool_result(
                            pending.id.clone(),
                            "tool call was not completed (interrupted)",
                        );
                        history.push(msg.clone());
                        journal.push(msg);
                    }
                    emit(HarnessEvent::SessionInterrupted);
                    emit(HarnessEvent::SessionIdle);
                    return Ok(());
                }
                // Doom-loop guard: the same call with the same arguments over
                // and over (even when it succeeds) means no progress is being
                // made. Stop with a visible notice so the user can redirect.
                let sig = format!("{:?}\u{0}{}", call.name, call.arguments);
                if last_call_sig.as_deref() == Some(sig.as_str()) {
                    same_call_repeats += 1;
                } else {
                    last_call_sig = Some(sig);
                    same_call_repeats = 1;
                }
                if same_call_repeats >= MAX_IDENTICAL_TOOL_CALLS {
                    crate::tlog!(
                        "STOP repetitive loop: {} x{} (turn {})",
                        call.name,
                        same_call_repeats,
                        turn
                    );
                    emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                        id: format!("{msg_id}-loop"),
                        message_id: msg_id.to_string(),
                        kind: PartKind::Text {
                            text: format!(
                                "(stopped: repeated the same `{}` call {}× with identical \
                                 arguments — likely a loop. Send a message to redirect.)",
                                call.name, same_call_repeats
                            ),
                            synthetic: false,
                        },
                    })));
                    emit(HarnessEvent::AssistantFinished);
                    emit(HarnessEvent::SessionIdle);
                    return Ok(());
                }
                let ok = self
                    .run_tool_call(
                        &msg_id,
                        call,
                        cwd,
                        ready[i].take(),
                        history,
                        emit,
                        journal,
                        snapshots,
                    )
                    .await;
                if !ok {
                    let key = (call.name.clone(), call.arguments.clone());
                    let n = failed.entry(key).or_insert(0);
                    *n += 1;
                    if *n >= MAX_REPEATED_TOOL_FAILURES {
                        crate::tlog!(
                            "STOP repeated failing tool {} x{} (turn {})",
                            call.name,
                            *n,
                            turn
                        );
                        emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
                            id: format!("{msg_id}-looped"),
                            message_id: msg_id.to_string(),
                            kind: PartKind::Text {
                                text: format!(
                                    "(stopped: `{}` failed {} times with the same arguments — \
                                     it is unlikely to succeed on a retry.)",
                                    call.name, *n
                                ),
                                synthetic: false,
                            },
                        })));
                        emit(HarnessEvent::AssistantFinished);
                        emit(HarnessEvent::SessionIdle);
                        return Ok(());
                    }
                }
            }
        }

        crate::tlog!(
            "STOP hit the {}-turn limit without finishing (model={})",
            self.max_turns,
            self.model
        );
        let message_id = if last_msg_id.is_empty() {
            format!("local-{run_id}-limit")
        } else {
            last_msg_id
        };
        emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
            id: format!("{message_id}-limit"),
            message_id,
            kind: PartKind::Text {
                text: format!(
                    "(stopped after {} tool rounds without finishing. Send another message to \
                     continue, or raise `[ai] max_turns`.)",
                    self.max_turns
                ),
                synthetic: false,
            },
        })));
        emit(HarnessEvent::SessionIdle);
        Ok(())
    }

    async fn summarize_prep(&self, prep: &context::Preparation) -> Option<String> {
        let cap = self.compaction.tool_result_cap;
        let mut ops = prep.file_ops.clone();
        let raw = if prep.is_split_turn && !prep.turn_prefix.is_empty() {
            let history_fut = async {
                if prep.messages_to_summarize.is_empty() {
                    Some("No prior history.".to_string())
                } else {
                    self.summarize_span(&prep.messages_to_summarize, prep.previous_summary.as_deref())
                        .await
                }
            };
            let prefix_fut = async {
                let conversation = context::serialize_conversation(&prep.turn_prefix, cap);
                let user = format!(
                    "<conversation>\n{conversation}\n</conversation>\n\n{}",
                    context::TURN_PREFIX_SUMMARIZATION_PROMPT
                );
                self.complete(context::SUMMARIZATION_SYSTEM_PROMPT, &user)
                    .await
                    .ok()
            };
            let (history_text, prefix) = tokio::join!(history_fut, prefix_fut);
            let history_text = history_text?;
            let prefix = prefix?;
            format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix}")
        } else {
            self.summarize_span(&prep.messages_to_summarize, prep.previous_summary.as_deref())
                .await?
        };
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

    pub async fn force_compact(
        &self,
        messages: &mut Vec<ChatMessage>,
    ) -> Option<(String, u64, Vec<ChatMessage>)> {
        let settings = self.compaction_settings();
        let prep = context::prepare(messages, &settings)?;
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

    async fn run_ask<F>(
        &self,
        msg_id: &str,
        call: &ToolCall,
        input: &Value,
        emit: &mut F,
    ) -> tools::ToolOutcome
    where
        F: FnMut(HarnessEvent),
    {
        let Some((questions, prompt)) = parse_questions(input, msg_id, &call.id) else {
            return tools::ToolOutcome::err("ask: expected a non-empty `questions` array");
        };
        let Some(broker) = &self.question_broker else {
            return tools::ToolOutcome::err("ask: no interactive session available");
        };
        let rx = broker.register(prompt.id.clone());
        emit(HarnessEvent::QuestionAsked(prompt));
        match rx.await {
            Ok(permissions::QuestionAnswer::Answered(answers)) => {
                let payload = json!({ "answers": answers });
                let mut out = tools::ToolOutcome::ok(payload.to_string());
                out.output = format!(
                    "User answered {} question(s): {}",
                    questions.len(),
                    payload["answers"]
                );
                out
            }
            _ => tools::ToolOutcome::err("ask: the user did not answer"),
        }
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        let provider = self.compaction_provider.as_ref().unwrap_or(&self.provider);
        let model = self.compaction_model.clone().unwrap_or_else(|| self.model.clone());
        let request = ChatRequest {
            model,
            messages: vec![
                ChatMessage::system(system.to_string()),
                ChatMessage::user(user.to_string()),
            ],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(context::summary_max_tokens(self.compaction.reserve_tokens) as u32),
            reasoning_effort: Some("low".into()),
        };
        let mut acc = String::new();
        let mut finish: Option<FinishReason> = None;
        let usage: Option<(u64, u64)>;
        let started = std::time::Instant::now();
        crate::tlog!(
            "REQ (summarize) provider={} model={} msgs={}",
            provider.id(),
            request.model,
            request.messages.len()
        );
        {
            let mut attempt = 0u32;
            loop {
                acc.clear();
                let mut attempt_usage: Option<(u64, u64)> = None;
                let result = {
                    let mut on_event = |ev: ProviderEvent| match ev {
                        ProviderEvent::TextDelta(t) => acc.push_str(&t),
                        ProviderEvent::Done(f) => finish = Some(f),
                        ProviderEvent::Usage { input, output } => {
                            attempt_usage = Some((input, output));
                        }
                        _ => {}
                    };
                    provider.stream(request.clone(), &mut on_event).await
                };
                match result {
                    Ok(turn) => {
                        if !turn.text.is_empty() {
                            acc = turn.text;
                        }
                        if finish.is_none() {
                            finish = turn.finish;
                        }
                        usage = attempt_usage;
                        break;
                    }
                    Err(e) => {
                        if attempt < self.max_retries && is_retryable(&e) {
                            attempt += 1;
                            let delay =
                                self.retry_base_ms.saturating_mul(1u64 << (attempt - 1).min(6));
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                            continue;
                        }
                        crate::tlog!(
                            "ERR (summarize) provider={} model={} msgs={} {e}",
                            provider.id(),
                            request.model,
                            request.messages.len()
                        );
                        return Err(e);
                    }
                }
            }
        }
        crate::tlog!(
            "{}",
            resp_log(
                " (summarize)",
                &request.model,
                started.elapsed().as_millis(),
                acc.chars().count(),
                0,
                usage,
            )
        );
        if acc.trim().is_empty() {
            return Err(ProviderError::Protocol("summarization returned no text".into()));
        }
        if matches!(finish, Some(FinishReason::Length)) {
            crate::tlog!(
                "WARN summarization hit the {} token cap; using truncated summary",
                context::summary_max_tokens(self.compaction.reserve_tokens)
            );
        }
        Ok(acc)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_tool_call<F>(
        &self,
        msg_id: &str,
        call: &ToolCall,
        cwd: &Path,
        precomputed: Option<tools::ToolOutcome>,
        history: &mut Vec<ChatMessage>,
        emit: &mut F,
        journal: &mut Vec<ChatMessage>,
        snapshots: &mut Vec<tools::FileSnapshot>,
    ) -> bool
    where
        F: FnMut(HarnessEvent),
    {
        let input: Value = serde_json::from_str(&call.arguments).unwrap_or(json!({}));
        emit(HarnessEvent::ToolStarted { tool: call.name.clone(), title: call.name.clone() });
        for path in tools::snapshot_paths(&call.name, &input, cwd) {
            let before = tokio::fs::read_to_string(&path).await.ok();
            snapshots.push(tools::FileSnapshot {
                path: path.to_string_lossy().to_string(),
                before,
            });
        }

        let outcome = if let Some(done) = precomputed {
            // Already executed concurrently with its siblings.
            done
        } else if call.name == "ask" {
            self.run_ask(msg_id, call, &input, emit).await
        } else {
            let tool = self.tools.iter().find(|t| t.spec().name == call.name);
            match tool {
            None => tools::ToolOutcome::err(format!("unknown tool: {}", call.name)),
            Some(tool) => {
                let mut decision = self.permission.check(&call.name, &input, cwd);
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
            }
        };

        let status = if outcome.ok { ToolStatus::Completed } else { ToolStatus::Error };
        let info = ToolInfo {
            tool: call.name.clone(),
            call_id: call.id.clone(),
            status,
            title: None,
            input: input.clone(),
            output: Some(outcome.output.clone()),
            error: if outcome.ok { None } else { Some(outcome.output.clone()) },
            metadata: outcome.metadata.clone(),
            start_ms: None,
        };
        emit(HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
            id: format!("{msg_id}-call-{}", call.id),
            message_id: msg_id.to_string(),
            kind: PartKind::Tool(info),
        })));
        emit(HarnessEvent::ToolFinished { tool: call.name.clone(), ok: outcome.ok });

        let ok = outcome.ok;
        let result_msg = ChatMessage::tool_result(
            call.id.clone(),
            if outcome.output.is_empty() { "(no output)".into() } else { outcome.output },
        );
        history.push(result_msg.clone());
        journal.push(result_msg);
        ok
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
    part_reasoning_span(msg_id, text, None, None)
}

fn part_reasoning_span(
    msg_id: &str,
    text: &str,
    start: Option<i64>,
    end: Option<i64>,
) -> HarnessEvent {
    HarnessEvent::Transcript(TranscriptUpdate::Part(Part {
        id: format!("{msg_id}-reasoning"),
        message_id: msg_id.to_string(),
        kind: PartKind::Reasoning {
            text: text.to_string(),
            running: end.is_none(),
            start,
            end,
        },
    }))
}

fn local_run_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}-{n}", now_ms())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Tools that only observe: no filesystem mutation, so several may run
/// concurrently without racing each other. This is exactly the set a model
/// tends to request in one batch (read three files, grep two patterns).
fn is_parallel_safe(tool: &str) -> bool {
    matches!(tool, "read" | "grep" | "glob" | "webfetch")
}

/// A short, human-readable reason for a retry, for the status line. The full
/// provider body is often a page of JSON and does not belong there.
pub(crate) fn short_reason(e: &ProviderError) -> String {
    match e {
        ProviderError::Status { code, .. } => match code {
            429 => "rate limited (429)".to_string(),
            408 => "request timed out (408)".to_string(),
            425 => "too early (425)".to_string(),
            c if (500..=599).contains(c) => format!("provider error ({c})"),
            c => format!("HTTP {c}"),
        },
        ProviderError::Transport(m) => {
            let m = m.to_ascii_lowercase();
            if m.contains("connect") || m.contains("connection") {
                "could not connect".to_string()
            } else if m.contains("timeout") || m.contains("timed out") {
                "network timeout".to_string()
            } else {
                "network error".to_string()
            }
        }
        other => other.to_string(),
    }
}

/// Sleep, returning `false` if the session was cancelled first.
///
/// A plain `tokio::time::sleep` ignores an interrupt, so a 30-second backoff
/// made the pane look frozen and Ctrl+C did nothing. Checks every 100ms so an
/// interrupt is felt immediately without busy-waiting.
async fn sleep_cancellable(
    total: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
) -> bool {
    let step = std::time::Duration::from_millis(100);
    let deadline = std::time::Instant::now() + total;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return true;
        }
        tokio::time::sleep(step.min(deadline - now)).await;
    }
}

const MAX_REPEATED_TOOL_FAILURES: usize = 3;

/// Consecutive identical tool calls (same name and arguments) before the loop
/// is stopped as a repetitive no-progress loop.
const MAX_IDENTICAL_TOOL_CALLS: usize = 4;

pub(crate) fn resp_log(
    label: &str,
    model: &str,
    ms: u128,
    chars: usize,
    tool_calls: usize,
    tokens: Option<(u64, u64)>,
) -> String {
    format!(
        "RESP{label} model={model} ms={ms} chars={chars} tool_calls={tool_calls} tokens={}",
        tokens
            .map(|(input, output)| format!("{input}/{output}"))
            .unwrap_or_else(|| "-".to_string())
    )
}

fn is_retryable(e: &ProviderError) -> bool {
    crate::ai::is_retryable_provider_error(e)
}

fn truncate_detail(args: &str) -> String {
    args.chars().take(200).collect()
}

fn parse_questions(
    input: &Value,
    msg_id: &str,
    call_id: &str,
) -> Option<(Vec<crate::harness::Question>, crate::harness::QuestionPrompt)> {
    use crate::harness::{Question, QuestionChoice, QuestionPrompt};
    let arr = input.get("questions")?.as_array()?;
    let mut questions = Vec::new();
    for q in arr {
        let question = q.get("question").and_then(|v| v.as_str()).unwrap_or("").trim();
        if question.is_empty() {
            continue;
        }
        let header: String = q
            .get("header")
            .and_then(|v| v.as_str())
            .unwrap_or(question)
            .chars()
            .take(40)
            .collect();
        let options = q
            .get("options")
            .and_then(|v| v.as_array())
            .map(|opts| {
                opts.iter()
                    .map(|o| QuestionChoice {
                        label: o.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        description: o
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        questions.push(Question {
            question: question.to_string(),
            header,
            options,
            multiple: q.get("multiple").and_then(|v| v.as_bool()).unwrap_or(false),
            custom: q.get("custom").and_then(|v| v.as_bool()).unwrap_or(true),
        });
    }
    if questions.is_empty() {
        return None;
    }
    Some((
        questions.clone(),
        QuestionPrompt { id: format!("{msg_id}-ask-{call_id}"), questions },
    ))
}

fn part_meta(msg_id: &str, input: u64, output: u64, cost: Option<f64>) -> HarnessEvent {
    HarnessEvent::Transcript(TranscriptUpdate::MessageMeta(Message {
        id: msg_id.to_string(),
        role: TRole::Assistant,
        error: None,
        completed: None,
        created: None,
        cost,
        tokens: Some(TokenUsage { input, output, ..Default::default() }),
        parts: Vec::new(),
    }))
}

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

pub fn assistant_turn(text: &str, calls: Vec<ToolCall>) -> AssistantTurn {
    AssistantTurn { text: text.to_string(), tool_calls: calls, finish: None }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
