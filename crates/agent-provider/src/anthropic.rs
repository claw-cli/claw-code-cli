use std::pin::Pin;

use async_anthropic::{
    types::{
        ContentBlockDelta, CreateMessagesRequestBuilder, MessageBuilder, MessageContent,
        MessageRole, MessagesStreamEvent, ToolResult, ToolUse,
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

/// Anthropic provider backed by the `async-anthropic` crate.
pub struct AnthropicProvider {
    client: Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::from_api_key(api_key.into()),
        }
    }

    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        let client = Client::builder()
            .base_url(base_url.into())
            .build()
            .expect("failed to build Anthropic client");
        Self { client }
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn complete(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
        let req = build_request(&request, false)?;
        debug!(model = %request.model, "anthropic complete");
        let resp = self
            .client
            .messages()
            .create(req)
            .await
            .map_err(|e| anyhow::anyhow!("Anthropic API error: {e}"))?;

        let content = resp
            .content
            .unwrap_or_default()
            .into_iter()
            .filter_map(map_content_block)
            .collect();

        let stop_reason = resp.stop_reason.as_deref().map(parse_stop_reason);
        let usage = resp
            .usage
            .map(|u| Usage {
                input_tokens: u.input_tokens.unwrap_or(0) as usize,
                output_tokens: u.output_tokens.unwrap_or(0) as usize,
                ..Default::default()
            })
            .unwrap_or_default();

        Ok(ModelResponse {
            id: resp.id.unwrap_or_default(),
            content,
            stop_reason,
            usage,
        })
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let req = build_request(&request, true)?;
        debug!(model = %request.model, "anthropic stream");

        let mut sdk_stream = self.client.messages().create_stream(req).await;

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(64);

        tokio::spawn(async move {
            let mut message_id = String::new();
            let mut input_tokens = 0usize;
            let mut output_tokens = 0usize;
            let mut stop_reason: Option<StopReason> = None;
            let mut content_blocks: Vec<ResponseContent> = Vec::new();
            let mut tool_json: std::collections::HashMap<usize, String> =
                std::collections::HashMap::new();

            while let Some(event) = sdk_stream.next().await {
                let evt = match event {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await;
                        return;
                    }
                };

                match evt {
                    MessagesStreamEvent::MessageStart { message, usage } => {
                        message_id = message.id;
                        if let Some(u) = usage {
                            input_tokens = u.input_tokens.unwrap_or(0) as usize;
                        }
                    }
                    MessagesStreamEvent::ContentBlockStart {
                        index,
                        content_block,
                    } => {
                        let rc = match &content_block {
                            MessageContent::Text(_) => ResponseContent::Text(String::new()),
                            MessageContent::ToolUse(tu) => {
                                tool_json.insert(index, String::new());
                                ResponseContent::ToolUse {
                                    id: tu.id.clone(),
                                    name: tu.name.clone(),
                                    input: serde_json::Value::Object(serde_json::Map::new()),
                                }
                            }
                            _ => continue,
                        };
                        while content_blocks.len() <= index {
                            content_blocks.push(ResponseContent::Text(String::new()));
                        }
                        content_blocks[index] = rc.clone();
                        let _ = tx
                            .send(Ok(StreamEvent::ContentBlockStart {
                                index,
                                content: rc,
                            }))
                            .await;
                    }
                    MessagesStreamEvent::ContentBlockDelta { index, delta } => match delta {
                        ContentBlockDelta::TextDelta { text } => {
                            if let Some(ResponseContent::Text(ref mut t)) =
                                content_blocks.get_mut(index)
                            {
                                t.push_str(&text);
                            }
                            let _ = tx
                                .send(Ok(StreamEvent::TextDelta {
                                    index,
                                    text: text.clone(),
                                }))
                                .await;
                        }
                        ContentBlockDelta::InputJsonDelta { partial_json } => {
                            if let Some(acc) = tool_json.get_mut(&index) {
                                acc.push_str(&partial_json);
                            }
                            let _ = tx
                                .send(Ok(StreamEvent::InputJsonDelta {
                                    index,
                                    partial_json: partial_json.clone(),
                                }))
                                .await;
                        }
                    },
                    MessagesStreamEvent::ContentBlockStop { index } => {
                        if let Some(json_str) = tool_json.remove(&index) {
                            if let Ok(parsed) = serde_json::from_str(&json_str) {
                                if let Some(ResponseContent::ToolUse {
                                    ref mut input, ..
                                }) = content_blocks.get_mut(index)
                                {
                                    *input = parsed;
                                }
                            }
                        }
                        let _ = tx
                            .send(Ok(StreamEvent::ContentBlockStop { index }))
                            .await;
                    }
                    MessagesStreamEvent::MessageDelta { delta, usage } => {
                        stop_reason = delta.stop_reason.as_deref().map(parse_stop_reason);
                        if let Some(u) = usage {
                            output_tokens = u.output_tokens.unwrap_or(0) as usize;
                        }
                    }
                    MessagesStreamEvent::MessageStop => {
                        let response = ModelResponse {
                            id: message_id.clone(),
                            content: content_blocks.clone(),
                            stop_reason: stop_reason.clone(),
                            usage: Usage {
                                input_tokens,
                                output_tokens,
                                ..Default::default()
                            },
                        };
                        let _ = tx.send(Ok(StreamEvent::MessageDone { response })).await;
                    }
                }
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

fn build_request(
    request: &ModelRequest,
    stream: bool,
) -> anyhow::Result<async_anthropic::types::CreateMessagesRequest> {
    let mut messages = Vec::new();

    for msg in &request.messages {
        let role = match msg.role.as_str() {
            "assistant" => MessageRole::Assistant,
            _ => MessageRole::User,
        };

        let mut content: Vec<MessageContent> = Vec::new();
        for block in &msg.content {
            match block {
                RequestContent::Text { text } => {
                    content.push(MessageContent::Text(async_anthropic::types::Text {
                        text: text.clone(),
                    }));
                }
                RequestContent::ToolUse { id, name, input } => {
                    content.push(MessageContent::ToolUse(ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }));
                }
                RequestContent::ToolResult {
                    tool_use_id,
                    content: result_content,
                    is_error,
                } => {
                    content.push(MessageContent::ToolResult(ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: Some(result_content.clone()),
                        is_error: is_error.unwrap_or(false),
                    }));
                }
            }
        }

        let sdk_msg = MessageBuilder::default()
            .role(role)
            .content(async_anthropic::types::MessageContentList(content))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build message: {e}"))?;

        messages.push(sdk_msg);
    }

    let mut builder = CreateMessagesRequestBuilder::default();
    builder
        .model(request.model.clone())
        .messages(messages)
        .max_tokens(request.max_tokens as i32)
        .stream(stream);

    if let Some(ref system) = request.system {
        builder.system(system.clone());
    }

    if let Some(ref tools) = request.tools {
        let sdk_tools: Vec<serde_json::Map<String, serde_json::Value>> = tools
            .iter()
            .map(|t| {
                let mut m = serde_json::Map::new();
                m.insert("name".into(), serde_json::json!(t.name));
                m.insert("description".into(), serde_json::json!(t.description));
                m.insert("input_schema".into(), t.input_schema.clone());
                m
            })
            .collect();
        builder.tools(sdk_tools);
    }

    builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build request: {e}"))
}

fn map_content_block(block: MessageContent) -> Option<ResponseContent> {
    match block {
        MessageContent::Text(t) => Some(ResponseContent::Text(t.text)),
        MessageContent::ToolUse(tu) => Some(ResponseContent::ToolUse {
            id: tu.id,
            name: tu.name,
            input: tu.input,
        }),
        _ => None,
    }
}

fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}
