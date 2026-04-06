use std::sync::Arc;

use futures::StreamExt;
use tracing::{debug, info, warn};

use clawcr_provider::{ModelProvider, ModelRequest, ResponseContent, StopReason, StreamEvent};
use clawcr_tools::{ToolCall, ToolContext, ToolOrchestrator, ToolRegistry};

use crate::{AgentError, ContentBlock, Message, Role, SessionState};

/// Events emitted during a query for the caller (CLI/UI) to observe.
#[derive(Debug, Clone)]
pub enum QueryEvent {
    /// Incremental text from the assistant.
    TextDelta(String),
    /// The assistant started a tool call.
    ToolUseStart { id: String, name: String },
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
    RateLimit,
    ServerError,
    Unretryable,
}

fn classify_error(e: &anyhow::Error) -> ErrorClass {
    let msg = e.to_string().to_lowercase();
    if msg.contains("context_too_long") {
        ErrorClass::ContextTooLong
    } else if msg.contains("429") || msg.contains("rate limit") {
        ErrorClass::RateLimit
    } else if msg.starts_with('5')
        || msg.contains("500")
        || msg.contains("502")
        || msg.contains("503")
        || msg.contains("internal server error")
    {
        ErrorClass::ServerError
    } else {
        ErrorClass::Unretryable
    }
}

// ---------------------------------------------------------------------------
// Session compaction (capabilities 1.3 / 1.7)
// ---------------------------------------------------------------------------

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

fn load_claude_md(cwd: &std::path::Path) -> Option<String> {
    let path = cwd.join("CLAUDE.md");
    std::fs::read_to_string(path).ok().filter(|s| !s.is_empty())
}

fn build_system_prompt(base: &str, memory: &Option<String>) -> String {
    match (base.is_empty(), memory) {
        (true, Some(mem)) => mem.clone(),
        (false, Some(mem)) => format!("{}\n\n{}", base, mem),
        _ => base.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Main query loop
// ---------------------------------------------------------------------------

const MAX_RETRIES: usize = 3;

/// The recursive agent loop — the beating heart of the runtime.
///
/// This is the Rust equivalent of Claude Code's `query.ts`. It drives
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
/// - Max turns are exceeded
/// - An unrecoverable error occurs
pub async fn query(
    session: &mut SessionState,
    provider: &dyn ModelProvider,
    registry: Arc<ToolRegistry>,
    orchestrator: &ToolOrchestrator,
    on_event: Option<EventCallback>,
) -> Result<(), AgentError> {
    let emit = |event: QueryEvent| {
        if let Some(ref cb) = on_event {
            cb(event);
        }
    };

    // 1.9: Memory prefetch — load CLAUDE.md once before the loop
    let memory_content = load_claude_md(&session.cwd);

    let mut retry_count: usize = 0;
    let mut context_compacted = false;

    loop {
        // 1.3 + 1.7: Check token budget and compact before building the request
        if session.last_input_tokens > 0
            && session
                .config
                .token_budget
                .should_compact(session.last_input_tokens)
        {
            info!("token budget threshold exceeded — compacting session");
            compact_session(session);
        }

        if session.turn_count >= session.config.max_turns {
            return Err(AgentError::MaxTurnsExceeded(session.config.max_turns));
        }

        session.turn_count += 1;
        info!(turn = session.turn_count, "starting turn");

        // Build model request
        let system = build_system_prompt(&session.config.system_prompt, &memory_content);
        let request = ModelRequest {
            model: session.config.model.clone(),
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            messages: session.to_request_messages(),
            max_tokens: session.config.token_budget.max_output_tokens,
            tools: Some(registry.tool_definitions()),
            temperature: None,
        };

        // 3.2: Stream with error classification
        let stream_result = provider.stream(request).await;

        let mut stream = match stream_result {
            Ok(s) => {
                retry_count = 0;
                context_compacted = false;
                s
            }
            Err(e) => {
                match classify_error(&e) {
                    ErrorClass::ContextTooLong => {
                        // 1.5: Compact history and retry once
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
                            warn!(attempt = retry_count, "transient error — retrying");
                            session.turn_count -= 1;
                            continue;
                        }
                        return Err(AgentError::Provider(e));
                    }
                    ErrorClass::Unretryable => {
                        return Err(AgentError::Provider(e));
                    }
                }
            }
        };

        let mut assistant_text = String::new();
        let mut tool_uses: Vec<(String, String, String)> = Vec::new(); // (id, name, json_accum)
        let mut stop_reason = None;

        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::TextDelta { text, .. }) => {
                    assistant_text.push_str(&text);
                    emit(QueryEvent::TextDelta(text));
                }
                Ok(StreamEvent::ContentBlockStart {
                    content: ResponseContent::ToolUse { id, name, .. },
                    ..
                }) => {
                    emit(QueryEvent::ToolUseStart {
                        id: id.clone(),
                        name: name.clone(),
                    });
                    tool_uses.push((id, name, String::new()));
                }
                Ok(StreamEvent::InputJsonDelta { partial_json, .. }) => {
                    if let Some(last) = tool_uses.last_mut() {
                        last.2.push_str(&partial_json);
                    }
                }
                Ok(StreamEvent::MessageDone { response }) => {
                    stop_reason = response.stop_reason.clone();

                    // 1.11: Accumulate all usage counters
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
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "stream error");
                    return Err(AgentError::Provider(e));
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
            .map(|(id, name, json_str)| {
                let input = serde_json::from_str(&json_str)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
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
            // 1.6: MaxOutputTokens auto-continue
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
        // 1.4: Apply micro-compact to large tool results
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

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use anyhow::Result;
    use async_trait::async_trait;
    use futures::Stream;
    use serde_json::json;

    use clawcr_provider::{
        ModelRequest, ModelResponse, ResponseContent, StopReason, StreamEvent, Usage,
    };
    use clawcr_safety::legacy_permissions::PermissionMode;
    use clawcr_tools::{Tool, ToolOrchestrator, ToolOutput, ToolRegistry};

    use super::query;
    use crate::{ContentBlock, Message, SessionConfig, SessionState};

    struct SingleToolUseProvider {
        requests: AtomicUsize,
    }

    #[async_trait]
    impl clawcr_provider::ModelProvider for SingleToolUseProvider {
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
            unreachable!("tests stream responses only")
        }

        async fn stream(
            &self,
            _request: ModelRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            let request_number = self.requests.fetch_add(1, Ordering::SeqCst);

            let events = if request_number == 0 {
                vec![
                    Ok(StreamEvent::ContentBlockStart {
                        index: 0,
                        content: ResponseContent::ToolUse {
                            id: "tool-1".into(),
                            name: "mutating_tool".into(),
                            input: json!({ "value": 1 }),
                        },
                    }),
                    Ok(StreamEvent::InputJsonDelta {
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
}
