use devo_protocol::RequestRole;
use devo_protocol::ToolDefinition;
use reqwest::Response;
use reqwest::StatusCode;
use serde_json::Value;
use serde_json::json;
use tracing::warn;

use super::OpenAIRole;
use super::capabilities::OpenAIReasoningMode;
use super::capabilities::OpenAIRequestProfile;
use devo_protocol::ReasoningEffort;

pub(crate) fn request_role(role: &str) -> OpenAIRole {
    match role.parse::<RequestRole>() {
        Ok(RequestRole::System) => OpenAIRole::System,
        Ok(RequestRole::Developer) => OpenAIRole::Developer,
        Ok(RequestRole::User) => OpenAIRole::User,
        Ok(RequestRole::Assistant) => OpenAIRole::Assistant,
        Ok(RequestRole::Tool) => OpenAIRole::Tool,
        Ok(RequestRole::Function) => OpenAIRole::Function,
        Err(_) => {
            warn!(
                role = role,
                fallback = "user",
                "unknown OpenAI request role; defaulting to user"
            );
            OpenAIRole::User
        }
    }
}

pub(crate) enum OpenAIReasoningValue {
    Effort(ReasoningEffort),
    Thinking {
        enabled: bool,
    },
    ThinkingWithEffort {
        enabled: bool,
        effort: Option<ReasoningEffort>,
    },
}

pub(crate) fn reasoning_value(
    profile: OpenAIRequestProfile,
    thinking: Option<&str>,
    reasoning_effort: Option<ReasoningEffort>,
) -> Option<OpenAIReasoningValue> {
    match profile.reasoning_mode {
        OpenAIReasoningMode::Effort => reasoning_effort.map(OpenAIReasoningValue::Effort),
        OpenAIReasoningMode::Thinking => {
            let enabled = !matches!(
                thinking
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "disabled" | "none"
            );
            Some(OpenAIReasoningValue::Thinking { enabled })
        }
        OpenAIReasoningMode::ThinkingWithEffort => {
            let enabled = !matches!(
                thinking
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "disabled" | "none"
            );
            Some(OpenAIReasoningValue::ThinkingWithEffort {
                enabled,
                effort: if enabled { reasoning_effort } else { None },
            })
        }
    }
}

pub(crate) fn tool_definitions(tools: &[ToolDefinition]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let mut function = json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                });
                if let Some(output_schema) = &tool.output_schema {
                    function["output_schema"] = output_schema.clone();
                }
                json!({
                    "type": "function",
                    "function": function
                })
            })
            .collect(),
    )
}

pub(crate) async fn invalid_status_error(
    provider: &'static str,
    model: &str,
    operation: &str,
    status: StatusCode,
    response: Response,
    request_body: &Value,
) -> anyhow::Error {
    let response_body = response
        .text()
        .await
        .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
    warn!(
        provider,
        model,
        operation,
        status = %status,
        http_body = %request_body,
        response_body = %response_body,
        "provider request failed"
    );
    anyhow::anyhow!(
        "{provider} {operation} error for model {model}: Invalid status code: {status}; response body: {response_body}"
    )
}
