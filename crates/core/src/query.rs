use std::sync::Arc;
use std::time::Duration;

use devo_protocol::ModelRequest;
use devo_protocol::ResolvedThinkingRequest;
use devo_protocol::ResponseContent;
use devo_protocol::ResponseExtra;
use devo_protocol::SamplingControls;
use devo_protocol::StopReason;
use devo_protocol::StreamEvent;
use futures::StreamExt;
use tokio::time::sleep;
use tracing::debug;
use tracing::info;
use tracing::info_span;
use tracing::warn;

use devo_provider::ModelProviderSDK;
use devo_tools::ToolCall;
use devo_tools::ToolRegistry;
use devo_tools::ToolRuntime;

use crate::AgentError;
use crate::ContentBlock;
use crate::Message;
use crate::Model;
use crate::Role;
use crate::SessionState;
use crate::TurnConfig;
use crate::context::AgentsMdDiffFragment;
use crate::context::AgentsMdManager;
use crate::context::ContextualUserFragment;
use crate::context::SessionContext;
use crate::context::TurnContext;
use crate::context::load_workspace_instructions;
use crate::context::turn_aborted::TurnAborted;
use crate::history::ContextView;
use crate::history::History;
use crate::history::TokenInfo;
use crate::history::compaction::CompactAction;
use crate::history::compaction::CompactionConfig;
use crate::history::compaction::CompactionKind;
use crate::history::compaction::compact_history;
use crate::history::summarizer::DefaultHistorySummarizer;
use crate::response_item::ResponseItem;
use crate::response_item::message_to_response_items;

fn estimate_request_prompt_tokens(request: &ModelRequest) -> usize {
    let system_bytes = request.system.as_ref().map_or(0, String::len);
    let message_bytes = request
        .messages
        .iter()
        .map(|message| serde_json::to_string(message).map_or(0, |json| json.len()))
        .sum::<usize>();
    let tool_bytes = request
        .tools
        .as_ref()
        .map(|tools| serde_json::to_string(tools).map_or(0, |json| json.len()))
        .unwrap_or(0);
    (system_bytes + message_bytes + tool_bytes).div_ceil(4)
}

/// Events emitted during a query for the caller (CLI/UI) to observe.
#[derive(Debug, Clone)]
pub enum QueryEvent {
    /// Incremental text from the assistant.
    TextDelta(String),
    /// Incremental reasoning text from the assistant.
    ReasoningDelta(String),
    /// Current reasoning block completed.
    ReasoningCompleted,
    /// Incremental token usage update from the provider stream.
    /// TODO: Review the mechanism from the OpenAI API / Anthropic API documentation.
    UsageDelta {
        input_tokens: usize,
        output_tokens: usize,
        cache_creation_input_tokens: Option<usize>,
        cache_read_input_tokens: Option<usize>,
    },
    /// The assistant started a tool call.
    ToolUseStart {
        /// Stable provider-issued tool use identifier.
        id: String,
        /// Tool name selected by the model.
        name: String,
        /// Fully decoded tool input payload, when available.
        input: serde_json::Value,
    },
    /// Incremental output delta from a running tool.
    ToolProgress {
        tool_use_id: String,
        content: String,
    },
    /// A tool call completed.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
        /// Human-readable summary for client-side rendering (e.g. "bash: npm run dev").
        summary: String,
    },
    /// A turn is complete (model stopped generating).
    TurnComplete { stop_reason: StopReason },
    /// Token usage update.
    Usage {
        input_tokens: usize,
        output_tokens: usize,
        cache_creation_input_tokens: Option<usize>,
        cache_read_input_tokens: Option<usize>,
    },
}

/// Callback for streaming query events to the UI layer.
pub type EventCallback = Arc<dyn Fn(QueryEvent) + Send + Sync>;

// ---------------------------------------------------------------------------
// Error classification (capability 3.2)
// ---------------------------------------------------------------------------

enum ErrorClass {
    ContextTooLong,
    ParameterError,
    FileContentAnomaly,
    AuthenticationFailure,
    FeatureUnavailable,
    TaskNotFound,
    RateLimit,
    NoApiPermission,
    FileTooLarge,
    ServerError,
    Unretryable,
}

enum ProviderRetryDecision {
    RetryAfter(Duration),
    CompactAndRetry,
    Fail,
}

fn classify_error(e: &anyhow::Error) -> ErrorClass {
    let msg = e.to_string().to_lowercase();
    // TODO: Expand the error of ContextTooLong
    if msg.contains("context_too_long") {
        ErrorClass::ContextTooLong
    } else if msg.contains("401")
        || msg.contains("authentication failure")
        || msg.contains("token timeout")
        || msg.contains("unauthorized")
        || msg.contains("api key")
    {
        ErrorClass::AuthenticationFailure
    } else if msg.contains("404")
        && (msg.contains("feature not available")
            || msg.contains("fine-tuning feature not available"))
    {
        ErrorClass::FeatureUnavailable
    } else if msg.contains("404")
        && (msg.contains("task does not exist")
            || msg.contains("does not exist")
            || msg.contains("not found"))
    {
        ErrorClass::TaskNotFound
    } else if msg.contains("429") || msg.contains("rate limit") {
        ErrorClass::RateLimit
    } else if msg.contains("434") || msg.contains("no api permission") || msg.contains("beta phase")
    {
        ErrorClass::NoApiPermission
    } else if msg.contains("435")
        || msg.contains("file size exceeds 100mb")
        || msg.contains("smaller than 100mb")
    {
        ErrorClass::FileTooLarge
    } else if msg.contains("400")
        && (msg.contains("file content anomaly")
            || msg.contains("jsonl file content")
            || msg.contains("jsonl"))
    {
        ErrorClass::FileContentAnomaly
    } else if msg.contains("400")
        || msg.contains("parameter error")
        || msg.contains("invalid parameter")
        || msg.contains("bad request")
    {
        ErrorClass::ParameterError
    } else if msg.starts_with('5')
        || msg.contains("500")
        || msg.contains("502")
        || msg.contains("503")
        || msg.contains("504")
        || msg.contains("internal server error")
        || msg.contains("server error occurred while processing the request")
    {
        ErrorClass::ServerError
    } else {
        ErrorClass::Unretryable
    }
}

fn provider_retry_decision(
    error: &anyhow::Error,
    retry_count: &mut usize,
    context_compacted: &mut bool,
) -> ProviderRetryDecision {
    match classify_error(error) {
        ErrorClass::ContextTooLong => {
            if *context_compacted {
                ProviderRetryDecision::Fail
            } else {
                *context_compacted = true;
                ProviderRetryDecision::CompactAndRetry
            }
        }
        ErrorClass::RateLimit | ErrorClass::ServerError => {
            if *retry_count >= MAX_RETRIES {
                ProviderRetryDecision::Fail
            } else {
                *retry_count += 1;
                ProviderRetryDecision::RetryAfter(retry_backoff_duration(*retry_count))
            }
        }
        ErrorClass::ParameterError
        | ErrorClass::FileContentAnomaly
        | ErrorClass::AuthenticationFailure
        | ErrorClass::FeatureUnavailable
        | ErrorClass::TaskNotFound
        | ErrorClass::NoApiPermission
        | ErrorClass::FileTooLarge
        | ErrorClass::Unretryable => ProviderRetryDecision::Fail,
    }
}

// ---------------------------------------------------------------------------
// Session compaction
// ---------------------------------------------------------------------------

/// Compact session messages using LLM-backed summarization.
///
/// Converts session messages to ResponseItems, runs compact_history()
/// with the history module's LLM summarizer, and converts the compacted
/// items back to Messages.
async fn summarize_and_compact(
    session: &mut SessionState,
    provider: &Arc<dyn ModelProviderSDK>,
    model_slug: &str,
    max_tokens: usize,
) {
    let items: Vec<ResponseItem> = session
        .prompt_source_messages()
        .iter()
        .cloned()
        .flat_map(message_to_response_items)
        .collect();

    let token_info = TokenInfo {
        input_tokens: session.total_input_tokens,
        cached_input_tokens: session.total_cache_read_tokens,
        output_tokens: session.total_output_tokens,
    };

    let config = CompactionConfig {
        budget: session.config.token_budget.clone(),
        kind: CompactionKind::Proactive,
    };

    let summarizer =
        DefaultHistorySummarizer::with_slug(Arc::clone(provider), model_slug, max_tokens);

    match compact_history(&items, &token_info, &summarizer, &config).await {
        Ok(CompactAction::Replaced(compacted_items)) => {
            let new_messages: Vec<Message> = compacted_items
                .into_iter()
                .filter_map(|item| match item {
                    ResponseItem::Message(msg) => Some(msg),
                    _ => None,
                })
                .collect();
            let removed = session
                .prompt_source_messages()
                .len()
                .saturating_sub(new_messages.len());
            info!("LLM compaction removed {removed} messages");
            session.set_prompt_messages(new_messages);
        }
        Ok(CompactAction::Skipped) => {
            debug!("LLM compaction skipped, nothing to compact");
        }
        Err(e) => {
            warn!("LLM compaction failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Micro compact
// ---------------------------------------------------------------------------

/// TODO: Now, the micro compact acts like a truncation, however, we already
/// have truncation policy, should follow model's truncation policy, so the
/// micro compact should be removed in the future.
const MICRO_COMPACT_THRESHOLD: usize = 10_000;

fn micro_compact(content: String) -> String {
    if content.len() > MICRO_COMPACT_THRESHOLD {
        let truncate_at = content
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= MICRO_COMPACT_THRESHOLD)
            .last()
            .unwrap_or(0);
        let mut truncated = content[..truncate_at].to_string();
        truncated.push_str("\n...[truncated]");
        truncated
    } else {
        content
    }
}

// ---------------------------------------------------------------------------
// Main query loop
// ---------------------------------------------------------------------------

const MAX_RETRIES: usize = 5;
const INITIAL_RETRY_BACKOFF_MS: u64 = 250;

/// TODO: The body of `query` is too lengthy, we should move out `stream lop` out, I am
/// not sure whether we should do this.
/// The recursive agent loop the beating heart of the runtime.
///
/// The implementation refers to Claude Code's `query.ts`. It drives
/// multi-turn conversations by:
///
/// 1. Building the model request from session state
/// 2. Streaming the model response
/// 3. Collecting assistant text and tool_use blocks
/// 4. Executing tool calls via the orchestrator
/// 5. Appending tool_result messages
/// 6. Recursing if the model wants to continue
///
/// The loop terminates when:
/// - The model emits `end_turn` with no tool calls
/// - An unrecoverable error occurs
pub async fn query(
    session: &mut SessionState,
    turn_config: &TurnConfig,
    provider: Arc<dyn ModelProviderSDK>,
    registry: Arc<ToolRegistry>,
    runtime: &ToolRuntime,
    on_event: Option<EventCallback>,
) -> Result<(), AgentError> {
    // emit is the event callback function.
    let emit = |event: QueryEvent| {
        if let Some(ref cb) = on_event {
            cb(event);
        }
    };

    let agents_md_manager = AgentsMdManager::new(session.config.agents_md.clone());
    let current_agents_snapshot = load_workspace_instructions(&session.cwd, &agents_md_manager);

    if session.session_context.is_none() {
        session.session_context = Some(SessionContext::capture(
            &turn_config.model,
            turn_config.thinking_selection.as_deref(),
            &session.cwd,
            current_agents_snapshot.clone(),
        ));
    }
    let current_turn_context = TurnContext::capture(
        &turn_config.model,
        turn_config.thinking_selection.as_deref(),
        &session.cwd,
        current_agents_snapshot.clone(),
    );
    if let Some(diff) = session
        .latest_turn_context
        .as_ref()
        .and_then(|previous| current_turn_context.diff_since(previous))
    {
        session.insert_context_message(diff.to_message());
    }
    if let Some(previous_turn_context) = session.latest_turn_context.as_ref()
        && let Some(diff) = AgentsMdManager::diff(
            previous_turn_context.observed_agents_snapshot.as_ref(),
            current_agents_snapshot.as_ref(),
        )
    {
        session.insert_context_message(AgentsMdDiffFragment::new(diff).to_message());
    }
    session.latest_turn_context = Some(current_turn_context);
    let session_context = session
        .session_context
        .clone()
        .expect("session context should be initialized");
    let prefetched_user_inputs = session_context.prefix_user_inputs();

    let mut retry_count: usize = 0;
    let mut context_compacted = false;
    let mut budget_steer_injected = false;

    if session.turn_state.is_none() {
        session.start_turn(devo_protocol::TurnKind::Regular);
    }

    'query_loop: loop {
        let pending = session.take_turn_pending_input();

        // If the user interrupted the assistant mid-turn, explain the interruption
        if !pending.is_empty()
            && session
                .messages
                .last()
                .is_some_and(|m| m.role == Role::Assistant)
        {
            let fragment = TurnAborted::new(TurnAborted::INTERRUPTED_GUIDANCE);
            if let ResponseItem::Message(msg) = fragment.to_response_item() {
                session.push_message(msg);
            }
        }

        for item in &pending {
            match &item.kind {
                devo_protocol::PendingInputKind::UserText { text } => {
                    session.push_message(Message::user(text.clone()));
                }
                devo_protocol::PendingInputKind::ToolCallBlockedByHook {
                    tool_use_id,
                    reason,
                } => {
                    session.push_message(Message::user(format!(
                        "[Tool call {} was blocked: {}]",
                        tool_use_id, reason
                    )));
                }
                devo_protocol::PendingInputKind::BudgetLimitSteering => {
                    session.push_message(Message::system(
                        "Note: The conversation is approaching the token budget limit. \
                         Please be concise and consider wrapping up the current task.",
                    ));
                }
            }
        }

        // 1.3 + 1.7: Check token budget and compact before building the request
        if session.last_input_tokens > 0
            && session
                .config
                .token_budget
                .should_compact(session.last_input_tokens)
        {
            if !budget_steer_injected {
                if let Some(turn) = session.turn_state.as_mut() {
                    turn.push_pending_input(devo_protocol::PendingInputItem {
                        kind: devo_protocol::PendingInputKind::BudgetLimitSteering,
                        metadata: None,
                        created_at: chrono::Utc::now(),
                    });
                }
                budget_steer_injected = true;
            }
            info!("token budget threshold exceeded, running LLM compaction");
            summarize_and_compact(
                session,
                &provider,
                &turn_config.model.slug,
                turn_config.model.max_tokens.unwrap_or(4096) as usize,
            )
            .await;
        }

        session.turn_count += 1;
        let turn_span = info_span!(
            "turn",
            turn = session.turn_count,
            session_id = %session.id,
            model = %turn_config.model.slug,
            cwd = %session.cwd.display()
        );
        let _turn_guard = turn_span.enter();
        info!("starting turn");

        // Build model request from the session-locked prefix.
        let system = session_context.build_system_prompt();

        // resolve thinking request parameter
        let ResolvedThinkingRequest {
            request_model,
            request_thinking,
            request_reasoning_effort,
            extra_body,
            effective_reasoning_effort: _,
        } = turn_config
            .model
            .resolve_thinking_selection(turn_config.thinking_selection.as_deref());

        let history = History {
            items: session
                .prompt_source_messages()
                .iter()
                .cloned()
                .flat_map(message_to_response_items)
                .collect(),
            token_info: TokenInfo::default(),
            context: ContextView::new(
                std::env::consts::OS,
                session_context.environment.shell.clone(),
                session_context.environment.timezone.clone(),
                session_context.model.slug.clone(),
                session_context
                    .reasoning_effort
                    .map(|effort| effort.label().to_lowercase()),
                Some(session_context.persona.as_str().to_string()),
                session_context.environment.current_date.clone(),
                session_context.environment.cwd.display().to_string(),
            ),
        };
        let messages = history
            .for_prompt_with_prefix(&prefetched_user_inputs, &turn_config.model.input_modalities);

        let request = ModelRequest {
            model: request_model,
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            messages,
            max_tokens: turn_config
                .model
                .max_tokens
                .map_or(session.config.token_budget.max_output_tokens, |value| {
                    value as usize
                }),
            tools: Some(registry.tool_definitions()),
            sampling: SamplingControls {
                temperature: turn_config.model.temperature,
                top_p: turn_config.model.top_p,
                top_k: turn_config.model.top_k.map(|value| value as u32),
            },
            thinking: request_thinking,
            reasoning_effort: request_reasoning_effort,
            extra_body,
        };
        session.prompt_token_estimate = estimate_request_prompt_tokens(&request);
        debug!(
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            has_system = request.system.is_some(),
            "built model request"
        );

        // Stream with error classification
        let stream_result = provider.completion_stream(request).await;

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    provider = provider.name(),
                    model = %turn_config.model.slug,
                    turn = session.turn_count,
                    error = ?e,
                    "failed to create provider stream"
                );
                match provider_retry_decision(&e, &mut retry_count, &mut context_compacted) {
                    ProviderRetryDecision::CompactAndRetry => {
                        warn!("context_too_long - compacting and retrying");
                        summarize_and_compact(
                            session,
                            &provider,
                            &turn_config.model.slug,
                            turn_config.model.max_tokens.unwrap_or(4096) as usize,
                        )
                        .await;
                        session.turn_count -= 1;
                        continue;
                    }
                    ProviderRetryDecision::RetryAfter(backoff) => {
                        warn!(
                            attempt = retry_count,
                            backoff_ms = backoff.as_millis(),
                            "transient provider error - retrying with exponential backoff"
                        );
                        sleep(backoff).await;
                        session.turn_count -= 1;
                        continue;
                    }
                    ProviderRetryDecision::Fail => {
                        return Err(AgentError::Provider(e));
                    }
                }
            }
        };

        // HTTP return ok, then processing Server Sent Event

        let mut assistant_text = String::new();
        let mut reasoning_text = String::new();
        let mut tool_uses: Vec<(String, String, serde_json::Value, String, bool)> = Vec::new();
        let mut final_response = None;
        let mut stop_reason = None;

        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::TextStart { .. }) => {}
                Ok(StreamEvent::TextDelta { text, .. }) => {
                    assistant_text.push_str(&text);
                    emit(QueryEvent::TextDelta(text));
                }
                Ok(StreamEvent::ReasoningStart { .. }) => {}
                Ok(StreamEvent::ReasoningDelta { text, .. }) => {
                    reasoning_text.push_str(&text);
                    emit(QueryEvent::ReasoningDelta(text));
                }
                Ok(StreamEvent::ReasoningDone { .. }) => {
                    emit(QueryEvent::ReasoningCompleted);
                }
                Ok(StreamEvent::ToolCallStart {
                    id, name, input, ..
                }) => {
                    tool_uses.push((id, name, input, String::new(), false));
                }
                Ok(StreamEvent::ToolCallInputDelta { partial_json, .. }) => {
                    if let Some(last) = tool_uses.last_mut() {
                        last.3.push_str(&partial_json);
                        last.4 = true;
                    }
                }
                Ok(StreamEvent::MessageDone { response }) => {
                    stop_reason = response.stop_reason.clone();
                    final_response = Some(response.clone());

                    // Accumulate all usage counters at completion time.
                    session.total_input_tokens += response.usage.input_tokens;
                    session.total_output_tokens += response.usage.output_tokens;
                    session.total_cache_creation_tokens +=
                        response.usage.cache_creation_input_tokens.unwrap_or(0);
                    session.total_cache_read_tokens +=
                        response.usage.cache_read_input_tokens.unwrap_or(0);
                    session.last_input_tokens = response.usage.input_tokens;

                    emit(QueryEvent::Usage {
                        input_tokens: response.usage.input_tokens,
                        output_tokens: response.usage.output_tokens,
                        cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
                        cache_read_input_tokens: response.usage.cache_read_input_tokens,
                    });
                }
                Ok(StreamEvent::UsageDelta(usage)) => {
                    emit(QueryEvent::UsageDelta {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                    });
                }
                Err(e) => {
                    warn!(
                        provider = provider.name(),
                        model = %turn_config.model.slug,
                        turn = session.turn_count,
                        error = ?e,
                        "stream error"
                    );
                    if !assistant_text.is_empty()
                        || !reasoning_text.is_empty()
                        || !tool_uses.is_empty()
                        || final_response.is_some()
                    {
                        return Err(AgentError::Provider(e));
                    }

                    match provider_retry_decision(&e, &mut retry_count, &mut context_compacted) {
                        ProviderRetryDecision::CompactAndRetry => {
                            warn!("context_too_long - compacting and retrying");
                            summarize_and_compact(
                                session,
                                &provider,
                                &turn_config.model.slug,
                                turn_config.model.max_tokens.unwrap_or(4096) as usize,
                            )
                            .await;
                            session.turn_count -= 1;
                            continue 'query_loop;
                        }
                        ProviderRetryDecision::RetryAfter(backoff) => {
                            warn!(
                                attempt = retry_count,
                                backoff_ms = backoff.as_millis(),
                                "transient provider stream error - retrying with exponential backoff"
                            );
                            sleep(backoff).await;
                            session.turn_count -= 1;
                            continue 'query_loop;
                        }
                        ProviderRetryDecision::Fail => {
                            return Err(AgentError::Provider(e));
                        }
                    }
                }
            }
        }

        retry_count = 0;
        context_compacted = false;

        if let Some(response) = &final_response {
            if assistant_text.is_empty() {
                assistant_text = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ResponseContent::Text(text) => Some(text.as_str()),
                        ResponseContent::ToolUse { .. } => None,
                    })
                    .collect();
            }
            if tool_uses.is_empty() {
                tool_uses = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ResponseContent::ToolUse { id, name, input } => Some((
                            id.clone(),
                            name.clone(),
                            input.clone(),
                            String::new(),
                            false,
                        )),
                        ResponseContent::Text(_) => None,
                    })
                    .collect();
            }
            if reasoning_text.is_empty() {
                let final_reasoning = response
                    .metadata
                    .extras
                    .iter()
                    .filter_map(|extra| match extra {
                        ResponseExtra::ReasoningText { text } => Some(text.as_str()),
                        ResponseExtra::ProviderSpecific { .. } => None,
                    })
                    .collect::<String>();
                if !final_reasoning.is_empty() {
                    emit(QueryEvent::ReasoningDelta(final_reasoning.clone()));
                    reasoning_text = final_reasoning;
                }
            }
        }

        // Build assistant message
        let mut assistant_content: Vec<ContentBlock> = Vec::new();

        if !reasoning_text.is_empty() {
            assistant_content.push(ContentBlock::Reasoning {
                text: reasoning_text,
            });
        }

        if !assistant_text.is_empty() {
            assistant_content.push(ContentBlock::Text {
                text: assistant_text,
            });
        }

        let tool_calls: Vec<ToolCall> = tool_uses
            .into_iter()
            .map(|(id, name, initial_input, json_str, saw_delta)| {
                let input = if saw_delta {
                    serde_json::from_str(&json_str).unwrap_or(initial_input)
                } else {
                    initial_input
                };
                emit(QueryEvent::ToolUseStart {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                assistant_content.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                ToolCall { id, name, input }
            })
            .collect();

        session.push_message(Message {
            role: Role::Assistant,
            content: assistant_content,
        });

        // If no tool calls, check stop reason
        if tool_calls.is_empty() {
            // MaxOutputTokens auto-continue
            if stop_reason == Some(StopReason::MaxTokens) {
                debug!("max_tokens reached injecting continuation prompt");
                session.push_message(Message::user("Please continue from where you left off."));
                continue;
            }

            if let Some(sr) = stop_reason {
                emit(QueryEvent::TurnComplete { stop_reason: sr });
            }
            debug!("no tool calls, ending query loop");
            session.end_turn();
            return Ok(());
        }

        // Execute tool calls
        let results = runtime.execute_batch(&tool_calls).await;

        // Build tool call name -> input map for computing summaries
        let tool_call_map: std::collections::HashMap<&str, (&str, &serde_json::Value)> = tool_calls
            .iter()
            .map(|c| (c.id.as_str(), (c.name.as_str(), &c.input)))
            .collect();

        // Build tool result message (user role, per Anthropic API convention)
        // Apply micro-compact to large tool results
        let result_content: Vec<ContentBlock> = results
            .into_iter()
            .map(|r| {
                let content_str = r.content.into_string();
                let compacted_content = micro_compact(content_str);
                let summary = tool_call_map
                    .get(r.tool_use_id.as_str())
                    .map(|(name, input)| {
                        devo_tools::tool_summary::tool_summary(name, input, &session.cwd)
                    })
                    .unwrap_or_default();
                emit(QueryEvent::ToolResult {
                    tool_use_id: r.tool_use_id.clone(),
                    content: compacted_content.clone(),
                    is_error: r.is_error,
                    summary: summary.clone(),
                });
                ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: compacted_content,
                    is_error: r.is_error,
                }
            })
            .collect();

        session.push_message(Message {
            role: Role::User,
            content: result_content,
        });
    }
}

/// Sends a minimal provider probe request used by onboarding and configuration checks.
pub async fn test_model_connection(
    provider: &dyn ModelProviderSDK,
    model: &Model,
    prompt: &str,
) -> Result<String, AgentError> {
    let ResolvedThinkingRequest {
        request_model,
        request_thinking,
        request_reasoning_effort,
        extra_body,
        effective_reasoning_effort: _,
    } = model.resolve_thinking_selection(None);
    let request = ModelRequest {
        model: request_model,
        system: None,
        messages: vec![devo_protocol::RequestMessage {
            role: "user".to_string(),
            content: vec![devo_protocol::RequestContent::Text {
                text: prompt.to_string(),
            }],
        }],
        max_tokens: model.max_tokens.map_or(64, |value| value as usize),
        tools: None,
        sampling: SamplingControls {
            temperature: model.temperature,
            top_p: model.top_p,
            top_k: model.top_k.map(|value| value as u32),
        },
        thinking: request_thinking,
        reasoning_effort: request_reasoning_effort,
        extra_body,
    };
    let mut stream = provider.completion_stream(request).await?;
    let mut reply_preview = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::TextDelta { text, .. } => reply_preview.push_str(&text),
            StreamEvent::MessageDone { response } => {
                if reply_preview.trim().is_empty() {
                    reply_preview = response
                        .content
                        .into_iter()
                        .find_map(|content| match content {
                            ResponseContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .unwrap_or_default();
                }
                break;
            }
            _ => {}
        }
    }
    let preview = reply_preview.trim();
    if preview.is_empty() {
        return Err(AgentError::Provider(anyhow::anyhow!(
            "provider validation completed without a model reply"
        )));
    }
    Ok(preview.to_string())
}

fn retry_backoff_duration(attempt: usize) -> Duration {
    let exponent = attempt.saturating_sub(1).min(10) as u32;
    let multiplier = 2u64.pow(exponent);
    Duration::from_millis(INITIAL_RETRY_BACKOFF_MS.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use anyhow::Result;
    use async_trait::async_trait;
    use devo_protocol::ModelRequest;
    use devo_protocol::ModelResponse;
    use devo_protocol::ResponseContent;
    use devo_protocol::ResponseExtra;
    use devo_protocol::ResponseMetadata;
    use devo_protocol::StopReason;
    use devo_protocol::StreamEvent;
    use devo_protocol::Usage;
    use devo_provider::ModelProviderSDK;
    use devo_safety::PermissionMode;
    use devo_tools::ToolRegistry;
    use devo_tools::ToolRuntime;
    use devo_tools::errors::ToolExecutionError;
    use devo_tools::handler_kind::ToolHandlerKind;
    use devo_tools::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
    use devo_tools::json_schema::JsonSchema;
    use devo_tools::registry::ToolRegistryBuilder;
    use devo_tools::router::PermissionChecker;
    use devo_tools::tool_handler::ToolHandler;
    use devo_tools::tool_spec::{ToolExecutionMode, ToolOutputMode, ToolSpec};
    use futures::Stream;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::QueryEvent;
    use super::query;
    use super::test_model_connection;
    use crate::ContentBlock;
    use crate::Message;
    use crate::Model;
    use crate::ReasoningEffort;
    use crate::Role;
    use crate::SessionConfig;
    use crate::SessionState;
    use crate::ThinkingCapability;
    use crate::ThinkingImplementation;
    use crate::ThinkingVariant;
    use crate::ThinkingVariantConfig;
    use crate::TruncationMode;
    use crate::TruncationPolicyConfig;
    use crate::TurnConfig;

    struct SingleToolUseProvider {
        requests: AtomicUsize,
    }

    #[async_trait]
    impl devo_provider::ModelProviderSDK for SingleToolUseProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
            unreachable!("tests stream responses only")
        }

        async fn completion_stream(
            &self,
            _request: ModelRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            let request_number = self.requests.fetch_add(1, Ordering::SeqCst);

            let events = if request_number == 0 {
                vec![
                    Ok(StreamEvent::ToolCallStart {
                        index: 0,
                        id: "tool-1".into(),
                        name: "mutating_tool".into(),
                        input: json!({}),
                    }),
                    Ok(StreamEvent::ToolCallInputDelta {
                        index: 0,
                        partial_json: r#"{"value":1}"#.into(),
                    }),
                    Ok(StreamEvent::MessageDone {
                        response: ModelResponse {
                            id: "resp-1".into(),
                            content: vec![ResponseContent::ToolUse {
                                id: "tool-1".into(),
                                name: "mutating_tool".into(),
                                input: json!({ "value": 1 }),
                            }],
                            stop_reason: Some(StopReason::ToolUse),
                            usage: Usage::default(),
                            metadata: Default::default(),
                        },
                    }),
                ]
            } else {
                vec![
                    Ok(StreamEvent::TextDelta {
                        index: 0,
                        text: "done".into(),
                    }),
                    Ok(StreamEvent::MessageDone {
                        response: ModelResponse {
                            id: "resp-2".into(),
                            content: vec![ResponseContent::Text("done".into())],
                            stop_reason: Some(StopReason::EndTurn),
                            usage: Usage::default(),
                            metadata: Default::default(),
                        },
                    }),
                ]
            };

            Ok(Box::pin(futures::stream::iter(events)))
        }

        fn name(&self) -> &str {
            "test-provider"
        }
    }

    struct MutatingTool;

    struct CapturingProvider {
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    struct OpenAiCapturingProvider {
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    struct TransientStreamCreateProvider {
        attempts: AtomicUsize,
    }

    struct TransientStreamEventProvider {
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl devo_provider::ModelProviderSDK for CapturingProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
            unreachable!("tests stream responses only")
        }

        async fn completion_stream(
            &self,
            request: ModelRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            self.requests.lock().expect("lock requests").push(request);
            Ok(Box::pin(futures::stream::iter(vec![Ok(
                StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp".into(),
                        content: vec![ResponseContent::Text("done".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                },
            )])))
        }

        fn name(&self) -> &str {
            "capturing-provider"
        }
    }

    #[async_trait]
    impl devo_provider::ModelProviderSDK for OpenAiCapturingProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
            unreachable!("tests stream responses only")
        }

        async fn completion_stream(
            &self,
            request: ModelRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            self.requests.lock().expect("lock requests").push(request);
            Ok(Box::pin(futures::stream::iter(vec![Ok(
                StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp".into(),
                        content: vec![ResponseContent::Text("done".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                },
            )])))
        }

        fn name(&self) -> &str {
            "openai"
        }
    }

    #[async_trait]
    impl devo_provider::ModelProviderSDK for TransientStreamCreateProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
            unreachable!("tests stream responses only")
        }

        async fn completion_stream(
            &self,
            _request: ModelRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return Err(anyhow::anyhow!("503 service unavailable"));
            }

            Ok(Box::pin(futures::stream::iter(vec![Ok(
                StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp".into(),
                        content: vec![ResponseContent::Text("done".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                },
            )])))
        }

        fn name(&self) -> &str {
            "transient-stream-create-provider"
        }
    }

    #[async_trait]
    impl devo_provider::ModelProviderSDK for TransientStreamEventProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
            unreachable!("tests stream responses only")
        }

        async fn completion_stream(
            &self,
            _request: ModelRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return Ok(Box::pin(futures::stream::iter(vec![Err(anyhow::anyhow!(
                    "500 internal server error"
                ))])));
            }

            Ok(Box::pin(futures::stream::iter(vec![Ok(
                StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp".into(),
                        content: vec![ResponseContent::Text("done".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                },
            )])))
        }

        fn name(&self) -> &str {
            "transient-stream-event-provider"
        }
    }

    #[async_trait]
    #[async_trait]
    impl ToolHandler for MutatingTool {
        fn tool_kind(&self) -> ToolHandlerKind {
            ToolHandlerKind::Write
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
            _progress: Option<devo_tools::events::ToolProgressSender>,
        ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
            Ok(Box::new(FunctionToolOutput::success("ok")))
        }
    }

    #[tokio::test]
    async fn query_retries_transient_stream_creation_errors() {
        let provider = Arc::new(TransientStreamCreateProvider {
            attempts: AtomicUsize::new(0),
        });
        let provider_sdk: Arc<dyn ModelProviderSDK> = provider.clone();
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        session.push_message(Message::user("hello"));

        query(
            &mut session,
            &TurnConfig {
                model: Model::default(),
                thinking_selection: None,
            },
            provider_sdk,
            registry,
            &runtime,
            None,
        )
        .await
        .expect("query should retry and succeed");

        assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            session.messages.last(),
            Some(&Message::assistant_text("done"))
        );
    }

    #[tokio::test]
    async fn query_retries_transient_stream_event_errors_before_content() {
        let provider = Arc::new(TransientStreamEventProvider {
            attempts: AtomicUsize::new(0),
        });
        let provider_sdk: Arc<dyn ModelProviderSDK> = provider.clone();
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        session.push_message(Message::user("hello"));

        query(
            &mut session,
            &TurnConfig {
                model: Model::default(),
                thinking_selection: None,
            },
            provider_sdk,
            registry,
            &runtime,
            None,
        )
        .await
        .expect("query should retry and succeed");

        assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            session.messages.last(),
            Some(&Message::assistant_text("done"))
        );
    }

    #[tokio::test]
    async fn query_uses_session_permission_mode_for_mutating_tools() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("mutating_tool", Arc::new(MutatingTool));
        builder.push_spec(ToolSpec {
            name: "mutating_tool".into(),
            description: "A test-only mutating tool.".into(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        });
        let registry = Arc::new(builder.build());
        let deny_checker = PermissionChecker::new(|request| {
            let n = request.tool_name;
            Box::pin(async move { Err(format!("{n} denied")) })
        });
        let runtime = ToolRuntime::new(Arc::clone(&registry), deny_checker);

        let mut session = SessionState::new(
            SessionConfig {
                permission_mode: PermissionMode::Deny,
                ..Default::default()
            },
            std::env::temp_dir(),
        );
        session.push_message(Message::user("run the tool"));

        query(
            &mut session,
            &TurnConfig {
                model: Model::default(),
                thinking_selection: None,
            },
            Arc::new(SingleToolUseProvider {
                requests: AtomicUsize::new(0),
            }),
            registry,
            &runtime,
            None,
        )
        .await
        .expect("query should complete and append a tool_result");

        let tool_result_message = session
            .messages
            .iter()
            .find(|message| {
                message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
            })
            .expect("tool_result message should be appended");
        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &tool_result_message.content[0]
        else {
            panic!("expected tool_result content block");
        };

        assert_eq!(tool_use_id, "tool-1");
        assert!(
            *is_error,
            "denied permission should surface as a tool error"
        );
        assert!(
            content.contains("permission denied"),
            "expected tool_result to mention permission denial, got: {content}"
        );
    }

    #[tokio::test]
    async fn query_resolves_model_variant_thinking_before_building_request() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ModelProviderSDK> = Arc::new(CapturingProvider {
            requests: Arc::clone(&requests),
        });
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let model = Model {
            slug: "kimi-k2.5".into(),
            display_name: "Kimi K2.5".into(),
            provider: devo_protocol::ProviderWireApi::OpenAIChatCompletions,
            description: None,
            thinking_capability: ThinkingCapability::Toggle,
            default_reasoning_effort: Some(ReasoningEffort::Medium),
            thinking_implementation: Some(ThinkingImplementation::ModelVariant(
                ThinkingVariantConfig {
                    variants: vec![
                        ThinkingVariant {
                            selection_value: "disabled".into(),
                            model_slug: "kimi-k2.5".into(),
                            reasoning_effort: None,
                            label: "Off".into(),
                            description: "Use the standard model".into(),
                        },
                        ThinkingVariant {
                            selection_value: "enabled".into(),
                            model_slug: "kimi-k2.5-thinking".into(),
                            reasoning_effort: Some(ReasoningEffort::Medium),
                            label: "On".into(),
                            description: "Use the thinking model".into(),
                        },
                    ],
                },
            )),
            base_instructions: String::new(),
            context_window: 200_000,
            effective_context_window_percent: None,
            truncation_policy: TruncationPolicyConfig {
                mode: TruncationMode::Tokens,
                limit: 10_000,
            },
            input_modalities: vec![],
            supports_image_detail_original: false,
            channel: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
        };
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        session.push_message(Message::user("hello"));

        query(
            &mut session,
            &TurnConfig {
                model,
                thinking_selection: Some("enabled".into()),
            },
            Arc::clone(&provider),
            registry,
            &runtime,
            None,
        )
        .await
        .expect("query should succeed");

        let captured = requests.lock().expect("lock requests");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model, "kimi-k2.5-thinking");
        assert_eq!(captured[0].thinking, None);
    }

    #[tokio::test]
    async fn query_locks_system_prompt_and_environment_prefix_per_session() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ModelProviderSDK> = Arc::new(CapturingProvider {
            requests: Arc::clone(&requests),
        });
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let temp_root =
            std::env::temp_dir().join(format!("devo-query-lock-{}", uuid::Uuid::new_v4()));
        let second_cwd = temp_root.join("nested");
        let first_model = Model {
            slug: "model-a".into(),
            base_instructions: "base-a".into(),
            ..Model::default()
        };
        let second_model = Model {
            slug: "model-b".into(),
            base_instructions: "base-b".into(),
            ..Model::default()
        };

        let mut session = SessionState::new(SessionConfig::default(), temp_root.clone());
        session.push_message(Message::user("hello"));

        query(
            &mut session,
            &TurnConfig {
                model: first_model,
                thinking_selection: None,
            },
            Arc::clone(&provider),
            Arc::clone(&registry),
            &runtime,
            None,
        )
        .await
        .expect("first query should succeed");

        session.cwd = second_cwd;
        session.push_message(Message::user("follow up"));

        query(
            &mut session,
            &TurnConfig {
                model: second_model,
                thinking_selection: Some("enabled".into()),
            },
            Arc::clone(&provider),
            registry,
            &runtime,
            None,
        )
        .await
        .expect("second query should succeed");

        let captured = requests.lock().expect("lock requests");
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].system.as_deref(), Some("base-a"));
        assert_eq!(captured[1].system.as_deref(), Some("base-a"));

        let first_prefix = &captured[0].messages[0];
        let second_prefix = &captured[1].messages[0];
        assert_eq!(first_prefix.role, second_prefix.role);
        let devo_protocol::RequestContent::Text { text: first_text } = &first_prefix.content[0]
        else {
            panic!("expected text prefix");
        };
        let devo_protocol::RequestContent::Text { text: second_text } = &second_prefix.content[0]
        else {
            panic!("expected text prefix");
        };
        assert_eq!(first_text, second_text);
    }

    #[tokio::test]
    async fn query_inserts_context_diff_before_changed_turn_input() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ModelProviderSDK> = Arc::new(CapturingProvider {
            requests: Arc::clone(&requests),
        });
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        let first_model = Model {
            slug: "model-a".into(),
            ..Model::default()
        };
        let second_model = Model {
            slug: "model-b".into(),
            ..Model::default()
        };

        session.push_message(Message::user("hello"));
        query(
            &mut session,
            &TurnConfig {
                model: first_model,
                thinking_selection: None,
            },
            Arc::clone(&provider),
            Arc::clone(&registry),
            &runtime,
            None,
        )
        .await
        .expect("first query should succeed");

        session.push_message(Message::user("follow up"));
        query(
            &mut session,
            &TurnConfig {
                model: second_model,
                thinking_selection: Some("enabled".into()),
            },
            Arc::clone(&provider),
            registry,
            &runtime,
            None,
        )
        .await
        .expect("second query should succeed");

        let diff_message = &session.messages[session.messages.len() - 3];
        let user_message = &session.messages[session.messages.len() - 2];
        assert_eq!(user_message, &Message::user("follow up"));
        let ContentBlock::Text { text } = &diff_message.content[0] else {
            panic!("expected text diff message");
        };
        assert!(text.contains("<context_changes>"));
        assert!(text.contains("model: model-a -> model-b"));
    }

    #[tokio::test]
    async fn query_drops_orphaned_tool_calls_from_prompt_history() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ModelProviderSDK> = Arc::new(CapturingProvider {
            requests: Arc::clone(&requests),
        });
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());

        session.push_message(Message::user("first"));
        session.push_message(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Calling tool".into(),
                },
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "bash".into(),
                    input: json!({ "cmd": "pwd" }),
                },
            ],
        });
        session.push_message(Message::user("follow up"));

        query(
            &mut session,
            &TurnConfig {
                model: Model::default(),
                thinking_selection: None,
            },
            provider,
            registry,
            &runtime,
            None,
        )
        .await
        .expect("query should succeed");

        let captured = requests.lock().expect("lock requests");
        assert_eq!(captured.len(), 1);
        assert!(
            captured[0]
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .all(|content| !matches!(content, devo_protocol::RequestContent::ToolUse { .. })),
            "expected orphaned tool calls to be removed from prompt history"
        );
    }

    #[tokio::test]
    async fn test_model_connection_sends_minimal_request() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = CapturingProvider {
            requests: Arc::clone(&requests),
        };
        let model = Model {
            slug: "glm-4.5".into(),
            top_p: Some(0.95),
            ..Model::default()
        };
        let preview = test_model_connection(&provider, &model, "Reply with OK only.")
            .await
            .expect("probe request should succeed");

        let captured = requests.lock().expect("lock requests");
        assert_eq!(preview, "done");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].system, None);
        assert!(captured[0].tools.is_none());
        assert_eq!(captured[0].messages.len(), 1);
        assert_eq!(captured[0].sampling.top_p, Some(0.95));
    }

    #[tokio::test]
    async fn query_emits_reasoning_without_polluting_assistant_message_content() {
        struct ReasoningProvider;

        #[async_trait]
        impl devo_provider::ModelProviderSDK for ReasoningProvider {
            async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
                unreachable!("tests stream responses only")
            }

            async fn completion_stream(
                &self,
                _request: ModelRequest,
            ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
                Ok(Box::pin(futures::stream::iter(vec![
                    Ok(StreamEvent::ReasoningStart { index: 0 }),
                    Ok(StreamEvent::ReasoningDelta {
                        index: 0,
                        text: "plan".into(),
                    }),
                    Ok(StreamEvent::TextStart { index: 1 }),
                    Ok(StreamEvent::TextDelta {
                        index: 1,
                        text: "final".into(),
                    }),
                    Ok(StreamEvent::MessageDone {
                        response: ModelResponse {
                            id: "resp-3".into(),
                            content: vec![ResponseContent::Text("final".into())],
                            stop_reason: Some(StopReason::EndTurn),
                            usage: Usage::default(),
                            metadata: ResponseMetadata {
                                extras: vec![ResponseExtra::ReasoningText {
                                    text: "plan".into(),
                                }],
                            },
                        },
                    }),
                ])))
            }

            fn name(&self) -> &str {
                "reasoning-provider"
            }
        }

        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        session.push_message(Message::user("hello"));
        let seen_events = Arc::new(Mutex::new(Vec::new()));
        let callback_events = Arc::clone(&seen_events);
        let callback = Arc::new(move |event: QueryEvent| {
            callback_events.lock().expect("lock callback").push(event);
        });

        query(
            &mut session,
            &TurnConfig {
                model: Model::default(),
                thinking_selection: None,
            },
            Arc::new(ReasoningProvider),
            registry,
            &runtime,
            Some(callback),
        )
        .await
        .expect("query should succeed");

        let events = seen_events.lock().expect("lock events");
        assert!(events.iter().any(|event| matches!(
            event,
            QueryEvent::ReasoningDelta(text) if text == "plan"
        )));
        drop(events);

        let assistant_message = session
            .messages
            .iter()
            .find(|message| matches!(message.role, Role::Assistant))
            .expect("assistant message");
        assert_eq!(
            assistant_message,
            &Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "plan".into(),
                    },
                    ContentBlock::Text {
                        text: "final".into(),
                    },
                ],
            }
        );
    }

    #[tokio::test]
    async fn query_disables_openai_thinking_when_reasoning_context_is_missing() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ModelProviderSDK> = Arc::new(OpenAiCapturingProvider {
            requests: Arc::clone(&requests),
        });
        let registry = Arc::new(ToolRegistry::new());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
        let model = Model {
            slug: "deepseek-v4-flash".into(),
            provider: devo_protocol::ProviderWireApi::OpenAIChatCompletions,
            thinking_capability: ThinkingCapability::Toggle,
            base_instructions: String::new(),
            ..Model::default()
        };
        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        session.push_message(Message::assistant_text("legacy assistant reply"));
        session.push_message(Message::user("follow up"));

        query(
            &mut session,
            &TurnConfig {
                model,
                thinking_selection: Some("enabled".into()),
            },
            Arc::clone(&provider),
            registry,
            &runtime,
            None,
        )
        .await
        .expect("query should succeed");

        let captured = requests.lock().expect("lock requests");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].thinking.as_deref(), Some("enabled"));
        // Toggle capability does not set reasoning_effort on the request.
        assert_eq!(captured[0].reasoning_effort, None);
    }

    #[tokio::test]
    async fn query_tool_result_summary_is_set() {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("mutating_tool", Arc::new(MutatingTool));
        builder.push_spec(ToolSpec {
            name: "mutating_tool".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        });
        let registry = Arc::new(builder.build());
        let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));

        let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
        session.push_message(Message::user("run the tool"));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let callback = Arc::new(move |event: QueryEvent| {
            if let QueryEvent::ToolResult { summary, .. } = event {
                seen_clone.lock().unwrap().push(summary);
            }
        });

        query(
            &mut session,
            &TurnConfig {
                model: Model::default(),
                thinking_selection: None,
            },
            Arc::new(SingleToolUseProvider {
                requests: AtomicUsize::new(0),
            }),
            registry,
            &runtime,
            Some(callback),
        )
        .await
        .expect("query should complete");

        let summaries = seen.lock().unwrap();
        assert!(
            !summaries.is_empty(),
            "should have at least one ToolResult summary"
        );
        for summary in summaries.iter() {
            assert!(!summary.is_empty(), "summary should not be empty");
        }
    }
}
