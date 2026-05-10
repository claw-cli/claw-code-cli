use serde::{Deserialize, Serialize};

/// The output returned by a tool after execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            metadata: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            metadata: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_success() {
        let out = ToolOutput::success("done");
        assert_eq!(out.content, "done");
        assert!(!out.is_error);
        assert!(out.metadata.is_none());
    }

    #[test]
    fn tool_output_error() {
        let out = ToolOutput::error("failed");
        assert_eq!(out.content, "failed");
        assert!(out.is_error);
        assert!(out.metadata.is_none());
    }

    #[test]
    fn tool_output_serde_roundtrip() {
        let out = ToolOutput {
            content: "hello".into(),
            is_error: false,
            metadata: Some(serde_json::json!({"key": "val"})),
        };
        let json = serde_json::to_string(&out).unwrap();
        let deserialized: ToolOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.content, "hello");
        assert!(!deserialized.is_error);
        assert!(deserialized.metadata.is_some());
    }
}
