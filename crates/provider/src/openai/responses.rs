use std::{collections::HashMap, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use devo_protocol::{
    ModelRequest, ModelResponse, RequestContent, ResponseContent, ResponseExtra, ResponseMetadata,
    StopReason, StreamEvent, Usage,
};
use futures::{Stream, StreamExt};
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest_eventsource::{Event, EventSource};
use serde_json::{Value, json};
use tracing::debug;

use crate::text_normalization::{TaggedTextFragment, TaggedTextParser, split_tagged_text};
use crate::{ModelProviderSDK, merge_extra_body};

use super::capabilities::{OpenAITransport, resolve_request_profile};
use super::{
    OpenAIRole,
    shared::{invalid_status_error, request_role, tool_definitions},
};

/// OpenAI Responses API provider.
/// <https://developers.openai.com/api/reference/resources/responses>
/// This adapter keeps the new Responses wire format isolated from the legacy
/// chat-completions adapter so the transport can evolve independently.
pub struct OpenAIResponsesProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl OpenAIResponsesProvider {
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
        format!("{}/responses", self.base_url.trim_end_matches('/'))
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

/// Builds the exact OpenAI Responses request body used by this provider.
fn build_request(request: &ModelRequest, stream: bool) -> Value {
    let profile = resolve_request_profile(&request.model, OpenAITransport::Responses);
    let mut root = json!({
        "model": request.model,
        "input": build_input(request),
        "max_output_tokens": request.max_tokens,
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

    if let Some(reasoning) = request.reasoning_effort {
        root["reasoning"] = json!({ "effort": reasoning });
    }

    if stream {
        root["stream_options"] = json!({ "include_usage": true });
    }

    merge_extra_body(&mut root, request.extra_body.as_ref());

    root
}

fn build_input(request: &ModelRequest) -> Vec<Value> {
    let mut input = Vec::new();

    if let Some(system) = &request.system {
        input.push(json!({
            "type": "message",
            "role": OpenAIRole::System,
            "content": [{"type": "input_text", "text": system}],
        }));
    }

    for message in &request.messages {
        let role = request_role(&message.role);
        input.push(build_input_message(role, &message.content));
    }

    input
}

fn build_input_message(role: OpenAIRole, content: &[RequestContent]) -> Value {
    let content = content
        .iter()
        .map(|block| match block {
            RequestContent::Text { text } => json!({
                "type": "input_text",
                "text": text,
            }),
            RequestContent::Reasoning { text } => json!({
                "type": "reasoning",
                "text": text,
            }),
            RequestContent::ToolUse { id, name, input } => json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "input": input,
            }),
            RequestContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => json!({
                "type": "function_call_output",
                "call_id": tool_use_id,
                "output": content,
                "is_error": is_error,
            }),
        })
        .collect::<Vec<_>>();

    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}
fn parse_response(value: Value) -> Result<ModelResponse> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut content = Vec::new();
    let mut metadata = ResponseMetadata::default();

    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            if let Some(reasoning_content) = item.get("reasoning_content").and_then(Value::as_str) {
                metadata.extras.push(ResponseExtra::ReasoningText {
                    text: reasoning_content.to_string(),
                });
            }
            if matches!(item.get("type").and_then(Value::as_str), Some("message")) {
                if let Some(items) = item.get("content").and_then(Value::as_array) {
                    for message_item in items {
                        if let Some(text) = message_item.get("text").and_then(Value::as_str) {
                            let (assistant_text, reasoning) = split_tagged_text(text);
                            for text in reasoning {
                                if !text.is_empty() {
                                    metadata.extras.push(ResponseExtra::ReasoningText { text });
                                }
                            }
                            if !assistant_text.is_empty() {
                                content.push(ResponseContent::Text(assistant_text));
                            }
                            continue;
                        }
                        if let Some(parsed) = parse_message_content(message_item) {
                            content.push(parsed);
                        }
                    }
                }
                continue;
            }
            content.extend(parse_output_item(item));
        }
    }

    let stop_reason = value
        .get("status")
        .and_then(Value::as_str)
        .map(parse_status_reason)
        .or_else(|| {
            value
                .get("incomplete")
                .and_then(|item| item.get("reason"))
                .and_then(Value::as_str)
                .map(parse_status_reason)
        });

    let usage = value.get("usage").and_then(parse_usage).unwrap_or_default();

    Ok(ModelResponse {
        id,
        content,
        stop_reason,
        usage,
        metadata,
    })
}

fn parse_output_item(item: &Value) -> Vec<ResponseContent> {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(parse_message_content)
            .collect(),
        Some("function_call") | Some("tool_call") => {
            let id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let input = item
                .get("arguments")
                .or_else(|| item.get("input"))
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            vec![ResponseContent::ToolUse { id, name, input }]
        }
        Some("reasoning") => Vec::new(),
        _ => Vec::new(),
    }
}

fn parse_message_content(item: &Value) -> Option<ResponseContent> {
    match item.get("type").and_then(Value::as_str) {
        Some("output_text") | Some("text") | Some("input_text") => {
            let assistant_text =
                split_tagged_text(item.get("text").and_then(Value::as_str).unwrap_or_default()).0;
            if assistant_text.is_empty() {
                None
            } else {
                Some(ResponseContent::Text(assistant_text))
            }
        }
        Some("tool_call") | Some("function_call") => Some(ResponseContent::ToolUse {
            id: item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input: item
                .get("arguments")
                .or_else(|| item.get("input"))
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
        }),
        _ => None,
    }
}

fn parse_usage(value: &Value) -> Option<Usage> {
    Some(Usage {
        input_tokens: value
            .get("input_tokens")
            .or_else(|| value.get("prompt_tokens"))
            .and_then(Value::as_u64)? as usize,
        output_tokens: value
            .get("output_tokens")
            .or_else(|| value.get("completion_tokens"))
            .and_then(Value::as_u64)? as usize,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    })
}

fn parse_status_reason(value: &str) -> StopReason {
    match value {
        "completed" | "stop" | "end_turn" => StopReason::EndTurn,
        "incomplete" | "max_output_tokens" | "length" => StopReason::MaxTokens,
        "tool_use" | "tool_calls" => StopReason::ToolUse,
        "stop_sequence" | "content_filter" => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}

#[async_trait]
impl ModelProviderSDK for OpenAIResponsesProvider {
    async fn completion(&self, request: ModelRequest) -> Result<ModelResponse> {
        let body = build_request(&request, false);
        debug!(
            provider = "openai-responses",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending openai responses completion request"
        );

        let response = self
            .request_builder(&body)
            .send()
            .await
            .context("failed to send openai responses request")?;
        let response = match response.error_for_status_ref() {
            Ok(_) => response,
            Err(_) => {
                let status = response.status();
                return Err(invalid_status_error(
                    "openai-responses",
                    &request.model,
                    "request",
                    status,
                    response,
                    &body,
                )
                .await);
            }
        };

        let value: Value = response
            .json()
            .await
            .context("failed to decode openai responses response")?;
        parse_response(value)
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let body = build_request(&request, true);
        debug!(
            provider = "openai-responses",
            api_base = %self.base_url,
            model = %request.model,
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            "sending openai responses streaming request"
        );

        let event_source = EventSource::new(self.request_builder(&body))
            .context("failed to create openai responses event source")?;
        let stream = async_stream::try_stream! {
            let mut text_buf = String::new();
            let mut reasoning_buf = String::new();
            let mut text_parser = TaggedTextParser::default();
            let mut response_id = String::new();
            let mut tool_calls: HashMap<String, (String, String, String)> = HashMap::new();
            let mut usage: Option<Usage> = None;
            let mut reasoning_started = false;
            let mut text_started = false;

            futures::pin_mut!(event_source);
            while let Some(event) = event_source.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(reqwest_eventsource::Error::InvalidStatusCode(status, response)) => {
                        Err(invalid_status_error(
                            "openai-responses",
                            &request.model,
                            "stream",
                            status,
                            response,
                            &body,
                        )
                        .await)?
                    }
                    Err(error) => Err(anyhow::anyhow!(
                        "openai responses stream error for model {}: {error}",
                        request.model
                    ))?,
                };

                match event {
                    Event::Open => {}
                    Event::Message(message) => {
                        if message.data == "[DONE]" {
                            break;
                        }

                        let chunk: Value = serde_json::from_str(&message.data)
                            .map_err(|error| anyhow::anyhow!("failed to parse openai responses stream chunk: {error}"))?;

                        if response_id.is_empty() {
                            response_id = chunk
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                        }

                        if let Some(parsed_usage) = chunk.get("usage").and_then(parse_usage) {
                            usage = Some(parsed_usage.clone());
                            yield StreamEvent::UsageDelta(parsed_usage);
                        }

                        match message.event.as_str() {
                            "response.output_text.delta" => {
                                let delta = chunk
                                    .get("delta")
                                    .and_then(Value::as_str)
                                    .or_else(|| chunk.get("text").and_then(Value::as_str))
                                    .unwrap_or_default();
                                if !delta.is_empty() {
                                    for fragment in text_parser.consume(delta) {
                                        match fragment {
                                            TaggedTextFragment::Text(text) => {
                                                if text.is_empty() {
                                                    continue;
                                                }
                                                if !text_started {
                                                    text_started = true;
                                                    yield StreamEvent::TextStart { index: 0 };
                                                }
                                                text_buf.push_str(&text);
                                                yield StreamEvent::TextDelta { index: 0, text };
                                            }
                                            TaggedTextFragment::Reasoning(text) => {
                                                if text.is_empty() {
                                                    continue;
                                                }
                                                if !reasoning_started {
                                                    reasoning_started = true;
                                                    yield StreamEvent::ReasoningStart { index: 1 };
                                                }
                                                reasoning_buf.push_str(&text);
                                                yield StreamEvent::ReasoningDelta { index: 1, text };
                                            }
                                        }
                                    }
                                }
                            }
                            "response.output_item.added" => {
                                if let Some(item) = chunk.get("item") {
                                    if let Some(reasoning_content) =
                                        item.get("reasoning_content").and_then(Value::as_str)
                                        && !reasoning_content.is_empty() {
                                            if !reasoning_started {
                                                reasoning_started = true;
                                                yield StreamEvent::ReasoningStart { index: 1 };
                                            }
                                            reasoning_buf.push_str(reasoning_content);
                                            yield StreamEvent::ReasoningDelta {
                                                index: 1,
                                                text: reasoning_content.to_string(),
                                            };
                                        }
                                    if let Some(ResponseContent::ToolUse { id, name, input }) = parse_output_item(item).into_iter().next() {
                                        let key = id.clone();
                                        tool_calls.insert(key.clone(), (id.clone(), name.clone(), input.to_string()));
                                        let index = tool_calls.len();
                                        yield StreamEvent::ToolCallStart {
                                            index,
                                            id,
                                            name,
                                            input,
                                        };
                                    }
                                }
                            }
                            "response.function_call_arguments.delta" | "response.output_item.delta" => {
                                let partial_json = chunk
                                    .get("delta")
                                    .or_else(|| chunk.get("arguments_delta"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let call_id = chunk
                                    .get("item_id")
                                    .or_else(|| chunk.get("call_id"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                if !partial_json.is_empty()
                                    && !call_id.is_empty()
                                    && let Some((index, entry)) = tool_calls
                                        .values_mut()
                                        .enumerate()
                                        .find(|(_, entry)| entry.0 == call_id)
                                {
                                    let input = &mut entry.2;
                                    input.push_str(partial_json);
                                    yield StreamEvent::ToolCallInputDelta {
                                        index: index + 1,
                                        partial_json: partial_json.to_string(),
                                    };
                                }
                            }
                            "response.completed" | "response.done" => {
                                for fragment in text_parser.finish() {
                                    match fragment {
                                        TaggedTextFragment::Text(text) => {
                                            if text.is_empty() {
                                                continue;
                                            }
                                            if !text_started {
                                                text_started = true;
                                                yield StreamEvent::TextStart { index: 0 };
                                            }
                                            text_buf.push_str(&text);
                                            yield StreamEvent::TextDelta { index: 0, text };
                                        }
                                        TaggedTextFragment::Reasoning(text) => {
                                            if text.is_empty() {
                                                continue;
                                            }
                                            if !reasoning_started {
                                                reasoning_started = true;
                                                yield StreamEvent::ReasoningStart { index: 1 };
                                            }
                                            reasoning_buf.push_str(&text);
                                            yield StreamEvent::ReasoningDelta { index: 1, text };
                                        }
                                    }
                                }
                                let response = if let Some(parsed) = chunk.get("response") {
                                    parse_response(parsed.clone())?
                                } else {
                                    ModelResponse {
                                        id: response_id.clone(),
                                        content: {
                                            let mut content = Vec::new();
                                            if !text_buf.is_empty() {
                                                content.push(ResponseContent::Text(text_buf.clone()));
                                            }
                                            for (id, name, input) in tool_calls.values() {
                                                let parsed_input = serde_json::from_str(input)
                                                    .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
                                                content.push(ResponseContent::ToolUse {
                                                    id: id.clone(),
                                                    name: name.clone(),
                                                    input: parsed_input,
                                                });
                                            }
                                            content
                                        },
                                        stop_reason: Some(StopReason::EndTurn),
                                        usage: usage.unwrap_or_default(),
                                        metadata: if reasoning_buf.is_empty() {
                                            ResponseMetadata::default()
                                        } else {
                                            ResponseMetadata {
                                                extras: vec![ResponseExtra::ReasoningText {
                                                    text: reasoning_buf.clone(),
                                                }],
                                            }
                                        },
                                    }
                                };
                                yield StreamEvent::MessageDone { response };
                                return;
                            }
                            _ => {}
                        }
                    }
                }
            }

            for fragment in text_parser.finish() {
                match fragment {
                    TaggedTextFragment::Text(text) => text_buf.push_str(&text),
                    TaggedTextFragment::Reasoning(text) => reasoning_buf.push_str(&text),
                }
            }

            let response = ModelResponse {
                id: response_id,
                content: {
                    let mut content = Vec::new();
                    if !text_buf.is_empty() {
                        content.push(ResponseContent::Text(text_buf));
                    }
                    for (id, name, input) in tool_calls.values() {
                        let parsed_input = serde_json::from_str(input)
                            .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
                        content.push(ResponseContent::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: parsed_input,
                        });
                    }
                    content
                },
                stop_reason: Some(StopReason::EndTurn),
                usage: usage.unwrap_or_default(),
                metadata: if reasoning_buf.is_empty() {
                    ResponseMetadata::default()
                } else {
                    ResponseMetadata {
                        extras: vec![ResponseExtra::ReasoningText {
                            text: reasoning_buf,
                        }],
                    }
                },
            };
            yield StreamEvent::MessageDone { response };
        };

        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "openai-responses"
    }
}

#[cfg(test)]
mod tests {
    use devo_protocol::{
        ModelRequest, RequestContent, RequestMessage, SamplingControls, ToolDefinition,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::parse_response;
    use devo_protocol::{ResponseContent, ResponseExtra};

    use crate::openai::responses::build_request;

    #[test]
    fn debug_request_body_includes_reasoning_and_tools() {
        let request = ModelRequest {
            model: "gpt-5.4".to_string(),
            system: Some("You are helpful.".to_string()),
            messages: vec![RequestMessage {
                role: "user".to_string(),
                content: vec![RequestContent::Text {
                    text: "hi".to_string(),
                }],
            }],
            max_tokens: 256,
            tools: Some(vec![ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather by city".to_string(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
            }]),
            sampling: SamplingControls {
                temperature: Some(0.4),
                top_p: Some(0.7),
                top_k: Some(12),
            },
            thinking: Some("medium".to_string()),
            reasoning_effort: Some(devo_protocol::ReasoningEffort::Medium),
            extra_body: None,
        };

        let body = build_request(&request, true);

        assert_eq!(body["model"], json!("gpt-5.4"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_output_tokens"], json!(256));
        assert_eq!(body["temperature"], json!(0.4));
        assert_eq!(body["top_p"], json!(0.7));
        assert!(body.get("top_k").is_none());
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["input"][0]["role"], json!("system"));
    }

    #[test]
    fn parse_response_extracts_text_and_tool_use() {
        let response = parse_response(json!({
            "id": "resp_123",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "Hello"},
                        {
                            "type": "function_call",
                            "id": "call_1",
                            "name": "get_weather",
                            "arguments": {"city": "Boston"}
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        }))
        .expect("parse response");

        assert_eq!(response.id, "resp_123");
        assert_eq!(response.content.len(), 2);
        assert!(matches!(response.content[0], ResponseContent::Text(_)));
        assert!(matches!(
            response.content[1],
            ResponseContent::ToolUse { .. }
        ));
    }

    #[test]
    fn parse_response_preserves_reasoning_text_as_metadata() {
        let response = parse_response(json!({
            "id": "resp_456",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "final"}
                    ],
                    "reasoning_content": "internal reasoning"
                }
            ],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 1
            }
        }))
        .expect("parse response");

        assert_eq!(response.metadata.extras.len(), 1);
        assert!(matches!(
            response.metadata.extras[0],
            ResponseExtra::ReasoningText { .. }
        ));
    }
}
