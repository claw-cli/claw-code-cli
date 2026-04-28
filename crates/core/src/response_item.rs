use serde::{Deserialize, Serialize};

use devo_protocol::{ContentBlock, Message, RequestContent, RequestMessage, Role};

/// Unified representation of LLM conversation items.
///
/// This is the core IR that the history management system operates on,
/// bridging provider-agnostic protocol types with normalization and
/// compaction workflows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResponseItem {
    /// Model reasoning / thinking output.
    /// Some models produce this, others do not.
    Reason { text: String },
    /// A user-sent message or model reply, containing text (and image in future).
    Message(Message),
    /// A model tool-call request.
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The result/output of a tool call.
    ToolCallOutput {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

impl ResponseItem {
    /// Returns `true` if this item is a `Reason` variant.
    pub fn is_reason(&self) -> bool {
        matches!(self, Self::Reason { .. })
    }

    /// Returns `true` if this item is a `Message` variant.
    pub fn is_message(&self) -> bool {
        matches!(self, Self::Message(..))
    }

    /// Returns `true` if this item is a `ToolCall` variant.
    pub fn is_tool_call(&self) -> bool {
        matches!(self, Self::ToolCall { .. })
    }

    /// Returns the tool-use id if this is a `ToolCall`.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Self::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        }
    }

    /// Returns `true` if this item is a `ToolCallOutput` variant.
    pub fn is_tool_call_output(&self) -> bool {
        matches!(self, Self::ToolCallOutput { .. })
    }

    /// Returns the tool-use id if this is a `ToolCallOutput`.
    pub fn tool_call_output_id(&self) -> Option<&str> {
        match self {
            Self::ToolCallOutput { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Conversions: from ContentBlock -> ResponseItem (partial)
// ---------------------------------------------------------------------------

impl From<ContentBlock> for ResponseItem {
    fn from(block: ContentBlock) -> Self {
        match block {
            ContentBlock::Text { text } => Self::Message(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text { text }],
            }),
            ContentBlock::Reasoning { text } => Self::Reason { text },
            ContentBlock::ToolUse { id, name, input } => Self::ToolCall { id, name, input },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Self::ToolCallOutput {
                tool_use_id,
                content,
                is_error,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Conversions: from ResponseItem -> RequestMessage (for LLM prompt building)
// ---------------------------------------------------------------------------

impl From<ResponseItem> for RequestMessage {
    fn from(item: ResponseItem) -> Self {
        match item {
            ResponseItem::Reason { text } => RequestMessage {
                role: Role::Assistant.as_str().to_string(),
                content: vec![RequestContent::Reasoning { text }],
            },
            ResponseItem::Message(msg) => msg.to_request_message(),
            ResponseItem::ToolCall { id, name, input } => RequestMessage {
                role: Role::Assistant.as_str().to_string(),
                content: vec![RequestContent::ToolUse { id, name, input }],
            },
            ResponseItem::ToolCallOutput {
                tool_use_id,
                content,
                is_error,
            } => RequestMessage {
                role: Role::User.as_str().to_string(),
                content: vec![RequestContent::ToolResult {
                    tool_use_id,
                    content,
                    is_error: if is_error { Some(true) } else { None },
                }],
            },
        }
    }
}

impl From<&ResponseItem> for RequestMessage {
    fn from(item: &ResponseItem) -> Self {
        match item {
            ResponseItem::Reason { text } => RequestMessage {
                role: Role::Assistant.as_str().to_string(),
                content: vec![RequestContent::Reasoning {
                    text: text.clone(),
                }],
            },
            ResponseItem::Message(msg) => msg.to_request_message(),
            ResponseItem::ToolCall { id, name, input } => RequestMessage {
                role: Role::Assistant.as_str().to_string(),
                content: vec![RequestContent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }],
            },
            ResponseItem::ToolCallOutput {
                tool_use_id,
                content,
                is_error,
            } => RequestMessage {
                role: Role::User.as_str().to_string(),
                content: vec![RequestContent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: if *is_error { Some(true) } else { None },
                }],
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Converting a full Message into ResponseItem(s)
// ---------------------------------------------------------------------------

/// Converts a `Message` into one or more `ResponseItem`s.
///
/// A `Message` can contain multiple content blocks of different types.
/// This split representation is useful for normalization (e.g. pairing
/// tool calls with their outputs) and modality-based filtering.
pub fn message_to_response_items(msg: Message) -> Vec<ResponseItem> {
    let role = msg.role;
    let mut items = Vec::new();

    for block in msg.content {
        match block {
            ContentBlock::Text { text } => {
                // Aggregate consecutive text blocks into one message, but
                // since we iterate, each text block becomes a separate Message item.
                // In practice, the assistant typically has one text block per message.
                items.push(ResponseItem::Message(Message {
                    role,
                    content: vec![ContentBlock::Text { text }],
                }));
            }
            ContentBlock::Reasoning { text } => {
                items.push(ResponseItem::Reason { text });
            }
            ContentBlock::ToolUse { id, name, input } => {
                items.push(ResponseItem::ToolCall { id, name, input });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                items.push(ResponseItem::ToolCallOutput {
                    tool_use_id,
                    content,
                    is_error,
                });
            }
        }
    }

    items
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use devo_protocol::{ContentBlock, Message, Role};

    #[test]
    fn response_item_reason_variant() {
        let item = ResponseItem::Reason {
            text: "thinking".into(),
        };
        assert!(item.is_reason());
        assert!(!item.is_message());
        assert!(!item.is_tool_call());
        assert!(!item.is_tool_call_output());
    }

    #[test]
    fn response_item_message_variant() {
        let msg = Message::user("hello");
        let item = ResponseItem::Message(msg);
        assert!(item.is_message());
        assert!(!item.is_reason());
    }

    #[test]
    fn response_item_tool_call_variant() {
        let item = ResponseItem::ToolCall {
            id: "call-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        };
        assert!(item.is_tool_call());
        assert_eq!(item.tool_call_id(), Some("call-1"));
    }

    #[test]
    fn response_item_tool_call_output_variant() {
        let item = ResponseItem::ToolCallOutput {
            tool_use_id: "call-1".into(),
            content: "ok".into(),
            is_error: false,
        };
        assert!(item.is_tool_call_output());
        assert_eq!(item.tool_call_output_id(), Some("call-1"));
    }

    #[test]
    fn from_content_block_reasoning() {
        let block = ContentBlock::Reasoning {
            text: "deep thought".into(),
        };
        let item: ResponseItem = block.into();
        assert!(item.is_reason());
    }

    #[test]
    fn from_content_block_tool_use() {
        let block = ContentBlock::ToolUse {
            id: "tu-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "pwd"}),
        };
        let item: ResponseItem = block.into();
        assert!(item.is_tool_call());
        assert_eq!(item.tool_call_id(), Some("tu-1"));
    }

    #[test]
    fn from_content_block_tool_result() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tu-1".into(),
            content: "result".into(),
            is_error: false,
        };
        let item: ResponseItem = block.into();
        assert!(item.is_tool_call_output());
        assert_eq!(item.tool_call_output_id(), Some("tu-1"));
    }

    #[test]
    fn response_item_to_request_message_reason() {
        let item = ResponseItem::Reason {
            text: "thinking".into(),
        };
        let req: RequestMessage = item.into();
        assert_eq!(req.role, "assistant");
        assert_eq!(req.content.len(), 1);
    }

    #[test]
    fn response_item_to_request_message_message() {
        let item = ResponseItem::Message(Message::user("hello"));
        let req: RequestMessage = item.into();
        assert_eq!(req.role, "user");
    }

    #[test]
    fn response_item_to_request_message_tool_call() {
        let item = ResponseItem::ToolCall {
            id: "tc-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        };
        let req: RequestMessage = item.into();
        assert_eq!(req.role, "assistant");
    }

    #[test]
    fn response_item_to_request_message_tool_output() {
        let item = ResponseItem::ToolCallOutput {
            tool_use_id: "tc-1".into(),
            content: "done".into(),
            is_error: false,
        };
        let req: RequestMessage = item.into();
        assert_eq!(req.role, "user");
    }

    #[test]
    fn message_to_response_items_splits_mixed_content() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "hmm".into(),
                },
                ContentBlock::Text {
                    text: "hello".into(),
                },
                ContentBlock::ToolUse {
                    id: "tu-1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"cmd": "ls"}),
                },
            ],
        };

        let items = message_to_response_items(msg);
        assert_eq!(items.len(), 3);
        assert!(items[0].is_reason());
        assert!(items[1].is_message());
        assert!(items[2].is_tool_call());
    }

    #[test]
    fn response_item_serde_roundtrip() {
        let items = vec![
            ResponseItem::Reason {
                text: "thinking".into(),
            },
            ResponseItem::Message(Message::user("hello")),
            ResponseItem::ToolCall {
                id: "tc-1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            },
            ResponseItem::ToolCallOutput {
                tool_use_id: "tc-1".into(),
                content: "done".into(),
                is_error: false,
            },
        ];

        for item in &items {
            let json = serde_json::to_string(item).expect("serialize");
            let restored: ResponseItem = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*item, restored);
        }
    }
}
