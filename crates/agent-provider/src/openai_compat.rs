use std::pin::Pin;

use async_openai::{
    config::{Config, OpenAIConfig},
    types::chat::{
        ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessage, ChatCompletionRequestToolMessage,
        ChatCompletionRequestUserMessage, ChatCompletionStreamResponseDelta,
        ChatCompletionTool, ChatCompletionTools, CreateChatCompletionRequest,
        CreateChatCompletionRequestArgs, FinishReason, FunctionCall, FunctionObject,
    },
    Client,
};
use async_trait::async_trait;
use futures::Stream;
use tokio_stream::StreamExt as _;
use tracing::debug;

use crate::{
    ModelProvider, ModelRequest, ModelResponse, RequestContent, ResponseContent, StopReason,
    StreamEvent, Usage,
};

/// OpenAI-compatible provider backed by the `async-openai` crate.
///
/// Works with OpenAI, Ollama, vLLM, LM Studio, and any other
/// OpenAI-format API by setting a custom base URL.
pub struct OpenAICompatProvider {
    client: Client<OpenAIConfig>,
}

impl OpenAICompatProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        let config = OpenAIConfig::new().with_api_base(base_url.into());
        Self {
            client: Client::with_config(config),
        }
    }

    pub fn with_api_key(self, api_key: impl Into<String>) -> Self {
        let existing_base = self.client.config().api_base().to_string();
        let config = OpenAIConfig::new()
            .with_api_base(existing_base)
            .with_api_key(api_key.into());
        Self {
            client: Client::with_config(config),
        }
    }
}

#[async_trait]
impl ModelProvider for OpenAICompatProvider {
    async fn complete(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
        let req = build_request(&request)?;
        debug!(model = %request.model, "openai-compat complete");

        let resp = self
            .client
            .chat()
            .create(req)
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI-compat API error: {e}"))?;

        let choice = resp.choices.into_iter().next();
        let mut content = Vec::new();

        if let Some(choice) = choice {
            if let Some(text) = choice.message.content {
                if !text.is_empty() {
                    content.push(ResponseContent::Text(text));
                }
            }
            if let Some(tool_calls) = choice.message.tool_calls {
                for tc in tool_calls {
                    if let ChatCompletionMessageToolCalls::Function(call) = tc {
                        let input = serde_json::from_str(&call.function.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        content.push(ResponseContent::ToolUse {
                            id: call.id,
                            name: call.function.name,
                            input,
                        });
                    }
                }
            }
        }

        let usage = resp
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens as usize,
                output_tokens: u.completion_tokens as usize,
                ..Default::default()
            })
            .unwrap_or_default();

        Ok(ModelResponse {
            id: resp.id,
            content,
            stop_reason: Some(StopReason::EndTurn),
            usage,
        })
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let req = build_request(&request)?;
        debug!(model = %request.model, "openai-compat stream");

        let mut sdk_stream = self
            .client
            .chat()
            .create_stream(req)
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI-compat stream error: {e}"))?;

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(64);

        tokio::spawn(async move {
            let mut response_id = String::new();
            let mut text_buf = String::new();
            // index → (id, name, args_accum)
            let mut tool_calls: std::collections::HashMap<u32, (String, String, String)> =
                std::collections::HashMap::new();
            let mut text_block_started = false;
            let mut tool_blocks_started: std::collections::HashSet<u32> =
                std::collections::HashSet::new();
            let mut finish_reason: Option<StopReason> = None;

            while let Some(chunk) = sdk_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await;
                        return;
                    }
                };

                if response_id.is_empty() {
                    response_id = chunk.id.clone();
                }

                for choice in chunk.choices {
                    let delta: ChatCompletionStreamResponseDelta = choice.delta;

                    // Text content
                    if let Some(content) = delta.content {
                        if !content.is_empty() {
                            if !text_block_started {
                                text_block_started = true;
                                let _ = tx
                                    .send(Ok(StreamEvent::ContentBlockStart {
                                        index: 0,
                                        content: ResponseContent::Text(String::new()),
                                    }))
                                    .await;
                            }
                            text_buf.push_str(&content);
                            let _ = tx
                                .send(Ok(StreamEvent::TextDelta {
                                    index: 0,
                                    text: content,
                                }))
                                .await;
                        }
                    }

                    // Tool calls
                    if let Some(tcs) = delta.tool_calls {
                        for tc in tcs {
                            let idx = tc.index;
                            let content_idx = (idx + 1) as usize;
                            let entry = tool_calls
                                .entry(idx)
                                .or_insert_with(|| (String::new(), String::new(), String::new()));

                            if let Some(id) = tc.id {
                                entry.0 = id;
                            }
                            if let Some(func) = tc.function {
                                if let Some(name) = func.name {
                                    entry.1 = name;
                                }
                                if let Some(args) = func.arguments.map(|s| s.to_string()) {
                                    entry.2.push_str(&args);

                                    if !tool_blocks_started.contains(&idx) {
                                        tool_blocks_started.insert(idx);
                                        let _ = tx
                                            .send(Ok(StreamEvent::ContentBlockStart {
                                                index: content_idx,
                                                content: ResponseContent::ToolUse {
                                                    id: entry.0.clone(),
                                                    name: entry.1.clone(),
                                                    input: serde_json::Value::Object(
                                                        serde_json::Map::new(),
                                                    ),
                                                },
                                            }))
                                            .await;
                                    }

                                    if !args.is_empty() {
                                        let _ = tx
                                            .send(Ok(StreamEvent::InputJsonDelta {
                                                index: content_idx,
                                                partial_json: args,
                                            }))
                                            .await;
                                    }
                                }
                            }
                        }
                    }

                    if let Some(reason) = choice.finish_reason {
                        finish_reason = Some(match reason {
                            FinishReason::Stop => StopReason::EndTurn,
                            FinishReason::ToolCalls => StopReason::ToolUse,
                            FinishReason::Length => StopReason::MaxTokens,
                            _ => StopReason::EndTurn,
                        });
                    }
                }
            }

            // Build final content list
            let mut content = Vec::new();
            if !text_buf.is_empty() {
                content.push(ResponseContent::Text(text_buf));
            }
            let mut sorted: Vec<_> = tool_calls.iter().collect();
            sorted.sort_by_key(|(idx, _)| *idx);
            for (_, (id, name, args)) in sorted {
                let input = serde_json::from_str(args)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                content.push(ResponseContent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input,
                });
            }

            let response = ModelResponse {
                id: response_id,
                content,
                stop_reason: finish_reason,
                usage: Usage::default(),
            };
            let _ = tx.send(Ok(StreamEvent::MessageDone { response })).await;
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai-compat"
    }
}

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

fn build_request(request: &ModelRequest) -> anyhow::Result<CreateChatCompletionRequest> {
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();

    if let Some(ref system) = request.system {
        messages.push(ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: system.clone().into(),
                name: None,
            },
        ));
    }

    for msg in &request.messages {
        match msg.role.as_str() {
            "assistant" => {
                let mut text_parts = Vec::new();
                let mut tool_calls: Vec<ChatCompletionMessageToolCalls> = Vec::new();

                for block in &msg.content {
                    match block {
                        RequestContent::Text { text } => text_parts.push(text.clone()),
                        RequestContent::ToolUse { id, name, input } => {
                            tool_calls.push(ChatCompletionMessageToolCalls::Function(
                                async_openai::types::chat::ChatCompletionMessageToolCall {
                                    id: id.clone(),
                                    function: FunctionCall {
                                        name: name.clone(),
                                        arguments: serde_json::to_string(input)
                                            .unwrap_or_default(),
                                    },
                                },
                            ));
                        }
                        _ => {}
                    }
                }

                let content = if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join("").into())
                };

                messages.push(ChatCompletionRequestMessage::Assistant(
                    ChatCompletionRequestAssistantMessage {
                        content,
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        ..Default::default()
                    },
                ));
            }
            _ => {
                // user role — may contain tool_result blocks
                let mut text_parts = Vec::new();
                let mut tool_results: Vec<(String, String)> = Vec::new();

                for block in &msg.content {
                    match block {
                        RequestContent::Text { text } => text_parts.push(text.clone()),
                        RequestContent::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            tool_results.push((tool_use_id.clone(), content.clone()));
                        }
                        _ => {}
                    }
                }

                // Tool results become separate "tool" role messages
                for (tool_use_id, content) in tool_results {
                    messages.push(ChatCompletionRequestMessage::Tool(
                        ChatCompletionRequestToolMessage {
                            content: content.into(),
                            tool_call_id: tool_use_id,
                        },
                    ));
                }

                if !text_parts.is_empty() {
                    messages.push(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessage {
                            content: text_parts.join("").into(),
                            name: None,
                        },
                    ));
                }
            }
        }
    }

    let mut builder = CreateChatCompletionRequestArgs::default();
    builder
        .model(request.model.clone())
        .messages(messages)
        .max_tokens(request.max_tokens as u32);

    if let Some(temp) = request.temperature {
        builder.temperature(temp as f32);
    }

    if let Some(ref tools) = request.tools {
        let sdk_tools: Vec<ChatCompletionTools> = tools
            .iter()
            .map(|t| {
                ChatCompletionTools::Function(ChatCompletionTool {
                    function: FunctionObject {
                        name: t.name.clone(),
                        description: Some(t.description.clone()),
                        parameters: Some(t.input_schema.clone()),
                        strict: None,
                    },
                })
            })
            .collect();
        builder.tools(sdk_tools);
    }

    builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build request: {e}"))
}
