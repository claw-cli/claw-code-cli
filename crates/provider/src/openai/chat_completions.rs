use std::pin::Pin;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::debug;
mod stream;
use clawcr_protocol::{
    ModelRequest, ModelResponse, ProviderFamily, RequestContent, ResponseContent, ResponseExtra,
    ResponseMetadata, StopReason, StreamEvent, Usage,
};

use super::capabilities::{OpenAIReasoningMode, OpenAITransport, resolve_request_profile};
use super::shared::{reasoning_value, request_role, tool_definitions};
use crate::text_normalization::split_tagged_text;
use crate::{ModelProviderSDK, ProviderAdapter, ProviderCapabilities, merge_extra_body};

/// OpenAI chat-completion provider backed by the official HTTP API.
/// <https://developers.openai.com/api/reference/chat-completions/overview>
/// Works with OpenAI chat-completion servers by changing the base URL.
pub struct OpenAIProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl OpenAIProvider {
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
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn request_builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .post(self.endpoint())
            .header(CONTENT_TYPE, "application/json");
        let builder = if let Some(api_key) = &self.api_key {
            builder.header(AUTHORIZATION, format!("Bearer {api_key}"))
        } else {
            builder
        };
        builder.json(body)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionResponse {
    id: String,
    #[serde(default)]
    choices: Vec<OpenAIChatCompletionChoice>,
    #[serde(default)]
    created: Option<u64>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    system_fingerprint: Option<String>,
    #[serde(default)]
    usage: Option<OpenAICompletionUsage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    index: Option<u32>,
    #[serde(default)]
    logprobs: Option<OpenAIChoiceLogprobs>,
    #[serde(default)]
    message: Option<OpenAIChatCompletionMessage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChoiceLogprobs {
    #[serde(default)]
    content: Vec<OpenAIChatCompletionTokenLogprob>,
    #[serde(default)]
    refusal: Vec<OpenAIChatCompletionTokenLogprob>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionTokenLogprob {
    token: String,
    #[serde(default)]
    bytes: Option<Vec<u8>>,
    logprob: f64,
    #[serde(default)]
    top_logprobs: Vec<OpenAIChatCompletionTopLogprob>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionTopLogprob {
    token: String,
    #[serde(default)]
    bytes: Option<Vec<u8>>,
    logprob: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    annotations: Vec<OpenAIChatCompletionAnnotation>,
    #[serde(default)]
    audio: Option<OpenAIChatCompletionAudio>,
    #[serde(default)]
    function_call: Option<OpenAIChatCompletionFunctionCall>,
    #[serde(default)]
    tool_calls: Vec<OpenAIChatCompletionMessageToolCall>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionAnnotation {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    url_citation: Option<OpenAIChatCompletionUrlCitation>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionUrlCitation {
    #[serde(default)]
    end_index: Option<u64>,
    #[serde(default)]
    start_index: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionAudio {
    id: String,
    data: String,
    expires_at: u64,
    transcript: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionFunctionCall {
    arguments: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionMessageToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    function: Option<OpenAIChatCompletionFunctionCall>,
    #[serde(default)]
    custom: Option<OpenAIChatCompletionCustomToolCall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIChatCompletionCustomToolCall {
    input: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAICompletionUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    #[serde(default)]
    total_tokens: Option<usize>,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAIPromptTokenDetails>,
    #[serde(default)]
    completion_tokens_details: Option<OpenAICompletionTokenDetails>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAIPromptTokenDetails {
    #[serde(default)]
    audio_tokens: Option<usize>,
    #[serde(default)]
    cached_tokens: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct OpenAICompletionTokenDetails {
    #[serde(default)]
    accepted_prediction_tokens: Option<usize>,
    #[serde(default)]
    audio_tokens: Option<usize>,
    #[serde(default)]
    reasoning_tokens: Option<usize>,
    #[serde(default)]
    rejected_prediction_tokens: Option<usize>,
}

/// Here is the documentation of the ChatCompletion request body.
///
/// Official reference:
/// <https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create>
///
/// Body Parameters
/// - messages: array of `ChatCompletionMessageParam`
///   A list of messages comprising the conversation so far. Depending on the
///   model, different content modalities may be supported, including text,
///   images, audio, files, tool calls, and tool results.
///   One of the following:
///   - `ChatCompletionDeveloperMessageParam = object`
///     Developer-provided instructions that the model should follow regardless
///     of messages sent by the user. With `o1` models and newer, developer
///     messages replace previous system messages.
///     - content: string or array of `ChatCompletionContentPartText`
///       The contents of the developer message.
///       One of the following:
///       - `TextContent = string`
///         The contents of the developer message.
///       - `ArrayOfContentParts = array of ChatCompletionContentPartText`
///         An array of content parts with a defined type. For developer
///         messages, only `text` parts are supported.
///         - text: string
///           The text content.
///         - type: `"text"`
///           The type of the content part.
///     - role: `"developer"`
///       The role of the message author.
///     - name: optional string
///       Optional participant name used to differentiate participants of the
///       same role.
///   - `ChatCompletionSystemMessageParam = object`
///     System-level instructions the model should follow regardless of user
///     messages. For newer reasoning models, developer messages are preferred.
///     - content: string or array of `ChatCompletionContentPartText`
///       The contents of the system message.
///       One of the following:
///       - `TextContent = string`
///       - `ArrayOfContentParts = array of ChatCompletionContentPartText`
///         Only `text` parts are supported.
///         - text: string
///         - type: `"text"`
///     - role: `"system"`
///     - name: optional string
///   - `ChatCompletionUserMessageParam = object`
///     Messages sent by an end user, containing prompts or additional context.
///     - content: string or array of `ChatCompletionContentPart`
///       The contents of the user message.
///       One of the following:
///       - `TextContent = string`
///         The text contents of the message.
///       - `ArrayOfContentParts = array of ChatCompletionContentPart`
///         Supported options differ by model and may include text, image,
///         audio, or file inputs.
///         One of the following:
///         - `ChatCompletionContentPartText = object`
///           - text: string
///           - type: `"text"`
///         - `ChatCompletionContentPartImage = object`
///           Learn about image inputs.
///           - image_url: object
///             - url: string
///               Either an image URL or base64-encoded image data.
///             - detail: optional `"auto"` or `"low"` or `"high"`
///               Specifies the image detail level.
///           - type: `"image_url"`
///         - `ChatCompletionContentPartInputAudio = object`
///           Learn about audio inputs.
///           - input_audio: object
///             - data: string
///               Base64 encoded audio data.
///             - format: `"wav"` or `"mp3"`
///           - type: `"input_audio"`
///         - `FileContentPart = object`
///           Learn about file inputs for text generation.
///           - file: object
///             - file_data: optional string
///             - file_id: optional string
///             - filename: optional string
///           - type: `"file"`
///     - role: `"user"`
///     - name: optional string
///   - `ChatCompletionAssistantMessageParam = object`
///     Messages sent by the model in response to user messages.
///     - role: `"assistant"`
///     - audio: optional object
///       Data about a previous audio response from the model.
///       - id: string
///         Unique identifier for a previous audio response.
///     - content: optional string or array of
///       `ChatCompletionContentPartText` or
///       `ChatCompletionContentPartRefusal`
///       Required unless `tool_calls` or deprecated `function_call` is
///       specified.
///       One of the following:
///       - `TextContent = string`
///       - `ArrayOfContentParts = array of ChatCompletionContentPartText or ChatCompletionContentPartRefusal`
///         Can be one or more text parts, or exactly one refusal part.
///         One of the following:
///         - `ChatCompletionContentPartText = object`
///           - text: string
///           - type: `"text"`
///         - `ChatCompletionContentPartRefusal = object`
///           - refusal: string
///           - type: `"refusal"`
///     - function_call: optional object
///       Deprecated and replaced by `tool_calls`.
///       - arguments: string
///       - name: string
///     - name: optional string
///     - refusal: optional string
///       The refusal message by the assistant.
///     - tool_calls: optional array of `ChatCompletionMessageToolCall`
///       The tool calls generated by the model.
///       One of the following:
///       - `ChatCompletionMessageFunctionToolCall = object`
///         A function tool call created by the model.
///         - id: string
///         - function: object
///           - arguments: string
///           - name: string
///         - type: `"function"`
///       - `ChatCompletionMessageCustomToolCall = object`
///         A custom tool call created by the model.
///         - id: string
///         - custom: object
///           - input: string
///           - name: string
///         - type: `"custom"`
///   - `ChatCompletionToolMessageParam = object`
///     Tool response message.
///     - content: string or array of `ChatCompletionContentPartText`
///       One of the following:
///       - `TextContent = string`
///       - `ArrayOfContentParts = array of ChatCompletionContentPartText`
///         Only `text` parts are supported.
///         - text: string
///         - type: `"text"`
///     - role: `"tool"`
///     - tool_call_id: string
///       Tool call that this message is responding to.
///   - `ChatCompletionFunctionMessageParam = object`
///     Deprecated function response message.
///     - content: string
///     - name: string
///     - role: `"function"`
/// - model: string
///   Model slug used to generate the response, such as `gpt-4o`, `o3`,
///   `gpt-5.4`, `gpt-5.4-mini`, or `gpt-5.4-nano`.
/// - audio: optional `ChatCompletionAudioParam`
///   Parameters for audio output. Required when audio output is requested with
///   `modalities: ["audio"]`.
///   - format: `"wav"` or `"aac"` or `"mp3"` or `"flac"` or `"opus"` or `"pcm16"`
///     Specifies the output audio format.
///   - voice: string or built-in voice name or object
///     Supported built-in voices include `alloy`, `ash`, `ballad`, `coral`,
///     `echo`, `sage`, `shimmer`, `verse`, `marin`, and `cedar`.
///     Custom voice objects may also be used with an `id`.
/// - frequency_penalty: optional number
///   Number between `-2.0` and `2.0`. Positive values reduce verbatim
///   repetition.
/// - function_call: optional `"none"` or `"auto"` or object
///   Deprecated in favor of `tool_choice`. Controls which function is called by
///   the model.
/// - functions: optional array of object
///   Deprecated in favor of `tools`. A list of functions the model may call.
///   - name: string
///   - description: optional string
///   - parameters: optional JSON Schema object
/// - logit_bias: optional map[number]
///   Maps token IDs to bias values between `-100` and `100`.
/// - logprobs: optional boolean
///   Whether to return log probabilities of output tokens.
/// - max_completion_tokens: optional number
///   Upper bound for completion tokens, including reasoning tokens.
/// - max_tokens: optional number
///   Deprecated in favor of `max_completion_tokens`. Controls the maximum
///   number of generated tokens.
/// - metadata: optional metadata object
///   Up to 16 key-value pairs attached to the request object.
/// - modalities: optional array of `"text"` or `"audio"`
///   Requested output modalities.
/// - n: optional number
///   Number of chat completion choices to generate. Keep `n = 1` to minimize
///   costs.
/// - parallel_tool_calls: optional boolean
///   Whether to enable parallel function calling during tool use.
/// - prediction: optional `ChatCompletionPredictionContent`
///   Static predicted output content that can accelerate regeneration.
///   - content: string or array of `ChatCompletionContentPartText`
///   - type: `"content"`
/// - presence_penalty: optional number
///   Number between `-2.0` and `2.0`. Positive values encourage new topics.
/// - prompt_cache_key: optional string
///   Used by OpenAI to cache responses for similar requests.
/// - prompt_cache_retention: optional `"in-memory"` or `"24h"`
///   Retention policy for prompt caching.
/// - reasoning_effort: optional `ReasoningEffort`
///   Supported values include `"none"`, `"minimal"`, `"low"`, `"medium"`,
///   `"high"`, and `"xhigh"` depending on model family.
/// - response_format: optional `ResponseFormatText`,
///   `ResponseFormatJSONSchema`, or `ResponseFormatJSONObject`
///   Controls the expected output format.
///   One of the following:
///   - `ResponseFormatText = object`
///     - type: `"text"`
///   - `ResponseFormatJSONSchema = object`
///     - json_schema: object
///       - name: string
///       - description: optional string
///       - schema: optional JSON Schema object
///       - strict: optional boolean
///     - type: `"json_schema"`
///   - `ResponseFormatJSONObject = object`
///     - type: `"json_object"`
/// - safety_identifier: optional string
///   Stable identifier used to help detect misuse while avoiding direct
///   personal information.
/// - seed: optional number
///   Beta deterministic sampling hint. Deprecated in newer flows.
/// - service_tier: optional `"auto"` or `"default"` or `"flex"` or `"scale"` or `"priority"`
///   Specifies the processing tier used for serving the request.
/// - stop: optional string or array of string
///   Up to 4 stop sequences. Not supported with some latest reasoning models.
/// - store: optional boolean
///   Whether the output may be stored for model distillation or eval products.
/// - stream: optional boolean
///   If true, the response is streamed using server-sent events.
/// - stream_options: optional `ChatCompletionStreamOptions`
///   Options for streaming responses. Only set when `stream` is true.
///   - include_obfuscation: optional boolean
///   - include_usage: optional boolean
///     If set, a final usage chunk is streamed before `data: [DONE]`.
/// - temperature: optional number
///   Sampling temperature between `0` and `2`.
/// - tool_choice: optional `ChatCompletionToolChoiceOption`
///   Controls which tool, if any, is called by the model.
///   One of the following:
///   - `"none"`
///   - `"auto"`
///   - `"required"`
///   - allowed-tools object
///   - named function tool object
///   - named custom tool object
/// - tools: optional array of `ChatCompletionTool`
///   A list of tools the model may call.
///   One of the following:
///   - `ChatCompletionFunctionTool = object`
///     - function: `FunctionDefinition`
///       - name: string
///       - description: optional string
///       - parameters: optional JSON Schema object
///       - strict: optional boolean
///     - type: `"function"`
///   - `ChatCompletionCustomTool = object`
///     - custom: object
///       - name: string
///       - description: optional string
///       - format: optional text or grammar object
///     - type: `"custom"`
/// - top_logprobs: optional number
///   Integer between `0` and `20`. Requires `logprobs = true`.
/// - top_p: optional number
///   Nucleus sampling parameter between `0` and `1`.
/// - user: optional string
///   Deprecated in favor of `safety_identifier` and `prompt_cache_key`.
/// - verbosity: optional `"low"` or `"medium"` or `"high"`
///   Constrains the verbosity of the model's response.
/// - web_search_options: optional object
///   Options for the web search tool.
///   - search_context_size: optional `"low"` or `"medium"` or `"high"`
///   - user_location: optional object
///     - approximate: object
///       - city: optional string
///       - country: optional string
///       - region: optional string
///       - timezone: optional string
///       - type: `"approximate"`
///
/// Notes about this implementation:
/// - This builder currently emits the subset of fields supported by the crate's
///   `ModelRequest` and the selected OpenAI request profile.
/// - System instructions are serialized as `system` messages.
/// - Assistant tool calls are serialized as `tool_calls` with `type:
///   "function"`.
/// - Tool results are serialized as `tool` messages with `tool_call_id`.
/// - Streaming requests add `stream_options: { "include_usage": true }`.
/// - Additional provider-specific fields may be merged in via `extra_body`.
fn build_request(request: &ModelRequest, stream: bool) -> Value {
    let profile = resolve_request_profile(&request.model, OpenAITransport::ChatCompletions);
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({ "role": super::OpenAIRole::System, "content": system }));
    }

    for message in &request.messages {
        match request_role(&message.role) {
            super::OpenAIRole::Assistant => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                for block in &message.content {
                    match block {
                        RequestContent::Text { text } => text_parts.push(text.clone()),
                        RequestContent::ToolUse { id, name, input } => tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": input.to_string(),
                            }
                        })),
                        RequestContent::ToolResult { .. } => {}
                    }
                }
                let mut entry = json!({ "role": super::OpenAIRole::Assistant });
                entry["content"] = if text_parts.is_empty() {
                    Value::Null
                } else {
                    Value::String(text_parts.join(""))
                };
                if !tool_calls.is_empty() {
                    entry["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(entry);
            }
            role => {
                for block in &message.content {
                    match block {
                        RequestContent::Text { text } => {
                            messages.push(json!({ "role": role, "content": text }));
                        }
                        RequestContent::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            messages.push(json!({
                                "role": super::OpenAIRole::Tool,
                                "tool_call_id": tool_use_id,
                                "content": content,
                            }));
                        }
                        RequestContent::ToolUse { .. } => {}
                    }
                }
            }
        }
    }

    let mut root = json!({
        "model": request.model,
        "messages": messages,
        "max_tokens": request.max_tokens,
        "stream": stream,
    });

    if let Some(tools) = &request.tools {
        root["tools"] = tool_definitions(tools);
    }

    if profile.supports_temperature
        && let Some(temperature) = request.sampling.temperature
    {
        root["temperature"] = json!(temperature);
    }

    if profile.supports_top_p
        && let Some(top_p) = request.sampling.top_p
    {
        root["top_p"] = json!(top_p);
    }

    if profile.supports_top_k
        && let Some(top_k) = request.sampling.top_k
    {
        root["top_k"] = json!(top_k);
    }

    if let Some(payload) = reasoning_value(profile, request.thinking.as_deref()) {
        match payload {
            super::shared::OpenAIReasoningValue::Effort(effort) => {
                root["reasoning_effort"] = json!(effort);
            }
            super::shared::OpenAIReasoningValue::Thinking { enabled } => {
                root["thinking"] = json!({
                    "type": if enabled { "enabled" } else { "disabled" },
                });
            }
        }
    }

    if stream {
        root["stream_options"] = json!({ "include_usage": true });
    }

    merge_extra_body(&mut root, request.extra_body.as_ref());

    root
}

/// Here is the documentation of the ChatCompletion response body.
///
/// Returns
/// - `ChatCompletion = object`
///   Represents a chat completion response returned by the model based on the
///   provided input.
/// - id: string
///   A unique identifier for the chat completion.
/// - choices: array of object
///   A list of chat completion choices. Can be more than one if `n > 1`.
///   - finish_reason: `"stop"` or `"length"` or `"tool_calls"` or
///     `"content_filter"` or `"function_call"`
///     The reason the model stopped generating tokens.
///   - index: number
///     The index of the choice in the list of choices.
///   - logprobs: object
///     Log-probability information for the choice.
///     - content: array of `ChatCompletionTokenLogprob`
///       A list of message content tokens with log probability information.
///       - token: string
///       - bytes: array of number
///       - logprob: number
///       - top_logprobs: array of object
///         The most likely tokens and their log probabilities at this token
///         position.
///         - token: string
///         - bytes: array of number
///         - logprob: number
///     - refusal: array of `ChatCompletionTokenLogprob`
///       A list of refusal tokens with the same token-logprob structure.
///   - message: `ChatCompletionMessage`
///     A chat completion message generated by the model.
///     - content: string
///       The contents of the message.
///     - refusal: string
///       The refusal message generated by the model.
///     - role: `"assistant"`
///       The role of the author of this message.
///     - annotations: optional array of object
///       Annotations for the message, for example when using web search.
///       - type: `"url_citation"`
///       - url_citation: object
///         - end_index: number
///         - start_index: number
///         - title: string
///         - url: string
///     - audio: optional `ChatCompletionAudio`
///       Audio response data when audio output is requested.
///       - id: string
///       - data: string
///       - expires_at: number
///       - transcript: string
///     - function_call: optional object
///       Deprecated and replaced by `tool_calls`.
///       - arguments: string
///       - name: string
///     - tool_calls: optional array of `ChatCompletionMessageToolCall`
///       The tool calls generated by the model.
///       One of the following:
///       - `ChatCompletionMessageFunctionToolCall = object`
///         - id: string
///         - function: object
///           - arguments: string
///           - name: string
///         - type: `"function"`
///       - `ChatCompletionMessageCustomToolCall = object`
///         - id: string
///         - custom: object
///           - input: string
///           - name: string
///         - type: `"custom"`
/// - created: number
///   Unix timestamp in seconds indicating when the chat completion was created.
/// - model: string
///   The model used for the chat completion.
/// - object: `"chat.completion"`
///   The object type.
/// - service_tier: optional `"auto"` or `"default"` or `"flex"` or `"scale"` or `"priority"`
///   The processing tier used to serve the request.
/// - system_fingerprint: optional string
///   Deprecated backend fingerprint that can be used with `seed` to reason
///   about determinism and backend changes.
/// - usage: optional `CompletionUsage`
///   Usage statistics for the completion request. See the comment above
///   `parse_usage`.
///
/// Notes about this implementation:
/// - `parse_response` currently reads `id`, the first entry from `choices`,
///   assistant `message.content`, assistant `message.tool_calls`,
///   `choice.finish_reason`, and `usage`.
/// - Reasoning text is also read from `message.reasoning_content` when present,
///   even though that field is not part of the basic schema summary above.
/// - Other documented response fields such as `created`, `model`, `object`,
///   `service_tier`, `annotations`, `audio`, `logprobs`, and deprecated
///   `function_call` are not currently mapped into `ModelResponse`.
fn parse_response(value: Value) -> Result<ModelResponse> {
    let response: OpenAIChatCompletionResponse = serde_json::from_value(value.clone())
        .context("failed to deserialize openai chat-completion response")?;
    let mut content = Vec::new();
    let mut stop_reason = None;
    let mut metadata = ResponseMetadata::default();

    if let Some(choice) = response.choices.first() {
        if let Some(message) = &choice.message {
            if let Some(reasoning_content) = &message.reasoning_content {
                metadata.extras.push(ResponseExtra::ReasoningText {
                    text: reasoning_content.clone(),
                });
            }
            if let Some(text) = &message.content {
                let (assistant_text, reasoning) = split_tagged_text(text);
                for text in reasoning {
                    if !text.is_empty() {
                        metadata.extras.push(ResponseExtra::ReasoningText { text });
                    }
                }
                if !assistant_text.is_empty() {
                    content.push(ResponseContent::Text(assistant_text));
                }
            }
            for tool_call in &message.tool_calls {
                if let Some(parsed) = parse_tool_use(tool_call) {
                    content.push(parsed);
                }
            }
        }
        if let Some(reason) = &choice.finish_reason {
            stop_reason = Some(parse_finish_reason(reason));
        }
    }

    let usage = response
        .usage
        .as_ref()
        .map(|usage| Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
        })
        .unwrap_or_default();

    if let Some(provider_payload) = build_provider_specific_response_payload(&response) {
        metadata.extras.push(ResponseExtra::ProviderSpecific {
            provider: "openai".to_string(),
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

pub(super) fn parse_tool_use(
    value: &OpenAIChatCompletionMessageToolCall,
) -> Option<ResponseContent> {
    match value.kind.as_str() {
        "function" => {
            let function = value.function.as_ref()?;
            let input = serde_json::from_str(&function.arguments)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
            Some(ResponseContent::ToolUse {
                id: value.id.clone(),
                name: function.name.clone(),
                input,
            })
        }
        "custom" => {
            let custom = value.custom.as_ref()?;
            Some(ResponseContent::ToolUse {
                id: value.id.clone(),
                name: custom.name.clone(),
                input: Value::String(custom.input.clone()),
            })
        }
        _ => None,
    }
}

pub(super) fn build_provider_specific_response_payload(
    response: &OpenAIChatCompletionResponse,
) -> Option<Value> {
    let mut payload = serde_json::Map::new();

    if let Some(created) = response.created {
        payload.insert("created".to_string(), json!(created));
    }
    if let Some(model) = &response.model {
        payload.insert("model".to_string(), json!(model));
    }
    if let Some(object) = &response.object {
        payload.insert("object".to_string(), json!(object));
    }
    if let Some(service_tier) = &response.service_tier {
        payload.insert("service_tier".to_string(), json!(service_tier));
    }
    if let Some(system_fingerprint) = &response.system_fingerprint {
        payload.insert("system_fingerprint".to_string(), json!(system_fingerprint));
    }
    if let Some(usage) = &response.usage {
        payload.insert("usage".to_string(), json!(usage));
    }

    let choices = response
        .choices
        .iter()
        .filter_map(build_provider_specific_choice_payload)
        .collect::<Vec<_>>();
    if !choices.is_empty() {
        payload.insert("choices".to_string(), Value::Array(choices));
    }

    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

fn build_provider_specific_choice_payload(choice: &OpenAIChatCompletionChoice) -> Option<Value> {
    let mut payload = serde_json::Map::new();

    if let Some(index) = choice.index {
        payload.insert("index".to_string(), json!(index));
    }
    if let Some(logprobs) = &choice.logprobs {
        payload.insert("logprobs".to_string(), json!(logprobs));
    }
    if let Some(message) = &choice.message
        && let Some(message_payload) = build_provider_specific_message_payload(message)
    {
        payload.insert("message".to_string(), message_payload);
    }

    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

fn build_provider_specific_message_payload(message: &OpenAIChatCompletionMessage) -> Option<Value> {
    let mut payload = serde_json::Map::new();

    if let Some(role) = &message.role {
        payload.insert("role".to_string(), json!(role));
    }
    if let Some(refusal) = &message.refusal {
        payload.insert("refusal".to_string(), json!(refusal));
    }
    if !message.annotations.is_empty() {
        payload.insert("annotations".to_string(), json!(message.annotations));
    }
    if let Some(audio) = &message.audio {
        payload.insert("audio".to_string(), json!(audio));
    }
    if let Some(function_call) = &message.function_call {
        payload.insert("function_call".to_string(), json!(function_call));
    }
    let custom_tool_calls = message
        .tool_calls
        .iter()
        .filter(|tool_call| tool_call.kind == "custom")
        .cloned()
        .collect::<Vec<_>>();
    if !custom_tool_calls.is_empty() {
        payload.insert("tool_calls".to_string(), json!(custom_tool_calls));
    }

    if payload.is_empty() {
        None
    } else {
        Some(Value::Object(payload))
    }
}

/// Here is the documentation of `CompletionUsage`.
///
/// - usage: optional `CompletionUsage`
///   Usage statistics for the completion request.
///   - completion_tokens: number
///     Number of tokens in the generated completion.
///   - prompt_tokens: number
///     Number of tokens in the prompt.
///   - total_tokens: number
///     Total number of tokens used in the request (`prompt + completion`).
///   - completion_tokens_details: optional object
///     Breakdown of tokens used in a completion.
///     - accepted_prediction_tokens: optional number
///       Tokens from a predicted output that appeared in the completion.
///     - audio_tokens: optional number
///       Audio input tokens generated by the model.
///     - reasoning_tokens: optional number
///       Tokens generated by the model for reasoning.
///     - rejected_prediction_tokens: optional number
///       Tokens from a predicted output that did not appear in the completion,
///       but still count toward billing and context limits.
///   - prompt_tokens_details: optional object
///     Breakdown of tokens used in the prompt.
///     - audio_tokens: optional number
///       Audio input tokens present in the prompt.
///     - cached_tokens: optional number
///       Cached tokens present in the prompt.
///
/// Notes about this implementation:
/// - `parse_usage` currently maps only `prompt_tokens` to
///   `Usage::input_tokens` and `completion_tokens` to
///   `Usage::output_tokens`.
/// - `total_tokens`, `completion_tokens_details`, and
///   `prompt_tokens_details` are documented here but not yet fully projected
///   into the crate's `Usage` type.
///
/// Example:
/// ```json
/// {
///   "usage": {
///     "prompt_tokens": 19,
///     "completion_tokens": 10,
///     "total_tokens": 29,
///     "prompt_tokens_details": {
///       "cached_tokens": 0,
///       "audio_tokens": 0
///     },
///     "completion_tokens_details": {
///       "reasoning_tokens": 0,
///       "audio_tokens": 0,
///       "accepted_prediction_tokens": 0,
///       "rejected_prediction_tokens": 0
///     }
///   }
/// }
/// ```
#[allow(dead_code)]
fn parse_usage(value: &Value) -> Option<Usage> {
    let usage: OpenAICompletionUsage = serde_json::from_value(value.clone()).ok()?;
    Some(Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens),
    })
}

fn parse_finish_reason(value: &str) -> StopReason {
    match value {
        "tool_calls" => StopReason::ToolUse,
        "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "stop" => StopReason::EndTurn,
        "content_filter" => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}

#[async_trait]
impl ModelProviderSDK for OpenAIProvider {
    async fn completion(&self, request: ModelRequest) -> Result<ModelResponse> {
        let body = build_request(&request, false);
        debug!(
            provider = "openai",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending openai completion request"
        );

        let response = self
            .request_builder(&body)
            .send()
            .await
            .context("failed to send openai request")?
            .error_for_status()
            .context("openai request failed")?;

        let value: Value = response
            .json()
            .await
            .context("failed to decode openai response")?;
        parse_response(value)
    }

    /// --------- Here is an example of stream response ------------------------
    /// ```text
    /// {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{"role":"assistant","content":""},"logprobs":null,"finish_reason":null}]}
    /// {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{"content":"Hello"},"logprobs":null,"finish_reason":null}]}
    /// ....
    /// {"id":"chatcmpl-123","object":"chat.completion.chunk","created":1694268190,"model":"gpt-4o-mini", "system_fingerprint": "fp_44709d6fcb", "choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}]}
    /// ```
    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        stream::completion_stream(self, request).await
    }

    fn name(&self) -> &str {
        "openai"
    }
}

#[async_trait]
impl ProviderAdapter for OpenAIProvider {
    fn family(&self) -> ProviderFamily {
        ProviderFamily::openai()
    }

    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        let profile = resolve_request_profile(model, OpenAITransport::ChatCompletions);
        let mut capabilities = ProviderCapabilities::openai();
        capabilities.supports_temperature = profile.supports_temperature;
        capabilities.supports_top_p = profile.supports_top_p;
        capabilities.supports_reasoning_effort =
            matches!(profile.reasoning_mode, OpenAIReasoningMode::Effort);
        capabilities.supports_top_k = profile.supports_top_k;
        capabilities.supports_reasoning_content = profile.supports_reasoning_content;
        capabilities.supported_roles = profile.supported_roles.to_vec();
        capabilities
    }
}

#[cfg(test)]
mod tests {
    use clawcr_protocol::{
        ModelRequest, RequestContent, RequestMessage, SamplingControls, ToolDefinition,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::super::OpenAIReasoningEffort;
    use super::super::shared::reasoning_effort;
    use super::{parse_finish_reason, parse_response, parse_usage};
    use clawcr_protocol::{ResponseContent, ResponseExtra, StopReason};

    use crate::openai::chat_completions::build_request;

    #[test]
    fn debug_request_body_includes_tools_and_reasoning_effort() {
        let request = ModelRequest {
            model: "gpt-4o-mini".to_string(),
            system: Some("You are helpful.".to_string()),
            messages: vec![
                RequestMessage {
                    role: "assistant".to_string(),
                    content: vec![
                        RequestContent::Text {
                            text: "Calling tool".to_string(),
                        },
                        RequestContent::ToolUse {
                            id: "call_123".to_string(),
                            name: "get_weather".to_string(),
                            input: json!({"city": "Boston"}),
                        },
                    ],
                },
                RequestMessage {
                    role: "user".to_string(),
                    content: vec![RequestContent::ToolResult {
                        tool_use_id: "call_123".to_string(),
                        content: "{\"temp\":72}".to_string(),
                        is_error: Some(false),
                    }],
                },
            ],
            max_tokens: 256,
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
                ..SamplingControls::default()
            },
            thinking: Some("medium".to_string()),
            extra_body: None,
        };

        let body = build_request(&request, true);

        assert_eq!(body["model"], json!("gpt-4o-mini"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_tokens"], json!(256));
        assert_eq!(body["reasoning_effort"], json!("medium"));
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["messages"][1]["role"], json!("assistant"));
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["arguments"],
            json!("{\"city\":\"Boston\"}")
        );
        assert_eq!(body["messages"][1]["content"], json!("Calling tool"));
        assert_eq!(body["messages"][2]["role"], json!("tool"));
        assert_eq!(body["messages"][2]["tool_call_id"], json!("call_123"));
    }

    #[test]
    fn debug_request_body_uses_thinking_object_for_zai_models() {
        let request = ModelRequest {
            model: "glm-4.5".to_string(),
            system: None,
            messages: vec![RequestMessage {
                role: "user".to_string(),
                content: vec![RequestContent::Text {
                    text: "hi".to_string(),
                }],
            }],
            max_tokens: 64,
            tools: None,
            sampling: SamplingControls::default(),
            thinking: Some("disabled".to_string()),
            extra_body: None,
        };

        let body = build_request(&request, false);

        assert_eq!(body["thinking"]["type"], json!("disabled"));
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn debug_request_body_includes_sampling_controls_for_capable_models() {
        let request = ModelRequest {
            model: "glm-4.5".to_string(),
            system: None,
            messages: vec![RequestMessage {
                role: "user".to_string(),
                content: vec![RequestContent::Text {
                    text: "hi".to_string(),
                }],
            }],
            max_tokens: 64,
            tools: None,
            sampling: SamplingControls {
                temperature: Some(0.3),
                top_p: Some(0.9),
                top_k: Some(40),
            },
            thinking: Some("enabled".to_string()),
            extra_body: None,
        };

        let body = build_request(&request, false);

        assert_eq!(body["thinking"]["type"], json!("enabled"));
        assert_eq!(body["temperature"], json!(0.3));
        assert_eq!(body["top_p"], json!(0.9));
        assert_eq!(body["top_k"], json!(40));
    }

    #[test]
    fn debug_request_body_preserves_top_p_precision() {
        let request = ModelRequest {
            model: "glm-5.1".to_string(),
            system: None,
            messages: vec![RequestMessage {
                role: "user".to_string(),
                content: vec![RequestContent::Text {
                    text: "Reply with OK only.".to_string(),
                }],
            }],
            max_tokens: 8192,
            tools: None,
            sampling: SamplingControls {
                temperature: Some(1.0),
                top_p: Some(0.95),
                top_k: None,
            },
            thinking: Some("enabled".to_string()),
            extra_body: None,
        };

        let body = build_request(&request, true);

        assert_eq!(body["top_p"], json!(0.95));
    }

    #[test]
    fn parse_response_extracts_text_tool_calls_and_usage() {
        let response = parse_response(json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1741569952,
            "model": "gpt-5.4",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [
                            {
                                "id": "call_abc123",
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "arguments": "{\"location\":\"Boston, MA\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ],
            "usage": {
                "prompt_tokens": 82,
                "completion_tokens": 17,
                "total_tokens": 99,
                "prompt_tokens_details": {
                    "cached_tokens": 12,
                    "audio_tokens": 0
                }
            },
            "service_tier": "default"
        }))
        .expect("parse response");

        assert_eq!(response.id, "chatcmpl-123");
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(response.usage.input_tokens, 82);
        assert_eq!(response.usage.output_tokens, 17);
        assert_eq!(response.usage.cache_read_input_tokens, Some(12));
        assert_eq!(response.content.len(), 1);
        match &response.content[0] {
            ResponseContent::ToolUse { id, name, input } => {
                assert_eq!(id, "call_abc123");
                assert_eq!(name, "get_weather");
                assert_eq!(input, &json!({"location": "Boston, MA"}));
            }
            other => panic!("expected tool use, got {other:?}"),
        }
        assert!(response.metadata.extras.iter().any(|extra| matches!(
            extra,
            ResponseExtra::ProviderSpecific { provider, .. } if provider == "openai"
        )));
    }

    #[test]
    fn parse_response_preserves_text_content() {
        let response = parse_response(json!({
            "id": "chatcmpl-456",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello! How can I assist you today?"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8
            }
        }))
        .expect("parse response");

        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.content.len(), 1);
        match &response.content[0] {
            ResponseContent::Text(text) => {
                assert_eq!(text, "Hello! How can I assist you today?");
            }
            other => panic!("expected text response, got {other:?}"),
        }
    }

    #[test]
    fn parse_usage_reads_chat_completion_usage_shape() {
        let usage = parse_usage(&json!({
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18,
            "prompt_tokens_details": {
                "cached_tokens": 5,
                "audio_tokens": 0
            },
            "completion_tokens_details": {
                "reasoning_tokens": 2,
                "audio_tokens": 0,
                "accepted_prediction_tokens": 1,
                "rejected_prediction_tokens": 0
            }
        }))
        .expect("parse usage");

        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.cache_read_input_tokens, Some(5));
    }

    #[test]
    fn parse_finish_reason_matches_chat_completion_contract() {
        assert_eq!(parse_finish_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(parse_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(parse_finish_reason("stop"), StopReason::EndTurn);
        assert_eq!(
            parse_finish_reason("content_filter"),
            StopReason::StopSequence
        );
        assert_eq!(parse_finish_reason("function_call"), StopReason::ToolUse);
    }

    #[test]
    fn parse_response_preserves_provider_specific_response_fields() {
        let response = parse_response(json!({
            "id": "chatcmpl-789",
            "object": "chat.completion",
            "created": 1741569952,
            "model": "gpt-5.4",
            "service_tier": "default",
            "system_fingerprint": "fp_123",
            "choices": [
                {
                    "index": 0,
                    "logprobs": {
                        "content": [],
                        "refusal": []
                    },
                    "message": {
                        "role": "assistant",
                        "content": "hello",
                        "refusal": "none",
                        "annotations": [
                            {
                                "type": "url_citation",
                                "url_citation": {
                                    "start_index": 0,
                                    "end_index": 5,
                                    "title": "Example",
                                    "url": "https://example.com"
                                }
                            }
                        ],
                        "audio": {
                            "id": "aud_1",
                            "data": "Zm9v",
                            "expires_at": 1741569999u64,
                            "transcript": "hello"
                        }
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        }))
        .expect("parse response");

        let provider_payload = response
            .metadata
            .extras
            .iter()
            .find_map(|extra| match extra {
                ResponseExtra::ProviderSpecific { provider, payload } if provider == "openai" => {
                    Some(payload)
                }
                _ => None,
            })
            .expect("provider-specific metadata");

        assert_eq!(provider_payload["object"], json!("chat.completion"));
        assert_eq!(provider_payload["model"], json!("gpt-5.4"));
        assert_eq!(provider_payload["service_tier"], json!("default"));
        assert_eq!(provider_payload["system_fingerprint"], json!("fp_123"));
        assert_eq!(
            provider_payload["choices"][0]["message"]["annotations"][0]["type"],
            json!("url_citation")
        );
    }

    #[test]
    fn map_reasoning_effort_maps_supported_values() {
        assert_eq!(
            reasoning_effort(Some("disabled")),
            Some(OpenAIReasoningEffort::None)
        );
        assert_eq!(
            reasoning_effort(Some("enabled")),
            Some(OpenAIReasoningEffort::Medium)
        );
        assert_eq!(
            reasoning_effort(Some("low")),
            Some(OpenAIReasoningEffort::Low)
        );
        assert_eq!(
            reasoning_effort(Some("medium")),
            Some(OpenAIReasoningEffort::Medium)
        );
        assert_eq!(
            reasoning_effort(Some("high")),
            Some(OpenAIReasoningEffort::High)
        );
        assert_eq!(
            reasoning_effort(Some("xhigh")),
            Some(OpenAIReasoningEffort::XHigh)
        );
        assert_eq!(reasoning_effort(Some("unknown")), None);
    }
}
