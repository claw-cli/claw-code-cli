use std::{collections::HashMap, pin::Pin};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest_eventsource::{Event, EventSource};
use serde_json::{Value, json};
use tracing::debug;

use crate::{
    ModelProviderSDK, ModelRequest, ModelResponse, RequestContent, ResponseContent, ResponseExtra,
    ResponseMetadata, StopReason, StreamEvent, Usage, merge_extra_body,
};

use super::capabilities::{OpenAITransport, resolve_request_profile};
use super::{
    OpenAIRole,
    shared::{reasoning_effort, request_role, tool_definitions},
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

    if let Some(reasoning) = reasoning_effort(request.thinking.as_deref()) {
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
        .filter_map(|block| match block {
            RequestContent::Text { text } => Some(json!({
                "type": "input_text",
                "text": text,
            })),
            RequestContent::ToolUse { id, name, input } => Some(json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "input": input,
            })),
            RequestContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(json!({
                "type": "function_call_output",
                "call_id": tool_use_id,
                "output": content,
                "is_error": is_error,
            })),
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
        Some("output_text") | Some("text") | Some("input_text") => Some(ResponseContent::Text(
            item.get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )),
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
            .context("failed to send openai responses request")?
            .error_for_status()
            .context("openai responses request failed")?;

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
            let mut response_id = String::new();
            let mut tool_calls: HashMap<String, (String, String, String)> = HashMap::new();
            let mut usage: Option<Usage> = None;

            futures::pin_mut!(event_source);
            while let Some(event) = event_source.next().await {
                let event = event.map_err(|error| {
                    anyhow::anyhow!("openai responses stream error for model {}: {error}", request.model)
                })?;

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
                                    if text_buf.is_empty() {
                                        yield StreamEvent::ContentBlockStart {
                                            index: 0,
                                            content: ResponseContent::Text(String::new()),
                                        };
                                    }
                                    text_buf.push_str(delta);
                                    yield StreamEvent::TextDelta {
                                        index: 0,
                                        text: delta.to_string(),
                                    };
                                }
                            }
                            "response.output_item.added" => {
                                if let Some(item) = chunk.get("item") {
                                    if let Some(ResponseContent::ToolUse { id, name, input }) = parse_output_item(item).into_iter().next() {
                                        let key = id.clone();
                                        tool_calls.insert(key.clone(), (id.clone(), name.clone(), input.to_string()));
                                        yield StreamEvent::ContentBlockStart {
                                            index: tool_calls.len(),
                                            content: ResponseContent::ToolUse { id, name, input },
                                        };
                                    }
                                }
                            }
                            "response.completed" | "response.done" => {
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
                                        metadata: ResponseMetadata::default(),
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
                metadata: ResponseMetadata::default(),
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
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::parse_response;
    use crate::{
        ModelRequest, RequestContent, RequestMessage, ResponseContent, ResponseExtra,
        SamplingControls, ToolDefinition, openai::responses::build_request,
    };

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
            }]),
            sampling: SamplingControls {
                temperature: Some(0.4),
                top_p: Some(0.7),
                top_k: Some(12),
            },
            thinking: Some("medium".to_string()),
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
