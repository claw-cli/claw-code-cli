use devo_core::{
    CommandExecutionItem, ContentBlock, Message, SessionRecord, TextItem, ToolCallItem,
    ToolResultItem, TurnItem, TurnRecord,
};
use devo_protocol::{SessionHistoryMetadata, SessionPlanStep, SessionPlanStepStatus};
use devo_utils::git_op::extract_paths_from_patch;
use devo_utils::shell_command::parse_command::parse_command;

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
        | TurnItem::WebSearch(TextItem { text })
        | TurnItem::ImageGeneration(TextItem { text })
        | TurnItem::HookPrompt(TextItem { text }) => Some(SessionHistoryItem::new(
            None,
            SessionHistoryItemKind::Assistant,
            String::new(),
            text.clone(),
        )),
        TurnItem::Plan(TextItem { text }) => {
            let metadata = parse_plan_history_metadata(text);
            let mut item = SessionHistoryItem::new(
                None,
                SessionHistoryItemKind::Assistant,
                String::new(),
                text.clone(),
            );
            if let Some(metadata) = metadata {
                item = item.with_metadata(metadata);
            }
            Some(item)
        }
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
        }) => {
            let title = summarize_tool_call(tool_name, input);
            let mut item = SessionHistoryItem::new(
                Some(tool_call_id.clone()),
                SessionHistoryItemKind::ToolCall,
                title.clone(),
                String::new(),
            );
            if matches!(tool_name.as_str(), "read" | "glob" | "grep") {
                let parsed = match tool_name.as_str() {
                    "read" => {
                        let path = input
                            .get("filePath")
                            .or_else(|| input.get("path"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let name = std::path::Path::new(path)
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.to_string());
                        vec![devo_protocol::parse_command::ParsedCommand::Read {
                            cmd: title.clone(),
                            name,
                            path: std::path::PathBuf::from(path),
                        }]
                    }
                    "glob" => vec![devo_protocol::parse_command::ParsedCommand::ListFiles {
                        cmd: title.clone(),
                        path: input
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                    }],
                    "grep" => vec![devo_protocol::parse_command::ParsedCommand::Search {
                        cmd: title.clone(),
                        query: input
                            .get("pattern")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                        path: input
                            .get("path")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                    }],
                    _ => Vec::new(),
                };
                item = item.with_metadata(SessionHistoryMetadata::Explored { actions: parsed });
            }
            Some(item)
        }
        TurnItem::ToolResult(ToolResultItem {
            tool_call_id,
            tool_name,
            output,
            display_content,
            is_error,
            ..
        }) => {
            let mut item = SessionHistoryItem::new(
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
            );
            if !*is_error
                && tool_name.as_deref() == Some("update_plan")
                && let Some(metadata) = match output {
                    serde_json::Value::String(text) => parse_plan_history_metadata(text),
                    other => parse_plan_history_metadata(&other.to_string()),
                }
            {
                item = item.with_metadata(metadata);
            }
            if !*is_error
                && matches!(tool_name.as_deref(), Some("apply_patch" | "write"))
                && let Some(metadata) = parse_edited_history_metadata(output)
            {
                item = item.with_metadata(metadata);
            }
            Some(item)
        }
        TurnItem::CommandExecution(CommandExecutionItem {
            tool_call_id,
            command,
            output,
            is_error,
            ..
        }) => {
            let parsed = parse_command(std::slice::from_ref(command));
            let mut item = SessionHistoryItem::new(
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
            );
            if !parsed.is_empty() {
                item = item.with_metadata(SessionHistoryMetadata::Explored { actions: parsed });
            }
            Some(item)
        }
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
                metadata: None,
                duration_ms: duration_secs,
            })
        }
    }
}

fn parse_plan_history_metadata(text: &str) -> Option<SessionHistoryMetadata> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let explanation = value
        .get("explanation")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|text| !text.trim().is_empty());
    let steps = value
        .get("plan")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .filter_map(|item| {
            let text = item.get("step")?.as_str()?.to_string();
            let status = match item.get("status").and_then(serde_json::Value::as_str)? {
                "pending" => SessionPlanStepStatus::Pending,
                "in_progress" => SessionPlanStepStatus::InProgress,
                "completed" => SessionPlanStepStatus::Completed,
                "cancelled" => SessionPlanStepStatus::Cancelled,
                _ => return None,
            };
            Some(SessionPlanStep { text, status })
        })
        .collect::<Vec<_>>();
    Some(SessionHistoryMetadata::PlanUpdate { explanation, steps })
}

fn parse_edited_history_metadata(output: &serde_json::Value) -> Option<SessionHistoryMetadata> {
    let files = output.get("files")?.as_array()?;
    let diff = output
        .get("diff")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut changes = std::collections::HashMap::new();
    for file in files {
        let path = std::path::PathBuf::from(file.get("path")?.as_str()?);
        let kind = file.get("kind")?.as_str()?;
        let additions = file
            .get("additions")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let deletions = file
            .get("deletions")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let change = match kind {
            "add" => devo_protocol::protocol::FileChange::Add {
                content: "\n".repeat(additions as usize),
            },
            "delete" => devo_protocol::protocol::FileChange::Delete {
                content: "\n".repeat(deletions as usize),
            },
            "update" | "move" => devo_protocol::protocol::FileChange::Update {
                unified_diff: file
                    .get("diff")
                    .or_else(|| file.get("patch"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| diff.clone()),
                move_path: file
                    .get("movePath")
                    .or_else(|| file.get("move_path"))
                    .and_then(serde_json::Value::as_str)
                    .map(std::path::PathBuf::from),
            },
            _ => continue,
        };
        changes.insert(path, change);
    }
    if changes.is_empty() && !diff.is_empty() {
        for path in extract_paths_from_patch(&diff) {
            changes.insert(
                std::path::PathBuf::from(path),
                devo_protocol::protocol::FileChange::Update {
                    unified_diff: diff.clone(),
                    move_path: None,
                },
            );
        }
    }
    (!changes.is_empty()).then_some(SessionHistoryMetadata::Edited { changes })
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
    use devo_core::TurnItem;
    use devo_core::{CommandExecutionItem, TextItem, ToolCallItem, ToolResultItem};
    use devo_protocol::{SessionHistoryMetadata, SessionPlanStepStatus};

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

    #[test]
    fn plan_turn_item_emits_structured_plan_metadata() {
        let item = TurnItem::Plan(TextItem {
            text: r#"{"explanation":"Do work","plan":[{"step":"Inspect","status":"completed"},{"step":"Patch","status":"in_progress"}]}"#.to_string(),
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        let SessionHistoryMetadata::PlanUpdate { explanation, steps } =
            history_item.metadata.expect("plan metadata")
        else {
            panic!("expected plan update metadata");
        };
        assert_eq!(explanation, Some("Do work".to_string()));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].status, SessionPlanStepStatus::Completed);
        assert_eq!(steps[1].status, SessionPlanStepStatus::InProgress);
    }

    #[test]
    fn command_execution_turn_item_emits_explored_metadata() {
        let item = TurnItem::CommandExecution(CommandExecutionItem {
            tool_call_id: "call-1".to_string(),
            tool_name: "exec_command".to_string(),
            command: "cat foo.txt".to_string(),
            input: serde_json::json!({}),
            output: serde_json::Value::String("hello".to_string()),
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        match history_item.metadata.expect("explored metadata") {
            SessionHistoryMetadata::Explored { actions } => {
                assert!(!actions.is_empty(), "expected parsed command actions");
            }
            other => panic!("unexpected metadata: {other:?}"),
        }
    }

    #[test]
    fn read_tool_call_turn_item_emits_explored_metadata() {
        let item = TurnItem::ToolCall(ToolCallItem {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            input: serde_json::json!({
                "filePath": "crates/tui/src/chatwidget.rs"
            }),
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        match history_item.metadata.expect("explored metadata") {
            SessionHistoryMetadata::Explored { actions } => {
                assert!(matches!(
                    &actions[0],
                    devo_protocol::parse_command::ParsedCommand::Read { name, .. }
                    if name == "chatwidget.rs"
                ));
            }
            other => panic!("unexpected metadata: {other:?}"),
        }
    }

    #[test]
    fn update_plan_tool_result_emits_plan_metadata() {
        let item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("update_plan".to_string()),
            output: serde_json::json!({
                "explanation": "",
                "plan": [
                    { "step": "创建一个示例计划，展示 plan 工具的使用方式", "status": "in_progress" },
                    { "step": "再添加一个已完成步骤作为对比", "status": "completed" },
                    { "step": "最后留一个待处理步骤", "status": "pending" }
                ]
            }),
            display_content: None,
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        let SessionHistoryMetadata::PlanUpdate { steps, .. } =
            history_item.metadata.expect("plan metadata")
        else {
            panic!("expected plan update metadata");
        };
        assert_eq!(steps.len(), 3);
    }

    #[test]
    fn write_tool_result_emits_edited_metadata() {
        let item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("write".to_string()),
            output: serde_json::json!({
                "diff": "diff --git a/foo.txt b/foo.txt\n--- a/foo.txt\n+++ b/foo.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "files": [
                    {
                        "path": "foo.txt",
                        "kind": "update",
                        "additions": 1,
                        "deletions": 1
                    }
                ]
            }),
            display_content: None,
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        let SessionHistoryMetadata::Edited { changes } =
            history_item.metadata.expect("edited metadata")
        else {
            panic!("expected edited metadata");
        };
        assert!(changes.contains_key(&std::path::PathBuf::from("foo.txt")));
    }

    #[test]
    fn write_tool_result_with_diff_only_still_emits_edited_metadata() {
        let item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("write".to_string()),
            output: serde_json::json!({
                "diff": "diff --git a/foo.txt b/foo.txt\n--- a/foo.txt\n+++ b/foo.txt\n@@ -1 +1 @@\n-old\n+new\n",
                "files": []
            }),
            display_content: None,
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        let SessionHistoryMetadata::Edited { changes } =
            history_item.metadata.expect("edited metadata")
        else {
            panic!("expected edited metadata");
        };
        assert!(changes.contains_key(&std::path::PathBuf::from("foo.txt")));
    }

    #[test]
    fn edited_metadata_prefers_file_local_diff_over_top_level_diff() {
        let item = TurnItem::ToolResult(ToolResultItem {
            tool_call_id: "call-1".to_string(),
            tool_name: Some("apply_patch".to_string()),
            output: serde_json::json!({
                "diff": "BROKEN TOP LEVEL DIFF",
                "files": [
                    {
                        "path": "foo.txt",
                        "kind": "update",
                        "diff": "diff --git a/foo.txt b/foo.txt\n--- a/foo.txt\n+++ b/foo.txt\n@@ -1 +1 @@\n-old\n+new\n",
                        "additions": 1,
                        "deletions": 1
                    }
                ]
            }),
            display_content: None,
            is_error: false,
        });

        let history_item = history_item_from_turn_item(&item).expect("history item");
        let SessionHistoryMetadata::Edited { changes } =
            history_item.metadata.expect("edited metadata")
        else {
            panic!("expected edited metadata");
        };
        let devo_protocol::protocol::FileChange::Update { unified_diff, .. } = changes
            .get(&std::path::PathBuf::from("foo.txt"))
            .expect("update change")
        else {
            panic!("expected update change");
        };
        assert!(unified_diff.contains("--- a/foo.txt"));
        assert!(!unified_diff.contains("BROKEN TOP LEVEL DIFF"));
    }
}
