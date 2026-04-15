use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clawcr_protocol::{
    ModelRequest, RequestContent, RequestMessage, ResolvedThinkingRequest, ResponseContent,
    ResponseExtra, SamplingControls, StopReason, StreamEvent, UserInput,
};
use futures::StreamExt;
use tokio::time::sleep;
use tracing::{debug, info, info_span, warn};

use clawcr_provider::ModelProviderSDK;
use clawcr_tools::{ToolCall, ToolContext, ToolOrchestrator, ToolRegistry};

use crate::{AgentError, ContentBlock, Message, Model, Role, SessionState, TurnConfig};

/// Events emitted during a query for the caller (CLI/UI) to observe.
#[derive(Debug, Clone)]
pub enum QueryEvent {
    /// Incremental text from the assistant.
    TextDelta(String),
    /// Incremental reasoning text from the assistant.
    ReasoningDelta(String),
    /// Incremental token usage update from the provider stream.
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
    /// A tool call completed.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
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

// ---------------------------------------------------------------------------
// Session compaction (capabilities 1.3 / 1.7)
// ---------------------------------------------------------------------------

/// TODO: The context compact is weired, should compact with a seperate LLM invoke.
/// Remove older messages to bring the conversation within budget.
/// Returns how many messages were removed.
fn compact_session(session: &mut SessionState) -> usize {
    let msg_count = session.messages.len();
    if msg_count <= 2 {
        return 0;
    }

    let input_budget = session.config.token_budget.input_budget();
    let last_tokens = session.last_input_tokens;

    if last_tokens == 0 {
        // No token data yet — drop the oldest half
        let remove = msg_count / 2;
        session.messages.drain(..remove);
        return remove;
    }

    let avg_tokens_per_msg = last_tokens / msg_count;
    if avg_tokens_per_msg == 0 {
        let remove = msg_count / 2;
        session.messages.drain(..remove);
        return remove;
    }

    // Aim for 70 % of input budget so the next request has headroom
    let target_tokens = (input_budget as f64 * 0.7) as usize;
    let keep_count = (target_tokens / avg_tokens_per_msg).max(2).min(msg_count);
    let remove_count = msg_count - keep_count;

    if remove_count > 0 {
        session.messages.drain(..remove_count);
    }
    remove_count
}

// ---------------------------------------------------------------------------
// Micro compact (capability 1.4)
// ---------------------------------------------------------------------------

/// TODO: Now, the micro compact acts like a truncation, however, we already
/// have truncation policy, should follow model's truncation policy, so the
/// micro compact should be removed in the future.
const MICRO_COMPACT_THRESHOLD: usize = 10_000;

fn micro_compact(content: String) -> String {
    if content.len() > MICRO_COMPACT_THRESHOLD {
        let mut truncated = content[..MICRO_COMPACT_THRESHOLD].to_string();
        truncated.push_str("\n...[truncated]");
        truncated
    } else {
        content
    }
}

// ---------------------------------------------------------------------------
// Memory prefetch (capability 1.9)
// ---------------------------------------------------------------------------

/// TODO: Current the Agent automatically read the `AGENTS.md` / `CLAUDE.md` at
/// current workspace root directory, for those md in sub-directory, what is
/// the load policy ? not sure, should investigate and design.
fn load_prompt_md(cwd: &std::path::Path) -> Option<String> {
    let mut sections = Vec::new();

    for file_name in ["AGENTS.md", "CLAUDE.md"] {
        let path = cwd.join(file_name);
        if let Ok(content) = std::fs::read_to_string(path) {
            let content = content.trim().to_string();
            if !content.is_empty() {
                sections.push(format!(
                    "# {} instructions for {}\n\n <INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
                    file_name,
                    cwd.display(),
                    content,
                ));
            }
        }
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

fn build_system_prompt(base_instructions: &str) -> String {
    let mut sections = Vec::new();
    if !base_instructions.is_empty() {
        sections.push(format!("{}", base_instructions.to_string()));
    }
    sections.join("\n\n")
}

fn build_environment_context(cwd: &Path) -> String {
    let shell = std::env::var("SHELL")
        .ok()
        .or_else(|| std::env::var("COMSPEC").ok())
        .unwrap_or_else(|| "unknown".to_string());

    let shell = shell
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(&shell)
        .to_lowercase();

    let current_date = chrono::Local::now().format("%Y-%m-%d").to_string();

    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string());

    format!(
        "<environment_context>\n  <cwd>{}</cwd>\n  <shell>{}</shell>\n  <current_date>{}</current_date>\n  <timezone>{}</timezone>\n</environment_context>",
        cwd.display(),
        shell,
        current_date,
        timezone,
    )
}

fn build_prefetched_user_inputs(cwd: &Path) -> Vec<UserInput> {
    let mut inputs = Vec::new();
    if let Some(text) = load_prompt_md(cwd) {
        inputs.push(UserInput::Text {
            text,
            text_elements: Vec::new(),
        });
    }
    inputs.push(UserInput::Text {
        text: build_environment_context(cwd),
        text_elements: Vec::new(),
    });
    inputs
}

fn append_prefetched_user_inputs(messages: &mut Vec<RequestMessage>, user_inputs: &[UserInput]) {
    messages.splice(
        0..0,
        user_inputs.iter().filter_map(|input| match input {
            UserInput::Text { text, .. } if !text.trim().is_empty() => Some(RequestMessage {
                role: Role::User.as_str().to_string(),
                content: vec![RequestContent::Text { text: text.clone() }],
            }),
            UserInput::Text { .. }
            | UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Skill { .. }
            | UserInput::Mention { .. }
            | _ => None,
        }),
    );
}

// ---------------------------------------------------------------------------
// Main query loop
// ---------------------------------------------------------------------------

const MAX_RETRIES: usize = 5;
const INITIAL_RETRY_BACKOFF_MS: u64 = 250;

/// TODO: The body of `query` is too lengthy, we should move out `stream lop` out, I am
/// not sure whether we should do this.

/// The recursive agent loop — the beating heart of the runtime.
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
    provider: &dyn ModelProviderSDK,
    registry: Arc<ToolRegistry>,
    orchestrator: &ToolOrchestrator,
    on_event: Option<EventCallback>,
) -> Result<(), AgentError> {
    // emit is the event callback function.
    let emit = |event: QueryEvent| {
        if let Some(ref cb) = on_event {
            cb(event);
        }
    };

    // Memory prefetch — load workspace instructions and environment context once
    // before the loop and inject them as leading user inputs.
    let prefetched_user_inputs = build_prefetched_user_inputs(&session.cwd);

    let mut retry_count: usize = 0;
    let mut context_compacted = false;

    loop {
        // Check token budget and compact before building the request
        if session.last_input_tokens > 0
            && session
                .config
                .token_budget
                .should_compact(session.last_input_tokens)
        {
            info!("token budget threshold exceeded — compacting session");
            compact_session(session);
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

        // Build model request
        // TODO: Should remove `memory_content` from system prompt
        let system = build_system_prompt(&turn_config.model.base_instructions);

        // resolve thinking request parameter
        let ResolvedThinkingRequest {
            request_model,
            request_thinking,
            extra_body,
            effective_reasoning_effort: _,
        } = turn_config
            .model
            .resolve_thinking_selection(turn_config.thinking_selection.as_deref());

        let mut messages = session.to_request_messages();
        append_prefetched_user_inputs(&mut messages, &prefetched_user_inputs);

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
            extra_body,
        };
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
            Ok(s) => {
                retry_count = 0;
                context_compacted = false;
                s
            }
            Err(e) => {
                warn!(
                    provider = provider.name(),
                    model = %turn_config.model.slug,
                    turn = session.turn_count,
                    error = ?e,
                    "failed to create provider stream"
                );
                match classify_error(&e) {
                    ErrorClass::ContextTooLong => {
                        // Compact history and retry once
                        if context_compacted {
                            return Err(AgentError::ContextTooLong);
                        }
                        warn!("context_too_long — compacting and retrying");
                        compact_session(session);
                        context_compacted = true;
                        session.turn_count -= 1;
                        continue;
                    }
                    ErrorClass::RateLimit | ErrorClass::ServerError => {
                        if retry_count < MAX_RETRIES {
                            retry_count += 1;
                            let backoff = retry_backoff_duration(retry_count);
                            warn!(
                                attempt = retry_count,
                                backoff_ms = backoff.as_millis(),
                                "transient error — retrying with exponential backoff"
                            );
                            sleep(backoff).await;
                            session.turn_count -= 1;
                            continue;
                        }
                        return Err(AgentError::Provider(e));
                    }
                    ErrorClass::ParameterError
                    | ErrorClass::FileContentAnomaly
                    | ErrorClass::AuthenticationFailure
                    | ErrorClass::FeatureUnavailable
                    | ErrorClass::TaskNotFound
                    | ErrorClass::NoApiPermission
                    | ErrorClass::FileTooLarge
                    | ErrorClass::Unretryable => {
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
                    return Err(AgentError::Provider(e));
                }
            }
        }

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
                debug!("max_tokens reached — injecting continuation prompt");
                session.push_message(Message::user("Please continue from where you left off."));
                continue;
            }

            if let Some(sr) = stop_reason {
                emit(QueryEvent::TurnComplete { stop_reason: sr });
            }
            debug!("no tool calls, ending query loop");
            return Ok(());
        }

        // Execute tool calls
        let tool_ctx = ToolContext {
            cwd: session.cwd.clone(),
            permissions: Arc::new(clawcr_safety::legacy_permissions::RuleBasedPolicy::new(
                session.config.permission_mode,
            )),
            session_id: session.id.clone(),
        };

        let results = orchestrator.execute_batch(&tool_calls, &tool_ctx).await;

        // Build tool result message (user role, per Anthropic API convention)
        // Apply micro-compact to large tool results
        let result_content: Vec<ContentBlock> = results
            .into_iter()
            .map(|r| {
                let compacted_content = micro_compact(r.output.content.clone());
                emit(QueryEvent::ToolResult {
                    tool_use_id: r.tool_use_id.clone(),
                    content: compacted_content.clone(),
                    is_error: r.output.is_error,
                });
                ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: compacted_content,
                    is_error: r.output.is_error,
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
        extra_body,
        effective_reasoning_effort: _,
    } = model.resolve_thinking_selection(None);
    let request = ModelRequest {
        model: request_model,
        system: None,
        messages: vec![clawcr_protocol::RequestMessage {
            role: "user".to_string(),
            content: vec![clawcr_protocol::RequestContent::Text {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::Result;
    use async_trait::async_trait;
    use clawcr_protocol::{
        ModelRequest, ModelResponse, ResponseContent, ResponseExtra, ResponseMetadata, StopReason,
        StreamEvent, Usage,
    };
    use clawcr_safety::legacy_permissions::PermissionMode;
    use clawcr_tools::{Tool, ToolOrchestrator, ToolOutput, ToolRegistry};
    use futures::Stream;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::{QueryEvent, query, test_model_connection};
    use crate::{
        ContentBlock, Message, Model, ProviderFamily, ReasoningEffort, Role, SessionConfig,
        SessionState, ThinkingCapability, ThinkingImplementation, ThinkingVariant,
        ThinkingVariantConfig, TruncationMode, TruncationPolicyConfig, TurnConfig,
    };

    struct SingleToolUseProvider {
        requests: AtomicUsize,
    }

    #[async_trait]
    impl clawcr_provider::ModelProviderSDK for SingleToolUseProvider {
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

    #[async_trait]
    impl clawcr_provider::ModelProviderSDK for CapturingProvider {
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
    impl Tool for MutatingTool {
        fn name(&self) -> &str {
            "mutating_tool"
        }

        fn description(&self) -> &str {
            "A test-only mutating tool."
        }

        fn input_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "integer" }
                },
                "required": ["value"]
            })
        }

        async fn execute(
            &self,
            _ctx: &clawcr_tools::ToolContext,
            _input: serde_json::Value,
        ) -> Result<ToolOutput> {
            Ok(ToolOutput::success("ok"))
        }
    }

    #[tokio::test]
    async fn query_uses_session_permission_mode_for_mutating_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(MutatingTool));
        let registry = Arc::new(registry);
        let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

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
            &SingleToolUseProvider {
                requests: AtomicUsize::new(0),
            },
            registry,
            &orchestrator,
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
        let provider = CapturingProvider {
            requests: Arc::clone(&requests),
        };
        let registry = Arc::new(ToolRegistry::new());
        let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
        let model = Model {
            slug: "kimi-k2.5".into(),
            display_name: "Kimi K2.5".into(),
            provider: ProviderFamily::openai(),
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
            &provider,
            registry,
            &orchestrator,
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
        impl clawcr_provider::ModelProviderSDK for ReasoningProvider {
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
        let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
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
            &ReasoningProvider,
            registry,
            &orchestrator,
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
                content: vec![ContentBlock::Text {
                    text: "final".into(),
                }],
            }
        );
    }
}
