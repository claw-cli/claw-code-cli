use devo_core::{
    CommandExecutionItem, ContentBlock, Message, SessionRecord, TextItem, ToolCallItem,
    ToolResultItem, TurnItem, TurnRecord,
};

use crate::session::{
    SessionHistoryItem, SessionHistoryItemKind, SessionMetadata, SessionRuntimeStatus,
};
use crate::turn::TurnMetadata;

/// Projects a canonical core session record into the API-visible session summary.
pub trait SessionProjector {
    /// Converts one core session record into a transport-facing session summary.
    fn project_session(
        &self,
        session: &SessionRecord,
        ephemeral: bool,
        status: SessionRuntimeStatus,
    ) -> SessionMetadata;
}

/// Projects a canonical core turn record into the API-visible turn summary.
pub trait TurnProjector {
    /// Converts one core turn record into a transport-facing turn summary.
    fn project_turn(&self, turn: &TurnRecord) -> TurnMetadata;
}

/// Default projector that performs field-by-field protocol projection.
#[derive(Debug, Clone, Default)]
pub struct DefaultProjection;

impl DefaultProjection {
    /// Converts replayed core conversation messages into a client-facing transcript snapshot.
    pub fn project_history(&self, messages: &[Message]) -> Vec<SessionHistoryItem> {
        let mut history = Vec::new();
        for message in messages {
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } if !text.is_empty() => {
                        let kind = if message.role == devo_core::Role::User {
                            SessionHistoryItemKind::User
                        } else {
                            SessionHistoryItemKind::Assistant
                        };
                        history.push(SessionHistoryItem::new(
                            None,
                            kind,
                            String::new(),
                            text.clone(),
                        ));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        history.push(SessionHistoryItem::new(
                            Some(id.clone()),
                            SessionHistoryItemKind::ToolCall,
                            summarize_tool_call(name, input),
                            String::new(),
                        ));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    } => {
                        history.push(SessionHistoryItem::new(
                            Some(tool_use_id.clone()),
                            if *is_error {
                                SessionHistoryItemKind::Error
                            } else {
                                SessionHistoryItemKind::ToolResult
                            },
                            if *is_error {
                                "Tool error".to_string()
                            } else {
                                "Tool output".to_string()
                            },
                            content.clone(),
                        ));
                    }
                    ContentBlock::Reasoning { text } if !text.is_empty() => {
                        history.push(SessionHistoryItem::new(
                            None,
                            SessionHistoryItemKind::Reasoning,
                            String::new(),
                            text.clone(),
                        ));
                    }
                    ContentBlock::Reasoning { .. } => {}
                    ContentBlock::Text { .. } => {}
                }
            }
        }
        history
    }
}

/// Projects one canonical persisted turn item into one replay-friendly history item when visible.
pub(crate) fn history_item_from_turn_item(item: &TurnItem) -> Option<SessionHistoryItem> {
    match item {
        TurnItem::UserMessage(TextItem { text }) | TurnItem::SteerInput(TextItem { text }) => {
            Some(SessionHistoryItem::new(
                None,
                SessionHistoryItemKind::User,
                String::new(),
                text.clone(),
            ))
        }
        TurnItem::AgentMessage(TextItem { text })
        | TurnItem::Plan(TextItem { text })
        | TurnItem::WebSearch(TextItem { text })
        | TurnItem::ImageGeneration(TextItem { text })
        | TurnItem::HookPrompt(TextItem { text }) => Some(SessionHistoryItem::new(
            None,
            SessionHistoryItemKind::Assistant,
            String::new(),
            text.clone(),
        )),
        TurnItem::ContextCompaction(TextItem { .. }) => None,
        TurnItem::Reasoning(TextItem { text }) => Some(SessionHistoryItem::new(
            None,
            SessionHistoryItemKind::Reasoning,
            String::new(),
            text.clone(),
        )),
        TurnItem::ToolCall(ToolCallItem {
            tool_call_id,
            tool_name,
            input,
        }) => Some(SessionHistoryItem::new(
            Some(tool_call_id.clone()),
            SessionHistoryItemKind::ToolCall,
            summarize_tool_call(tool_name, input),
            String::new(),
        )),
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            tool_name,
            output,
            display_content,
            is_error,
            ..
        }) => Some(SessionHistoryItem::new(
            Some(tool_call_id.clone()),
            if *is_error {
                SessionHistoryItemKind::Error
            } else {
                SessionHistoryItemKind::ToolResult
            },
            summarize_tool_result(tool_name.as_deref(), *is_error),
            display_content.clone().unwrap_or_else(|| match output {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            }),
        )),
        TurnItem::CommandExecution(CommandExecutionItem {
            tool_call_id,
            command,
            output,
            is_error,
            ..
        }) => Some(SessionHistoryItem::new(
            Some(tool_call_id.clone()),
            if *is_error {
                SessionHistoryItemKind::Error
            } else {
                SessionHistoryItemKind::CommandExecution
            },
            command.clone(),
            match output {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            },
        )),
        TurnItem::ToolProgress(_)
        | TurnItem::ApprovalRequest(_)
        | TurnItem::ApprovalDecision(_) => None,
        TurnItem::TurnSummary(TextItem { text }) => {
            // Format: "model_name:duration_secs" or just "model_name"
            let (model_name, duration_secs) = match text.split_once(':') {
                Some((model, dur)) => (model.to_string(), dur.parse::<u64>().ok()),
                None => (text.clone(), None),
            };
            Some(SessionHistoryItem {
                tool_call_id: None,
                kind: SessionHistoryItemKind::TurnSummary,
                title: model_name,
                body: String::new(),
                duration_ms: duration_secs,
            })
        }
    }
}

impl SessionProjector for DefaultProjection {
    fn project_session(
        &self,
        session: &SessionRecord,
        ephemeral: bool,
        status: SessionRuntimeStatus,
    ) -> SessionMetadata {
        SessionMetadata {
            session_id: session.id,
            cwd: session.cwd.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            title: session.title.clone(),
            title_state: session.title_state.clone(),
            ephemeral,
            model: session.model.clone(),
            thinking: session.thinking.clone(),
            reasoning_effort: session
                .latest_turn_context
                .as_ref()
                .and_then(|context| context.reasoning_effort)
                .or_else(|| {
                    session
                        .session_context
                        .as_ref()
                        .and_then(|context| context.reasoning_effort)
                }),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            prompt_token_estimate: 0,
            last_query_total_tokens: 0,
            status,
        }
    }
}

impl TurnProjector for DefaultProjection {
    fn project_turn(&self, turn: &TurnRecord) -> TurnMetadata {
        TurnMetadata {
            turn_id: turn.id,
            session_id: turn.session_id,
            sequence: turn.sequence,
            status: turn.status.clone(),
            kind: turn.kind.clone(),
            model: turn.model.clone(),
            thinking: turn.thinking.clone(),
            reasoning_effort: turn
                .turn_context
                .as_ref()
                .and_then(|context| context.reasoning_effort)
                .or_else(|| {
                    turn.session_context
                        .as_ref()
                        .and_then(|context| context.reasoning_effort)
                }),
            request_model: turn.request_model.clone(),
            request_thinking: turn.request_thinking.clone(),
            started_at: turn.started_at,
            completed_at: turn.completed_at,
            usage: turn.usage.clone(),
        }
    }
}

fn summarize_tool_call(tool_name: &str, input: &serde_json::Value) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    devo_tools::tool_summary::tool_summary(tool_name, input, &cwd).replacen(": ", " ", 1)
}

fn summarize_tool_result(tool_name: Option<&str>, is_error: bool) -> String {
    match (tool_name, is_error) {
        (Some(tool_name), true) => format!("{tool_name} error"),
        (Some(tool_name), false) => format!("{tool_name} output"),
        (None, true) => "Tool error".to_string(),
        (None, false) => "Tool output".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::history_item_from_turn_item;
    use crate::session::SessionHistoryItemKind;
    use devo_core::ToolResultItem;
    use devo_core::TurnItem;

    #[test]
    fn history_projection_prefers_tool_result_display_content() {
        let item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("read".to_string()),
            output: serde_json::Value::String("<content>canonical</content>".to_string()),
            display_content: Some("canonical".to_string()),
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        assert_eq!(history_item.kind, SessionHistoryItemKind::ToolResult);
        assert_eq!(history_item.title, "read output");
        assert_eq!(history_item.body, "canonical");
    }

    #[test]
    fn history_projection_falls_back_to_tool_result_output() {
        let item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("read".to_string()),
            output: serde_json::Value::String("<content>canonical</content>".to_string()),
            display_content: None,
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        assert_eq!(history_item.body, "<content>canonical</content>");
    }
}
