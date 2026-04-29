use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{RequestContent, RequestMessage};

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = uuid::Error;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Ok(Self(Uuid::parse_str(value)?))
            }
        }

        impl TryFrom<String> for $name {
            type Error = uuid::Error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_from(value.as_str())
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_from(s)
            }
        }
    };
}

define_id!(SessionId);
define_id!(TurnId);
define_id!(ItemId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionTitleState {
    Unset,
    Provisional,
    Final(SessionTitleFinalSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionTitleFinalSource {
    ModelGenerated,
    UserRename,
    ExplicitCreate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Pending,
    Running,
    WaitingApproval,
    Interrupted,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "reasoning")]
    Reasoning { text: String },
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
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn tool_uses(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                ContentBlock::Text { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::ToolResult { .. } => None,
            })
            .collect()
    }

    pub fn to_request_message(&self) -> RequestMessage {
        let content = self
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => RequestContent::Text { text: text.clone() },
                ContentBlock::Reasoning { text } => {
                    RequestContent::Reasoning { text: text.clone() }
                }
                ContentBlock::ToolUse { id, name, input } => RequestContent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                },
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => RequestContent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: if *is_error { Some(true) } else { None },
                },
            })
            .collect();

        RequestMessage {
            role: self.role.as_str().to_string(),
            content,
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn role_system_as_str() {
        assert_eq!(Role::System.as_str(), "system");
    }

    #[test]
    fn role_user_as_str() {
        assert_eq!(Role::User.as_str(), "user");
    }

    #[test]
    fn role_assistant_as_str() {
        assert_eq!(Role::Assistant.as_str(), "assistant");
    }

    #[test]
    fn message_system_creates_system_role() {
        let msg = Message::system("budget notice");
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.content.len(), 1);
        assert!(
            matches!(msg.content[0], ContentBlock::Text { ref text } if text == "budget notice")
        );
    }

    #[test]
    fn message_system_to_request_message() {
        let msg = Message::system("system instruction");
        let req = msg.to_request_message();
        assert_eq!(req.role, "system");
    }

    #[test]
    fn message_user_to_request_message_role() {
        let msg = Message::user("hello");
        let req = msg.to_request_message();
        assert_eq!(req.role, "user");
    }
}
