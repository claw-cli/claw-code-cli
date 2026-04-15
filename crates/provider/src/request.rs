use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A normalized message role used by provider adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
    Function,
}

impl RequestRole {
    /// Returns the stable wire label for this role.
    pub fn as_str(self) -> &'static str {
        match self {
            RequestRole::System => "system",
            RequestRole::Developer => "developer",
            RequestRole::User => "user",
            RequestRole::Assistant => "assistant",
            RequestRole::Tool => "tool",
            RequestRole::Function => "function",
        }
    }
}

impl fmt::Display for RequestRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RequestRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Ok(RequestRole::System),
            "developer" => Ok(RequestRole::Developer),
            "user" => Ok(RequestRole::User),
            "assistant" => Ok(RequestRole::Assistant),
            "tool" => Ok(RequestRole::Tool),
            "function" => Ok(RequestRole::Function),
            other => Err(format!("invalid request role: {other}")),
        }
    }
}

/// A tool definition sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A content block within a message sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RequestContent {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// A message in the request to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    pub role: String,
    pub content: Vec<RequestContent>,
}

/// Full request to the model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<RequestMessage>,
    pub max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(default)]
    pub sampling: SamplingControls,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,
}

/// Merges an extra JSON object into a provider request body.
pub fn merge_extra_body(body: &mut Value, extra_body: Option<&Value>) {
    let Some(extra_body) = extra_body else {
        return;
    };
    let Some(body_object) = body.as_object_mut() else {
        return;
    };
    let Some(extra_object) = extra_body.as_object() else {
        return;
    };

    for (key, value) in extra_object {
        body_object.insert(key.clone(), value.clone());
    }
}

/// Sampling controls and model-selection hints shared across adapters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingControls {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_definition_serde_roundtrip() {
        let def = ToolDefinition {
            name: "bash".into(),
            description: "run commands".into(),
            input_schema: json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "bash");
        assert_eq!(deserialized.description, "run commands");
    }

    #[test]
    fn request_content_text_serde() {
        let content = RequestContent::Text {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"text""#));
        let deserialized: RequestContent = serde_json::from_str(&json).unwrap();
        match deserialized {
            RequestContent::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn request_content_tool_result_skips_none_error() {
        let content = RequestContent::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: None,
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(!json.contains("is_error"));
    }

    #[test]
    fn request_content_tool_result_includes_error() {
        let content = RequestContent::ToolResult {
            tool_use_id: "t1".into(),
            content: "failed".into(),
            is_error: Some(true),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("is_error"));
    }

    #[test]
    fn model_request_serde() {
        let req = ModelRequest {
            model: "claude-sonnet-4-20250514".into(),
            system: Some("You are helpful.".into()),
            messages: vec![RequestMessage {
                role: "user".into(),
                content: vec![RequestContent::Text { text: "hi".into() }],
            }],
            max_tokens: 4096,
            tools: None,
            sampling: SamplingControls::default(),
            thinking: None,
            extra_body: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("tools"));
        assert!(!json.contains("temperature"));
        let deserialized: ModelRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.model, "claude-sonnet-4-20250514");
        assert_eq!(deserialized.messages.len(), 1);
    }

    #[test]
    fn merge_extra_body_overrides_existing_fields() {
        let mut body = json!({
            "model": "base-model",
            "temperature": 0.2
        });
        let extra = json!({
            "temperature": 0.8,
            "top_k": 32
        });

        merge_extra_body(&mut body, Some(&extra));

        assert_eq!(
            body,
            json!({
                "model": "base-model",
                "temperature": 0.8,
                "top_k": 32
            })
        );
    }

    #[test]
    fn request_role_roundtrip() {
        for role in [
            RequestRole::System,
            RequestRole::Developer,
            RequestRole::User,
            RequestRole::Assistant,
            RequestRole::Tool,
            RequestRole::Function,
        ] {
            let rendered = role.as_str();
            let parsed: RequestRole = rendered.parse().unwrap();
            assert_eq!(parsed, role);
        }
    }
}
