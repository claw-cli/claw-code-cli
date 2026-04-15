use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use clawcr_protocol::{
    ModelRequest, ModelResponse, ProviderFamily, RequestContent, RequestMessage, ResponseContent,
    ResponseExtra, ResponseMetadata, StopReason, StreamEvent, Usage,
};
use futures::{Stream, StreamExt};
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::debug;

use super::AnthropicAIRole;
use crate::{ModelProviderSDK, ProviderAdapter, ProviderCapabilities, merge_extra_body};

/// <https://platform.claude.com/docs/en/api/messages>
/// Anthropic provider backed by the official HTTP API.
pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl AnthropicProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url.trim_end_matches('/'))
    }

    fn request_builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .post(self.endpoint())
            .header("anthropic-version", "2023-06-01")
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let builder = if let Some(api_key) = &self.api_key {
            builder.header("x-api-key", api_key)
        } else {
            builder
        };
        builder.json(body)
    }
}

#[derive(Debug, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    max_tokens: usize,
    stream: bool,
    messages: Vec<AnthropicInputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
}

#[derive(Debug, Serialize)]
struct AnthropicInputMessage {
    role: AnthropicAIRole,
    content: Vec<AnthropicInputContentBlock>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum AnthropicInputContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct AnthropicThinkingConfig {
    #[serde(rename = "type")]
    kind: &'static str,
    budget_tokens: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicMessageResponse {
    id: String,
    #[serde(default)]
    container: Option<AnthropicContainer>,
    #[serde(default)]
    content: Vec<AnthropicResponseContentBlock>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    stop_details: Option<AnthropicStopDetails>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicContainer {
    id: String,
    expires_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicStopDetails {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    explanation: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicUsage {
    input_tokens: usize,
    output_tokens: usize,
    #[serde(default)]
    cache_creation_input_tokens: Option<usize>,
    #[serde(default)]
    cache_read_input_tokens: Option<usize>,
    #[serde(default)]
    cache_creation: Option<AnthropicCacheCreation>,
    #[serde(default)]
    inference_geo: Option<String>,
    #[serde(default)]
    server_tool_use: Option<AnthropicServerToolUsage>,
    #[serde(default)]
    service_tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicCacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: Option<usize>,
    #[serde(default)]
    ephemeral_5m_input_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicServerToolUsage {
    #[serde(default)]
    web_fetch_requests: Option<usize>,
    #[serde(default)]
    web_search_requests: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicResponseContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    citations: Option<Value>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    caller: Option<Value>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[async_trait]
impl ModelProviderSDK for AnthropicProvider {
    async fn completion(&self, request: ModelRequest) -> Result<ModelResponse> {
        let body = build_request(&request, false);
        debug!(
            provider = "anthropic",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending anthropic completion request"
        );

        let response = self
            .request_builder(&body)
            .send()
            .await
            .context("failed to send anthropic request")?
            .error_for_status()
            .context("anthropic request failed")?;

        let value: Value = response
            .json()
            .await
            .context("failed to decode anthropic response")?;
        parse_response(value)
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let body = build_request(&request, true);
        debug!(
            provider = "anthropic",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending anthropic streaming request"
        );

        let event_source = EventSource::new(self.request_builder(&body))
            .context("failed to create anthropic event source")?;
        let stream = async_stream::try_stream! {
            let mut message_id = String::new();
            let mut input_tokens = 0usize;
            let mut output_tokens = 0usize;
            let mut stop_reason: Option<StopReason> = None;
            let mut content_blocks: BTreeMap<usize, ResponseContent> = BTreeMap::new();
            let mut reasoning_blocks: BTreeMap<usize, String> = BTreeMap::new();
            let mut tool_json: HashMap<usize, String> = HashMap::new();

            futures::pin_mut!(event_source);
            while let Some(event) = event_source.next().await {
                let event = event.map_err(|error| {
                    anyhow::anyhow!("anthropic stream error for model {}: {error}", request.model)
                })?;

                match event {
                    Event::Open => {}
                    Event::Message(message) => {
                        let data: Value = serde_json::from_str(&message.data)
                            .map_err(|error| anyhow::anyhow!("failed to parse anthropic stream payload: {error}"))?;

                        match message.event.as_str() {
                            "message_start" => {
                                if let Some(id) = data
                                    .get("message")
                                    .and_then(Value::as_object)
                                    .and_then(|message| message.get("id"))
                                    .and_then(Value::as_str)
                                {
                                    message_id = id.to_string();
                                }
                                if let Some(usage) = data.get("usage") {
                                    if let Some(input) =
                                        usage.get("input_tokens").and_then(Value::as_u64)
                                    {
                                        input_tokens = input as usize;
                                    }
                                }
                            }
                            "content_block_start" => {
                                let Some(index) = data.get("index").and_then(Value::as_u64) else {
                                    continue;
                                };
                                let Some(content_block) = data.get("content_block") else {
                                    continue;
                                };
                                let block: AnthropicResponseContentBlock =
                                    serde_json::from_value(content_block.clone()).map_err(|error| {
                                        anyhow::anyhow!(
                                            "failed to parse anthropic content block start: {error}"
                                        )
                                    })?;
                                match block.kind.as_str() {
                                    "text" => {
                                        content_blocks.insert(
                                            index as usize,
                                            ResponseContent::Text(String::new()),
                                        );
                                        yield StreamEvent::TextStart {
                                            index: index as usize,
                                        };
                                    }
                                    "tool_use" | "server_tool_use" => {
                                        let Some(id) = block.id.clone() else {
                                            continue;
                                        };
                                        let Some(name) = block.name.clone() else {
                                            continue;
                                        };
                                        let input = block
                                            .input
                                            .clone()
                                            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                                        content_blocks.insert(
                                            index as usize,
                                            ResponseContent::ToolUse {
                                                id: id.clone(),
                                                name: name.clone(),
                                                input: input.clone(),
                                            },
                                        );
                                        tool_json.insert(index as usize, String::new());
                                        yield StreamEvent::ToolCallStart {
                                            index: index as usize,
                                            id,
                                            name,
                                            input,
                                        };
                                    }
                                    "thinking" => {
                                        reasoning_blocks.insert(index as usize, String::new());
                                        yield StreamEvent::ReasoningStart {
                                            index: index as usize,
                                        };
                                    }
                                    _ => {}
                                };
                            }
                            "content_block_delta" => {
                                let Some(index) = data.get("index").and_then(Value::as_u64) else {
                                    continue;
                                };
                                let Some(delta) = data.get("delta").and_then(Value::as_object)
                                else {
                                    continue;
                                };
                                match delta.get("type").and_then(Value::as_str) {
                                    Some("text_delta") => {
                                        let text = delta
                                            .get("text")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default();
                                        if let Some(ResponseContent::Text(value)) =
                                            content_blocks.get_mut(&(index as usize))
                                        {
                                            value.push_str(text);
                                        }
                                        yield StreamEvent::TextDelta {
                                            index: index as usize,
                                            text: text.to_string(),
                                        };
                                    }
                                    Some("thinking_delta") => {
                                        let text = delta
                                            .get("thinking")
                                            .or_else(|| delta.get("text"))
                                            .and_then(Value::as_str)
                                            .unwrap_or_default();
                                        if let Some(value) = reasoning_blocks.get_mut(&(index as usize))
                                        {
                                            value.push_str(text);
                                        }
                                        yield StreamEvent::ReasoningDelta {
                                            index: index as usize,
                                            text: text.to_string(),
                                        };
                                    }
                                    Some("input_json_delta") => {
                                        let partial_json = delta
                                            .get("partial_json")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default();
                                        if let Some(acc) = tool_json.get_mut(&(index as usize)) {
                                            acc.push_str(partial_json);
                                        }
                                        yield StreamEvent::ToolCallInputDelta {
                                            index: index as usize,
                                            partial_json: partial_json.to_string(),
                                        };
                                    }
                                    _ => {}
                                }
                            }
                            "content_block_stop" => {
                                let index = data.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                                if let Some(json_str) = tool_json.remove(&index) {
                                    if let Ok(parsed) = serde_json::from_str(&json_str) {
                                        if let Some(ResponseContent::ToolUse { input, .. }) =
                                            content_blocks.get_mut(&index)
                                        {
                                            *input = parsed;
                                        }
                                    }
                                }
                            }
                            "message_delta" => {
                                if let Some(delta) = data.get("delta").and_then(Value::as_object) {
                                    if let Some(reason) =
                                        delta.get("stop_reason").and_then(Value::as_str)
                                    {
                                        stop_reason = Some(parse_stop_reason(reason));
                                    }
                                }
                                if let Some(usage) = data.get("usage") {
                                    if let Some(output) = usage.get("output_tokens").and_then(Value::as_u64)
                                    {
                                        output_tokens = output as usize;
                                    }
                                    yield StreamEvent::UsageDelta(Usage {
                                        input_tokens,
                                        output_tokens,
                                        cache_creation_input_tokens: None,
                                        cache_read_input_tokens: None,
                                    });
                                }
                            }
                            "message_stop" => {
                                let response = ModelResponse {
                                    id: message_id.clone(),
                                    content: content_blocks.into_values().collect(),
                                    stop_reason: stop_reason.clone(),
                                    usage: Usage {
                                        input_tokens,
                                        output_tokens,
                                        cache_creation_input_tokens: None,
                                        cache_read_input_tokens: None,
                                    },
                                    metadata: ResponseMetadata {
                                        extras: reasoning_blocks
                                            .values()
                                            .filter(|text| !text.is_empty())
                                            .cloned()
                                            .map(|text| ResponseExtra::ReasoningText { text })
                                            .collect(),
                                    },
                                };
                                yield StreamEvent::MessageDone { response };
                                return;
                            }
                            _ => {}
                        }
                    }
                }
            }

            let response = ModelResponse {
                id: message_id,
                content: content_blocks.into_values().collect(),
                stop_reason,
                usage: Usage {
                    input_tokens,
                    output_tokens,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                },
                metadata: ResponseMetadata {
                    extras: reasoning_blocks
                        .into_values()
                        .filter(|text| !text.is_empty())
                        .map(|text| ResponseExtra::ReasoningText { text })
                        .collect(),
                },
            };
            yield StreamEvent::MessageDone { response };
        };

        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::anthropic()
    }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities::anthropic()
    }
}

/// Here is the documentation of the Anthropic Messages API request body.
///
/// Official reference:
/// <https://platform.claude.com/docs/en/api/messages>
///
/// Create
/// - `POST /v1/messages`
///   Send a structured list of input messages with text and/or image content,
///   and the model will generate the next message in the conversation.
///   The Messages API can be used for either single queries or stateless
///   multi-turn conversations.
///
/// Body Parameters
/// - `max_tokens: number`
///   The maximum number of tokens to generate before stopping.
/// - `messages: array of MessageParam`
///   Input messages. Anthropic models operate on alternating `user` and
///   `assistant` turns, and consecutive turns with the same role are combined.
///   Each input message has:
///   - `role: "user" or "assistant"`
///   - `content: string or array of ContentBlockParam`
///     A string is shorthand for a single text block.
///     Supported content block families documented by Anthropic include:
///     - `TextBlockParam`
///     - `ImageBlockParam`
///     - `DocumentBlockParam`
///     - `SearchResultBlockParam`
///     - `ThinkingBlockParam`
///     - `RedactedThinkingBlockParam`
///     - `ToolUseBlockParam`
///     - `ToolResultBlockParam`
///     - `ServerToolUseBlockParam`
///     - `WebSearchToolResultBlockParam`
///     - `WebFetchToolResultBlockParam`
///     - `CodeExecutionToolResultBlockParam`
///     - `BashCodeExecutionToolResultBlockParam`
///     - `TextEditorCodeExecutionToolResultBlockParam`
///     - `ToolSearchToolResultBlockParam`
///     - `ContainerUploadBlockParam`
/// - `model: Model`
///   The model slug/id that will complete the prompt.
/// - `cache_control: optional CacheControlEphemeral`
///   Top-level cache control that automatically applies a cache marker to the
///   last cacheable block in the request.
/// - `container: optional string`
///   Container identifier for reuse across requests.
/// - `inference_geo: optional string`
///   Geographic region for inference processing.
/// - `metadata: optional Metadata`
///   Request metadata such as `user_id`.
/// - `output_config: optional OutputConfig`
///   Output configuration such as response format and effort.
/// - `service_tier: optional "auto" or "standard_only"`
///   Determines whether priority or standard capacity is used.
/// - `stop_sequences: optional array of string`
///   Custom sequences that stop generation.
/// - `stream: optional boolean`
///   Whether to stream the response using server-sent events.
/// - `system: optional string or array of TextBlockParam`
///   System prompt. Anthropic Messages uses a top-level `system` field rather
///   than a `"system"` message role.
/// - `temperature: optional number`
///   Amount of randomness injected into the response.
/// - `thinking: optional ThinkingConfigParam`
///   Extended-thinking configuration.
///   One of the following:
///   - `ThinkingConfigEnabled = object { budget_tokens, type, display }`
///   - `ThinkingConfigDisabled = object { type }`
///   - `ThinkingConfigAdaptive = object { type, display }`
/// - `tool_choice: optional ToolChoice`
///   Controls how the model uses available tools.
/// - `tools: optional array of ToolUnion`
///   Tool definitions that the model may use.
/// - `top_k: optional number`
///   Limits sampling to the top K options.
/// - `top_p: optional number`
///   Uses nucleus sampling.
///
/// Notes about this implementation:
/// - This builder currently emits the subset represented by the crate's
///   `ModelRequest`: `model`, `max_tokens`, `stream`, `messages`, optional
///   `system`, optional `tools`, optional `thinking`, and any merged
///   `extra_body`.
/// - Request message content is currently serialized from shared IR blocks into
///   Anthropic `text`, `tool_use`, and `tool_result` blocks.
/// - More advanced Anthropic request features documented above, such as image
///   blocks, document blocks, `tool_choice`, `output_config`, `cache_control`,
///   `metadata`, `service_tier`, `stop_sequences`, `top_k`, and `top_p`, are
///   not constructed directly here unless supplied through `extra_body`.
fn build_request(request: &ModelRequest, stream: bool) -> Value {
    let body = AnthropicMessagesRequest {
        model: request.model.clone(),
        max_tokens: request.max_tokens,
        stream,
        messages: request
            .messages
            .iter()
            .map(build_message)
            .collect::<Vec<_>>(),
        system: request.system.clone(),
        tools: request.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|tool| AnthropicToolDefinition {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                })
                .collect::<Vec<_>>()
        }),
        thinking: request.thinking.as_deref().and_then(build_thinking),
        temperature: request.sampling.temperature,
        top_p: request.sampling.top_p,
        top_k: request.sampling.top_k,
    };
    let mut root =
        serde_json::to_value(body).expect("anthropic request body serialization should succeed");

    merge_extra_body(&mut root, request.extra_body.as_ref());

    root
}

/// Here is the documentation of the Anthropic Messages API response body.
///
/// Returns
/// - `Message = object`
///   Generated assistant message returned by the Messages API.
/// - `id: string`
///   Unique object identifier.
/// - `container: Container`
///   Information about the container used in the request.
///   - `id: string`
///   - `expires_at: string`
/// - `content: array of ContentBlock`
///   Content generated by the model.
///   Anthropic documents a large union of possible response block types,
///   including:
///   - `TextBlock`
///   - `ThinkingBlock`
///   - `RedactedThinkingBlock`
///   - `ToolUseBlock`
///   - `ServerToolUseBlock`
///   - `WebSearchToolResultBlock`
///   - `WebFetchToolResultBlock`
///   - `CodeExecutionToolResultBlock`
///   - `BashCodeExecutionToolResultBlock`
///   - `TextEditorCodeExecutionToolResultBlock`
///   - `ToolSearchToolResultBlock`
///   - `ContainerUploadBlock`
/// - `model: Model`
///   The model that completed the prompt.
/// - `role: "assistant"`
///   The generated message role.
/// - `stop_details: RefusalStopDetails`
///   Structured refusal information when applicable.
/// - `stop_reason: StopReason`
///   One of:
///   - `"end_turn"`
///   - `"max_tokens"`
///   - `"stop_sequence"`
///   - `"tool_use"`
///   - `"pause_turn"`
///   - `"refusal"`
/// - `stop_sequence: string`
///   The matched custom stop sequence, if any.
/// - `type: "message"`
///   Object type for Messages API responses.
/// - `usage: Usage`
///   Billing and rate-limit usage.
///   Includes fields such as:
///   - `input_tokens`
///   - `output_tokens`
///   - `cache_creation_input_tokens`
///   - `cache_read_input_tokens`
///   - `cache_creation`
///   - `inference_geo`
///   - `server_tool_use`
///   - `service_tier`
///
/// Notes about this implementation:
/// - `parse_response` currently reads `id`, `content`, `stop_reason`, and
///   `usage`.
/// - Response content is currently mapped only for Anthropic `text` and
///   `tool_use` blocks.
/// - Other documented response fields such as `container`, `model`, `role`,
///   `stop_details`, `stop_sequence`, `type`, server-tool result blocks,
///   thinking blocks, citations, and the richer usage breakdown are not
///   currently projected into `ModelResponse`.
/// -------------------------- Below is an example -------------------------
/// ```json
/// {
///  "id": "msg_013Zva2CMHLNnXjNJJKqJ2EF",
///  "container": {
///    "id": "id",
///    "expires_at": "2019-12-27T18:11:19.117Z"
///  },
///  "content": [
///    {
///      "citations": [
///        {
///          "cited_text": "cited_text",
///          "document_index": 0,
///          "document_title": "document_title",
///          "end_char_index": 0,
///          "file_id": "file_id",
///          "start_char_index": 0,
///          "type": "char_location"
///        }
///      ],
///      "text": "Hi! My name is Claude.",
///      "type": "text"
///    }
///  ],
///  "model": "claude-opus-4-6",
///  "role": "assistant",
///  "stop_details": {
///    "category": "cyber",
///    "explanation": "explanation",
///    "type": "refusal"
///  },
///  "stop_reason": "end_turn",
///  "stop_sequence": null,
///  "type": "message",
///  "usage": {
///    "cache_creation": {
///      "ephemeral_1h_input_tokens": 0,
///      "ephemeral_5m_input_tokens": 0
///    },
///    "cache_creation_input_tokens": 2051,
///    "cache_read_input_tokens": 2051,
///    "inference_geo": "inference_geo",
///    "input_tokens": 2095,
///    "output_tokens": 503,
///    "server_tool_use": {
///      "web_fetch_requests": 2,
///      "web_search_requests": 0
///    },
///    "service_tier": "standard"
///  }
///}
/// ```
fn parse_response(value: Value) -> Result<ModelResponse> {
    let response: AnthropicMessageResponse = serde_json::from_value(value.clone())
        .context("failed to deserialize anthropic messages response")?;
    let mut content = Vec::new();
    let mut metadata = ResponseMetadata::default();

    for block in &response.content {
        if let Some(parsed) = parse_response_content_block(block, &mut metadata) {
            content.push(parsed);
        }
    }
    let stop_reason = response.stop_reason.as_deref().map(parse_stop_reason);
    let usage = response.usage.as_ref().map(map_usage).unwrap_or_default();

    if let Some(provider_payload) = build_provider_specific_response_payload(&response) {
        metadata.extras.push(ResponseExtra::ProviderSpecific {
            provider: "anthropic".to_string(),
            payload: provider_payload,
        });
    }

    Ok(ModelResponse {
        id: response.id,
        content,
        stop_reason,
        usage,
        metadata,
    })
}

fn build_message(message: &RequestMessage) -> AnthropicInputMessage {
    let role = message
        .role
        .parse::<AnthropicAIRole>()
        .unwrap_or(AnthropicAIRole::User);
    let content = message
        .content
        .iter()
        .map(build_content_block)
        .collect::<Vec<_>>();

    AnthropicInputMessage { role, content }
}

fn build_content_block(block: &RequestContent) -> AnthropicInputContentBlock {
    match block {
        RequestContent::Text { text } => AnthropicInputContentBlock::Text { text: text.clone() },
        RequestContent::ToolUse { id, name, input } => AnthropicInputContentBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        RequestContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => AnthropicInputContentBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
            is_error: *is_error,
        },
    }
}

fn parse_response_content_block(
    block: &AnthropicResponseContentBlock,
    metadata: &mut ResponseMetadata,
) -> Option<ResponseContent> {
    match block.kind.as_str() {
        "text" => Some(ResponseContent::Text(
            block.text.clone().unwrap_or_default(),
        )),
        "tool_use" | "server_tool_use" => Some(ResponseContent::ToolUse {
            id: block.id.clone()?,
            name: block.name.clone()?,
            input: block
                .input
                .clone()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
        }),
        "thinking" => {
            if let Some(thinking) = &block.thinking
                && !thinking.is_empty()
            {
                metadata.extras.push(ResponseExtra::ReasoningText {
                    text: thinking.clone(),
                });
            }
            None
        }
        _ => None,
    }
}

fn map_usage(usage: &AnthropicUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
    }
}

fn build_provider_specific_response_payload(response: &AnthropicMessageResponse) -> Option<Value> {
    let mut payload = serde_json::Map::new();

    if let Some(container) = &response.container {
        payload.insert("container".to_string(), json!(container));
    }
    if !response.content.is_empty() {
        payload.insert("content".to_string(), json!(response.content));
    }
    if let Some(model) = &response.model {
        payload.insert("model".to_string(), json!(model));
    }
    if let Some(role) = &response.role {
        payload.insert("role".to_string(), json!(role));
    }
    if let Some(stop_details) = &response.stop_details {
        payload.insert("stop_details".to_string(), json!(stop_details));
    }
    if let Some(stop_sequence) = &response.stop_sequence {
        payload.insert("stop_sequence".to_string(), json!(stop_sequence));
    }
    if let Some(kind) = &response.kind {
        payload.insert("type".to_string(), json!(kind));
    }
    if let Some(usage) = &response.usage {
        payload.insert("usage".to_string(), json!(usage));
    }

    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

fn parse_stop_reason(value: &str) -> StopReason {
    match value {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}

fn build_thinking(level: &str) -> Option<AnthropicThinkingConfig> {
    let budget_tokens = match level.trim().to_ascii_lowercase().as_str() {
        "disabled" => return None,
        "enabled" | "medium" => 4_096,
        "low" => 1_024,
        "high" => 8_192,
        "xhigh" => 16_384,
        _ => 4_096,
    };

    Some(AnthropicThinkingConfig {
        kind: "enabled",
        budget_tokens,
    })
}

#[cfg(test)]
mod tests {
    use clawcr_protocol::{
        ModelRequest, RequestContent, RequestMessage, SamplingControls, ToolDefinition,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::{build_request, parse_response, parse_stop_reason};
    use clawcr_protocol::{ResponseContent, ResponseExtra, StopReason};

    #[test]
    fn build_request_includes_sampling_tools_and_thinking() {
        let request = ModelRequest {
            model: "claude-sonnet-4-6".to_string(),
            system: Some("You are helpful.".to_string()),
            messages: vec![
                RequestMessage {
                    role: "assistant".to_string(),
                    content: vec![
                        RequestContent::Text {
                            text: "Calling tool".to_string(),
                        },
                        RequestContent::ToolUse {
                            id: "toolu_123".to_string(),
                            name: "get_weather".to_string(),
                            input: json!({"city": "Boston"}),
                        },
                    ],
                },
                RequestMessage {
                    role: "user".to_string(),
                    content: vec![RequestContent::ToolResult {
                        tool_use_id: "toolu_123".to_string(),
                        content: "{\"temp\":72}".to_string(),
                        is_error: Some(false),
                    }],
                },
            ],
            max_tokens: 1024,
            tools: Some(vec![ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather by city".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            }]),
            sampling: SamplingControls {
                temperature: Some(0.2),
                top_p: Some(0.9),
                top_k: Some(32),
            },
            thinking: Some("medium".to_string()),
            extra_body: None,
        };

        let body = build_request(&request, true);

        assert_eq!(body["model"], json!("claude-sonnet-4-6"));
        assert_eq!(body["max_tokens"], json!(1024));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["system"], json!("You are helpful."));
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["top_p"], json!(0.9));
        assert_eq!(body["top_k"], json!(32));
        assert_eq!(body["thinking"]["type"], json!("enabled"));
        assert_eq!(body["thinking"]["budget_tokens"], json!(4096));
        assert_eq!(body["messages"][0]["role"], json!("assistant"));
        assert_eq!(body["messages"][0]["content"][1]["type"], json!("tool_use"));
        assert_eq!(
            body["messages"][1]["content"][0]["type"],
            json!("tool_result")
        );
        assert_eq!(body["tools"][0]["name"], json!("get_weather"));
    }

    #[test]
    fn parse_response_extracts_text_tool_use_reasoning_and_usage() {
        let response = parse_response(json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [
                {
                    "type": "thinking",
                    "thinking": "Need to call the weather tool first.",
                    "signature": "sig_123"
                },
                {
                    "type": "text",
                    "text": "Let me check that."
                },
                {
                    "type": "server_tool_use",
                    "id": "srvtool_1",
                    "name": "web_search",
                    "input": { "query": "Boston weather" },
                    "caller": { "type": "direct" }
                }
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7,
                "cache_creation_input_tokens": 3,
                "cache_read_input_tokens": 5,
                "service_tier": "standard",
                "inference_geo": "us"
            }
        }))
        .expect("parse response");

        assert_eq!(response.id, "msg_123");
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(response.usage.input_tokens, 11);
        assert_eq!(response.usage.output_tokens, 7);
        assert_eq!(response.usage.cache_creation_input_tokens, Some(3));
        assert_eq!(response.usage.cache_read_input_tokens, Some(5));
        assert_eq!(response.content.len(), 2);
        match &response.content[0] {
            ResponseContent::Text(text) => {
                assert_eq!(text, "Let me check that.");
            }
            other => panic!("expected text block, got {other:?}"),
        }
        match &response.content[1] {
            ResponseContent::ToolUse { id, name, input } => {
                assert_eq!(id, "srvtool_1");
                assert_eq!(name, "web_search");
                assert_eq!(input, &json!({"query": "Boston weather"}));
            }
            other => panic!("expected tool use block, got {other:?}"),
        }
        assert!(response.metadata.extras.iter().any(|extra| matches!(
            extra,
            ResponseExtra::ReasoningText { text }
            if text == "Need to call the weather tool first."
        )));
        assert!(response.metadata.extras.iter().any(|extra| matches!(
            extra,
            ResponseExtra::ProviderSpecific { provider, .. } if provider == "anthropic"
        )));
    }

    #[test]
    fn parse_stop_reason_matches_messages_contract() {
        assert_eq!(parse_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(parse_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(parse_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(parse_stop_reason("stop_sequence"), StopReason::StopSequence);
        assert_eq!(parse_stop_reason("pause_turn"), StopReason::EndTurn);
        assert_eq!(parse_stop_reason("refusal"), StopReason::EndTurn);
    }
}
