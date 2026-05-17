use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use super::*;
use crate::{FileChangePayload, TurnPlanStepPayload, TurnPlanUpdatedPayload};
use devo_utils::git_op::extract_paths_from_patch;

struct PendingToolCall {
    item_id: ItemId,
    item_seq: u64,
    input: serde_json::Value,
    is_command_execution: bool,
    command: String,
}

async fn complete_reasoning_item(
    runtime: &Arc<ServerRuntime>,
    session_id: SessionId,
    turn_id: TurnId,
    item_id: ItemId,
    item_seq: u64,
    text: String,
) {
    runtime
        .complete_item(
            session_id,
            turn_id,
            item_id,
            item_seq,
            ItemKind::Reasoning,
            TurnItem::Reasoning(TextItem { text: text.clone() }),
            serde_json::json!({ "title": "Reasoning", "text": text }),
        )
        .await;
}

async fn complete_assistant_item(
    runtime: &Arc<ServerRuntime>,
    session_id: SessionId,
    turn_id: TurnId,
    item_id: ItemId,
    item_seq: u64,
    text: String,
) {
    runtime
        .complete_item(
            session_id,
            turn_id,
            item_id,
            item_seq,
            ItemKind::AgentMessage,
            TurnItem::AgentMessage(TextItem { text: text.clone() }),
            serde_json::json!({ "title": "Assistant", "text": text }),
        )
        .await;
}

fn is_unified_exec_tool(name: &str) -> bool {
    matches!(name, "exec_command" | "write_stdin")
}

fn is_file_change_tool(name: &str) -> bool {
    matches!(name, "apply_patch" | "write")
}

fn is_plan_tool(name: &str) -> bool {
    matches!(name, "update_plan")
}

fn command_display_from_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "exec_command" => input
            .get("cmd")
            .or_else(|| input.get("command"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "write_stdin" => {
            let session_id = input
                .get("session_id")
                .and_then(serde_json::Value::as_i64)
                .map(|id| id.to_string())
                .unwrap_or_else(|| "?".to_string());
            let chars = input
                .get("chars")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if chars.is_empty() {
                format!("poll session {session_id}")
            } else {
                format!("write_stdin session {session_id}")
            }
        }
        "read" => {
            let path = input
                .get("filePath")
                .or_else(|| input.get("path"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            format!("read {path}")
        }
        "glob" => {
            let pattern = input
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let path = input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if path.is_empty() {
                format!("glob {pattern}")
            } else {
                format!("glob {pattern} in {path}")
            }
        }
        "grep" => {
            let pattern = input
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let path = input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if path.is_empty() {
                format!("grep {pattern}")
            } else {
                format!("grep {pattern} in {path}")
            }
        }
        _ => String::new(),
    }
}

fn command_actions_from_tool_input(
    tool_name: &str,
    command: &str,
    input: &serde_json::Value,
) -> Vec<devo_protocol::parse_command::ParsedCommand> {
    match tool_name {
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
                cmd: command.to_string(),
                name,
                path: std::path::PathBuf::from(path),
            }]
        }
        "glob" => vec![devo_protocol::parse_command::ParsedCommand::ListFiles {
            cmd: command.to_string(),
            path: input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
        }],
        "grep" => vec![devo_protocol::parse_command::ParsedCommand::Search {
            cmd: command.to_string(),
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
    }
}

fn command_execution_item_id_for_progress(
    pending_tool_calls: &HashMap<String, PendingToolCall>,
    tool_use_id: &str,
) -> Option<ItemId> {
    pending_tool_calls
        .get(tool_use_id)
        .filter(|pending| pending.is_command_execution)
        .map(|pending| pending.item_id)
}

impl ServerRuntime {
    /// Execute one turn end-to-end, including streaming query events,
    /// persisting turn state, and draining queued follow-up inputs.
    pub(super) async fn execute_turn(
        self: Arc<Self>,
        session_id: SessionId,
        turn: TurnMetadata,
        turn_config: TurnConfig,
        display_input: String,
        input: String,
    ) {
        if let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() {
            session_arc.lock().await.turn_approval_cache =
                crate::execution::ApprovalGrantCache::default();
        }
        // Record the user's message immediately so the UI can show it even if
        // the model call or event stream takes a moment to start.
        self.emit_turn_item(
            session_id,
            turn.turn_id,
            ItemKind::UserMessage,
            TurnItem::UserMessage(TextItem {
                text: display_input.clone(),
            }),
            serde_json::json!({ "title": "You", "text": display_input.clone() }),
        )
        .await;

        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<QueryEvent>();
        let runtime = Arc::clone(&self);
        let turn_for_events = turn.clone();
        let turn_for_plan_updates = turn.clone();
        let event_session_arc = Arc::clone(&session_arc);
        let event_task = tokio::spawn(async move {
            // This task owns the streamed model output. It turns raw query
            // callbacks into persisted turn items and keeps enough state to
            // resume cleanly if the turn is interrupted mid-stream.
            let mut assistant_item_id = None;
            let mut assistant_item_seq = None;
            let mut assistant_text = String::new();
            let mut reasoning_item_id = None;
            let mut reasoning_item_seq = None;
            let mut reasoning_text = String::new();
            let mut tool_names_by_id = HashMap::new();
            let mut pending_tool_calls: HashMap<String, PendingToolCall> = HashMap::new();
            let mut latest_usage: Option<TurnUsage> = None;
            let mut usage_base: Option<(usize, usize, usize)> = None;
            while let Some(event) = event_rx.recv().await {
                match event {
                    QueryEvent::TextDelta(text) => {
                        let (item_id, item_seq) = match (assistant_item_id, assistant_item_seq) {
                            (Some(item_id), Some(item_seq)) => (item_id, item_seq),
                            (None, None) => {
                                let (item_id, item_seq) = runtime
                                    .start_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        ItemKind::AgentMessage,
                                        serde_json::json!({ "title": "Assistant", "text": "" }),
                                    )
                                    .await;
                                assistant_item_id = Some(item_id);
                                assistant_item_seq = Some(item_seq);
                                (item_id, item_seq)
                            }
                            _ => continue,
                        };
                        assistant_text.push_str(&text);
                        runtime
                            .broadcast_event(ServerEvent::ItemDelta {
                                delta_kind: ItemDeltaKind::AgentMessageDelta,
                                payload: ItemDeltaPayload {
                                    context: EventContext {
                                        session_id,
                                        turn_id: Some(turn_for_events.turn_id),
                                        item_id: Some(item_id),
                                        seq: 0,
                                    },
                                    delta: text,
                                    stream_index: None,
                                    channel: None,
                                },
                            })
                            .await;
                        let _ = item_seq;

                        // Store deferred completion info for interrupt recovery
                        if let Ok(mut session) = event_session_arc.try_lock() {
                            session.deferred_assistant =
                                Some((item_id, item_seq, assistant_text.clone()));
                        }
                    }
                    QueryEvent::ReasoningDelta(text) => {
                        let (item_id, item_seq) = match (reasoning_item_id, reasoning_item_seq) {
                            (Some(item_id), Some(item_seq)) => (item_id, item_seq),
                            (None, None) => {
                                let (item_id, item_seq) = runtime
                                    .start_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        ItemKind::Reasoning,
                                        serde_json::json!({ "title": "Reasoning", "text": "" }),
                                    )
                                    .await;
                                reasoning_item_id = Some(item_id);
                                reasoning_item_seq = Some(item_seq);
                                (item_id, item_seq)
                            }
                            _ => continue,
                        };
                        reasoning_text.push_str(&text);
                        runtime
                            .broadcast_event(ServerEvent::ItemDelta {
                                delta_kind: ItemDeltaKind::ReasoningTextDelta,
                                payload: ItemDeltaPayload {
                                    context: EventContext {
                                        session_id,
                                        turn_id: Some(turn_for_events.turn_id),
                                        item_id: Some(item_id),
                                        seq: 0,
                                    },
                                    delta: text,
                                    stream_index: None,
                                    channel: None,
                                },
                            })
                            .await;
                        let _ = item_seq;

                        // Store deferred completion info for interrupt recovery
                        if let Ok(mut session) = event_session_arc.try_lock() {
                            session.deferred_reasoning =
                                Some((item_id, item_seq, reasoning_text.clone()));
                        }
                    }
                    QueryEvent::ReasoningCompleted => {
                        if let (Some(item_id), Some(item_seq)) =
                            (reasoning_item_id.take(), reasoning_item_seq.take())
                        {
                            if let Ok(mut session) = event_session_arc.try_lock() {
                                session.deferred_reasoning.take();
                            }
                            complete_reasoning_item(
                                &runtime,
                                session_id,
                                turn_for_events.turn_id,
                                item_id,
                                item_seq,
                                reasoning_text.clone(),
                            )
                            .await;
                            reasoning_text.clear();
                        }
                    }
                    QueryEvent::ToolUseStart { id, name, input } => {
                        tool_names_by_id.insert(id.clone(), name.clone());
                        if let (Some(item_id), Some(item_seq)) =
                            (reasoning_item_id.take(), reasoning_item_seq.take())
                        {
                            complete_reasoning_item(
                                &runtime,
                                session_id,
                                turn_for_events.turn_id,
                                item_id,
                                item_seq,
                                reasoning_text.clone(),
                            )
                            .await;
                            reasoning_text.clear();
                        }
                        if let (Some(item_id), Some(item_seq)) =
                            (assistant_item_id.take(), assistant_item_seq.take())
                        {
                            complete_assistant_item(
                                &runtime,
                                session_id,
                                turn_for_events.turn_id,
                                item_id,
                                item_seq,
                                assistant_text.clone(),
                            )
                            .await;
                            assistant_text.clear();
                        }
                        let is_command_execution = is_unified_exec_tool(&name);
                        let command = command_display_from_input(&name, &input);
                        let item_kind = if is_file_change_tool(&name) {
                            ItemKind::FileChange
                        } else if is_command_execution {
                            ItemKind::CommandExecution
                        } else if is_plan_tool(&name) {
                            ItemKind::Plan
                        } else {
                            ItemKind::ToolCall
                        };
                        let started_payload = if is_file_change_tool(&name) {
                            serde_json::to_value(FileChangePayload {
                                tool_call_id: id.clone(),
                                changes: Vec::new(),
                                is_error: false,
                            })
                            .expect("serialize file change payload")
                        } else if is_command_execution {
                            serde_json::to_value(CommandExecutionPayload {
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                command: command.clone(),
                                source: devo_protocol::protocol::ExecCommandSource::Agent,
                                command_actions: command_actions_from_tool_input(&name, &command, &input),
                                output: None,
                                is_error: false,
                            })
                            .expect("serialize command execution payload")
                        } else if is_plan_tool(&name) {
                            serde_json::json!({
                                "title": "Plan",
                                "text": ""
                            })
                        } else {
                            serde_json::to_value(ToolCallPayload {
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                parameters: input.clone(),
                                command_actions: command_actions_from_tool_input(&name, &command, &input),
                            })
                            .expect("serialize tool call payload")
                        };
                        let (item_id, item_seq) = runtime
                            .start_item(
                                session_id,
                                turn_for_events.turn_id,
                                item_kind,
                                started_payload,
                            )
                            .await;
                        pending_tool_calls.insert(
                            id,
                            PendingToolCall {
                                item_id,
                                item_seq,
                                input,
                                is_command_execution,
                                command,
                            },
                        );
                    }
                    QueryEvent::ToolResult {
                        tool_use_id,
                        content,
                        display_content,
                        is_error,
                        summary,
                    } => {
                        let tool_name = tool_names_by_id.get(&tool_use_id).cloned();
                        // First complete the pending ToolCall item so its item/completed
                        // arrives before the ToolResult item/completed.
                        if let Some(pending) = pending_tool_calls.remove(&tool_use_id) {
                            if let Some(tool_name) = tool_name.clone()
                                && is_plan_tool(&tool_name)
                            {
                                let output_json = match content.clone() {
                                    devo_tools::ToolContent::Text(text) => serde_json::Value::String(text),
                                    devo_tools::ToolContent::Json(json) => json,
                                    devo_tools::ToolContent::Mixed { text, json } => {
                                        json.unwrap_or_else(|| serde_json::Value::String(text.unwrap_or_default()))
                                    }
                                };
                                let explanation = output_json
                                    .get("explanation")
                                    .and_then(serde_json::Value::as_str)
                                    .map(ToOwned::to_owned);
                                let plan = output_json
                                    .get("plan")
                                    .and_then(serde_json::Value::as_array)
                                    .cloned()
                                    .unwrap_or_default();

                                runtime
                                    .complete_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        pending.item_id,
                                        pending.item_seq,
                                        ItemKind::Plan,
                                        TurnItem::Plan(TextItem {
                                            text: output_json.to_string(),
                                        }),
                                        serde_json::json!({
                                            "title": "Plan",
                                            "text": output_json.to_string(),
                                        }),
                                    )
                                    .await;

                                runtime
                                    .broadcast_event(ServerEvent::TurnPlanUpdated(
                                        TurnPlanUpdatedPayload {
                                            session_id,
                                            turn: turn_for_plan_updates.clone(),
                                            explanation,
                                            plan: plan
                                                .into_iter()
                                                .filter_map(|item| {
                                                    Some(TurnPlanStepPayload {
                                                        step: item.get("step")?.as_str()?.to_string(),
                                                        status: item.get("status")?.as_str()?.to_string(),
                                                    })
                                                })
                                                .collect(),
                                        },
                                    ))
                                    .await;
                                continue;
                            }

                            if let Some(tool_name) = tool_name.clone()
                                && is_file_change_tool(&tool_name)
                            {
                                let output_json = match content.clone() {
                                    devo_tools::ToolContent::Text(text) => serde_json::Value::String(text),
                                    devo_tools::ToolContent::Json(json) => json,
                                    devo_tools::ToolContent::Mixed { text, json } => {
                                        json.unwrap_or_else(|| serde_json::Value::String(text.unwrap_or_default()))
                                    }
                                };
                                let changes = output_json
                                    .get("files")
                                    .and_then(serde_json::Value::as_array)
                                    .cloned()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter_map(|file| {
                                        let path = std::path::PathBuf::from(file.get("path")?.as_str()?);
                                        let kind = file.get("kind")?.as_str()?;
                                        let additions = file.get("additions").and_then(serde_json::Value::as_u64).unwrap_or(0);
                                        let deletions = file.get("deletions").and_then(serde_json::Value::as_u64).unwrap_or(0);
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
                                                    .or_else(|| output_json.get("diff"))
                                                    .and_then(serde_json::Value::as_str)
                                                    .unwrap_or("")
                                                    .to_string(),
                                                move_path: file
                                                    .get("movePath")
                                                    .or_else(|| file.get("move_path"))
                                                    .and_then(serde_json::Value::as_str)
                                                    .map(std::path::PathBuf::from),
                                            },
                                            _ => return None,
                                        };
                                        Some((path, change))
                                    })
                                    .collect::<Vec<_>>();
                                let changes = if changes.is_empty() {
                                    output_json
                                        .get("diff")
                                        .and_then(serde_json::Value::as_str)
                                        .map(extract_paths_from_patch)
                                        .unwrap_or_default()
                                        .into_iter()
                                        .map(|path| {
                                            (
                                                std::path::PathBuf::from(path),
                                                devo_protocol::protocol::FileChange::Update {
                                                    unified_diff: output_json
                                                        .get("diff")
                                                        .and_then(serde_json::Value::as_str)
                                                        .unwrap_or("")
                                                        .to_string(),
                                                    move_path: None,
                                                },
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                } else {
                                    changes
                                };

                                runtime
                                    .complete_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        pending.item_id,
                                        pending.item_seq,
                                        ItemKind::FileChange,
                                        TurnItem::ToolResult(ToolResultItem {
                                            tool_call_id: tool_use_id.clone(),
                                            tool_name: Some(tool_name.clone()),
                                            output: output_json.clone(),
                                            display_content: display_content.clone(),
                                            is_error,
                                        }),
                                        serde_json::to_value(FileChangePayload {
                                            tool_call_id: tool_use_id.clone(),
                                            changes,
                                            is_error,
                                        })
                                        .expect("serialize file change payload"),
                                    )
                                    .await;
                                continue;
                            }

                            if pending.is_command_execution {
                                let tool_name = tool_name.clone().unwrap_or_default();
                                let output = match content.clone() {
                                    devo_tools::ToolContent::Text(text) => serde_json::Value::String(text),
                                    devo_tools::ToolContent::Json(json) => json,
                                    devo_tools::ToolContent::Mixed { text, json } => {
                                        json.unwrap_or_else(|| serde_json::Value::String(text.unwrap_or_default()))
                                    }
                                };
                                let completed_payload =
                                    serde_json::to_value(CommandExecutionPayload {
                                        tool_call_id: tool_use_id.clone(),
                                        tool_name: tool_name.clone(),
                                        command: pending.command.clone(),
                                        source: devo_protocol::protocol::ExecCommandSource::Agent,
                                        command_actions: command_actions_from_tool_input(&tool_name, &pending.command, &pending.input),
                                        output: Some(output.clone()),
                                        is_error,
                                    })
                                    .expect("serialize command execution payload");
                                runtime
                                    .complete_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        pending.item_id,
                                        pending.item_seq,
                                        ItemKind::CommandExecution,
                                        TurnItem::CommandExecution(CommandExecutionItem {
                                            tool_call_id: tool_use_id.clone(),
                                            tool_name,
                                            command: pending.command,
                                            input: pending.input,
                                            output,
                                            is_error,
                                        }),
                                        completed_payload,
                                    )
                                    .await;
                                continue;
                            }
                            let completed_payload = serde_json::to_value(ToolCallPayload {
                                tool_call_id: tool_use_id.clone(),
                                tool_name: tool_name.clone().unwrap_or_default(),
                                parameters: pending.input.clone(),
                                command_actions: command_actions_from_tool_input(
                                    tool_name.clone().unwrap_or_default().as_str(),
                                    &pending.command,
                                    &pending.input,
                                ),
                            })
                            .expect("serialize tool call payload");
                            runtime
                                .complete_item(
                                    session_id,
                                    turn_for_events.turn_id,
                                    pending.item_id,
                                    pending.item_seq,
                                    ItemKind::ToolCall,
                                    TurnItem::ToolCall(ToolCallItem {
                                        tool_call_id: tool_use_id.clone(),
                                        tool_name: tool_name.clone().unwrap_or_default(),
                                        input: pending.input,
                                    }),
                                    completed_payload,
                                )
                                .await;
                        }
                        runtime
                            .emit_turn_item(
                                session_id,
                                turn_for_events.turn_id,
                                ItemKind::ToolResult,
                                TurnItem::ToolResult(ToolResultItem {
                                    tool_call_id: tool_use_id.clone(),
                                    tool_name: tool_name.clone(),
                                    output: match content.clone() {
                                        devo_tools::ToolContent::Text(text) => serde_json::Value::String(text),
                                        devo_tools::ToolContent::Json(json) => json,
                                        devo_tools::ToolContent::Mixed { text, json } => {
                                            json.unwrap_or_else(|| serde_json::Value::String(text.unwrap_or_default()))
                                        }
                                    },
                                    display_content: display_content.clone(),
                                    is_error,
                                }),
                                serde_json::to_value(ToolResultPayload {
                                    tool_call_id: tool_use_id.clone(),
                                    tool_name,
                                    content: match content {
                                        devo_tools::ToolContent::Text(text) => serde_json::Value::String(text),
                                        devo_tools::ToolContent::Json(json) => json,
                                        devo_tools::ToolContent::Mixed { text, json } => {
                                            json.unwrap_or_else(|| serde_json::Value::String(text.unwrap_or_default()))
                                        }
                                    },
                                    display_content,
                                    is_error,
                                    summary,
                                })
                                .expect("serialize tool result payload"),
                            )
                            .await;
                    }
                    QueryEvent::ToolProgress {
                        tool_use_id,
                        content,
                    } => {
                        let item_id = command_execution_item_id_for_progress(
                            &pending_tool_calls,
                            &tool_use_id,
                        );
                        let _ = runtime
                            .broadcast_event(ServerEvent::ItemDelta {
                                delta_kind: ItemDeltaKind::CommandExecutionOutputDelta,
                                payload: ItemDeltaPayload {
                                    context: EventContext {
                                        session_id,
                                        turn_id: Some(turn_for_events.turn_id),
                                        item_id,
                                        seq: 0,
                                    },
                                    delta: serde_json::json!({
                                        "tool_use_id": tool_use_id,
                                        "text": content,
                                    })
                                    .to_string(),
                                    stream_index: None,
                                    channel: None,
                                },
                            })
                            .await;
                    }
                    QueryEvent::UsageDelta {
                        input_tokens,
                        output_tokens,
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
                    } => {
                        let usage = TurnUsage {
                            input_tokens: input_tokens as u32,
                            output_tokens: output_tokens as u32,
                            cache_creation_input_tokens: cache_creation_input_tokens
                                .map(|value| value as u32),
                            cache_read_input_tokens: cache_read_input_tokens
                                .map(|value| value as u32),
                        };
                        latest_usage = Some(usage.clone());

                        let base = if let Some(base) = usage_base {
                            base
                        } else {
                            let base = {
                                let session = event_session_arc.lock().await;
                                (
                                    session.summary.total_input_tokens,
                                    session.summary.total_output_tokens,
                                    session.summary.total_cache_read_tokens,
                                )
                            };
                            usage_base = Some(base);
                            base
                        };
                        {
                            let mut session = event_session_arc.lock().await;
                            session.summary.total_input_tokens =
                                base.0 + usage.input_tokens as usize;
                            session.summary.total_output_tokens =
                                base.1 + usage.output_tokens as usize;
                        }
                        let _ = runtime
                            .broadcast_event(ServerEvent::TurnUsageUpdated(
                                TurnUsageUpdatedPayload {
                                    session_id,
                                    turn_id: turn_for_events.turn_id,
                                    usage,
                                    total_input_tokens: base.0 + input_tokens,
                                    total_output_tokens: base.1 + output_tokens,
                                    total_cache_read_tokens: base.2
                                        + cache_read_input_tokens.unwrap_or(0),
                                    last_query_input_tokens: input_tokens,
                                },
                            ))
                            .await;
                    }
                    QueryEvent::Usage {
                        input_tokens,
                        output_tokens,
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
                    } => {
                        let usage = TurnUsage {
                            input_tokens: input_tokens as u32,
                            output_tokens: output_tokens as u32,
                            cache_creation_input_tokens: cache_creation_input_tokens
                                .map(|value| value as u32),
                            cache_read_input_tokens: cache_read_input_tokens
                                .map(|value| value as u32),
                        };
                        latest_usage = Some(usage.clone());

                        let base = if let Some(base) = usage_base {
                            base
                        } else {
                            let base = {
                                let session = event_session_arc.lock().await;
                                (
                                    session.summary.total_input_tokens,
                                    session.summary.total_output_tokens,
                                    session.summary.total_cache_read_tokens,
                                )
                            };
                            usage_base = Some(base);
                            base
                        };
                        {
                            let mut session = event_session_arc.lock().await;
                            session.summary.total_input_tokens =
                                base.0 + usage.input_tokens as usize;
                            session.summary.total_output_tokens =
                                base.1 + usage.output_tokens as usize;
                            session.summary.total_cache_read_tokens =
                                base.2 + usage.cache_read_input_tokens.unwrap_or(0) as usize;
                            session.summary.last_query_total_tokens =
                                usage.input_tokens as usize + usage.output_tokens as usize;
                        }
                        let _ = runtime
                            .broadcast_event(ServerEvent::TurnUsageUpdated(
                                TurnUsageUpdatedPayload {
                                    session_id,
                                    turn_id: turn_for_events.turn_id,
                                    usage,
                                    total_input_tokens: base.0 + input_tokens,
                                    total_output_tokens: base.1 + output_tokens,
                                    total_cache_read_tokens: base.2
                                        + cache_read_input_tokens.unwrap_or(0),
                                    last_query_input_tokens: input_tokens,
                                },
                            ))
                            .await;
                    }
                    QueryEvent::TurnComplete { .. } => {}
                }
            }
            // Complete any deferred items that the interrupt handler didn't already take.
            // handle_interrupt takes deferred_assistant/deferred_reasoning from the session
            // and completes them; if they're already None we must skip to avoid persisting duplicates.
            if let Some((item_id, item_seq, text)) = {
                let mut session = event_session_arc.lock().await;
                session.deferred_reasoning.take()
            } {
                complete_reasoning_item(
                    &runtime,
                    session_id,
                    turn_for_events.turn_id,
                    item_id,
                    item_seq,
                    text,
                )
                .await;
            }
            if let Some((item_id, item_seq, text)) = {
                let mut session = event_session_arc.lock().await;
                session.deferred_assistant.take()
            } {
                complete_assistant_item(
                    &runtime,
                    session_id,
                    turn_for_events.turn_id,
                    item_id,
                    item_seq,
                    text,
                )
                .await;
            }
            latest_usage
        });

        let (
            result,
            session_total_input_tokens,
            session_total_output_tokens,
            session_total_cache_creation_tokens,
            session_total_cache_read_tokens,
            session_last_input_tokens,
            session_prompt_token_estimate,
        ) = {
            // Run the model query only after the event pipeline is ready so
            // streamed deltas can be consumed and persisted immediately.
            let core_session = {
                let session = session_arc.lock().await;
                Arc::clone(&session.core_session)
            };
            let mut core_session = core_session.lock().await;
            core_session.push_message(Message::user(input.clone()));
            let event_callback_tx = event_tx.clone();
            let callback = std::sync::Arc::new(move |event: QueryEvent| {
                let _ = event_callback_tx.send(event);
            });
            let registry = Arc::clone(&self.deps.registry);
            let permission_mode = core_session.config.permission_mode;
            let permission_profile = core_session.config.permission_profile.clone();
            let runtime = ToolRuntime::new_with_context(
                Arc::clone(&registry),
                self.build_permission_checker(
                    session_id,
                    turn_for_events.turn_id,
                    permission_mode,
                    permission_profile,
                ),
                ToolRuntimeContext {
                    session_id: session_id.to_string(),
                    turn_id: Some(turn_for_events.turn_id.to_string()),
                    cwd: core_session.cwd.clone(),
                },
            );
            let result = query(
                &mut core_session,
                &turn_config,
                self.deps.provider.clone(),
                registry,
                &runtime,
                Some(callback),
            )
            .await;
            (
                result,
                core_session.total_input_tokens,
                core_session.total_output_tokens,
                core_session.total_cache_creation_tokens,
                core_session.total_cache_read_tokens,
                core_session.last_input_tokens,
                core_session.prompt_token_estimate,
            )
        };
        drop(event_tx);
        // Wait for the event task to finish draining buffered stream events
        // before we persist the terminal turn state.
        let latest_usage = event_task.await.ok().flatten();
        self.active_tasks.lock().await.remove(&session_id);

        let final_turn = {
            let mut session = session_arc.lock().await;
            let mut final_turn = turn.clone();
            final_turn.completed_at = Some(Utc::now());
            final_turn.status = if result.is_ok() {
                TurnStatus::Completed
            } else {
                TurnStatus::Failed
            };
            final_turn.usage = latest_usage.clone();
            session.latest_turn = Some(final_turn.clone());
            session.active_turn = None;
            session.summary.status = SessionRuntimeStatus::Idle;
            session.summary.updated_at = Utc::now();
            session.summary.total_input_tokens = session_total_input_tokens;
            session.summary.total_output_tokens = session_total_output_tokens;
            session.summary.total_cache_creation_tokens = session_total_cache_creation_tokens;
            session.summary.total_cache_read_tokens = session_total_cache_read_tokens;
            session.summary.prompt_token_estimate = session_prompt_token_estimate;
            if let Some(usage) = &final_turn.usage {
                session.summary.last_query_total_tokens =
                    usage.input_tokens as usize + usage.output_tokens as usize;
            }

            // Persist token stats to SQLite (skip for ephemeral sessions)
            if !session.summary.ephemeral {
                let stats = SessionStats {
                    total_input_tokens: session_total_input_tokens,
                    total_output_tokens: session_total_output_tokens,
                    total_cache_creation_tokens: session_total_cache_creation_tokens,
                    total_cache_read_tokens: session_total_cache_read_tokens,
                    last_input_tokens: final_turn
                        .usage
                        .as_ref()
                        .map(|u| u.input_tokens as usize)
                        .unwrap_or(session_last_input_tokens),
                    turn_count: session.summary.updated_at.timestamp() as usize,
                    prompt_token_estimate: session_prompt_token_estimate,
                };
                if let Err(err) = self.deps.db.update_stats(&session_id, &stats) {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %err,
                        "failed to persist token stats to database"
                    );
                }
            }

            final_turn
        };

        // The turn is finished, so any queued "btw" input no longer applies.
        // Clear both the in-memory queue and the persisted mirror.
        {
            let is_ephemeral = {
                let session = session_arc.lock().await;
                session.summary.ephemeral
            };
            let btw_input_queue = {
                let session = session_arc.lock().await;
                Arc::clone(&session.btw_input_queue)
            };
            btw_input_queue
                .lock()
                .expect("btw input queue mutex should not be poisoned")
                .clear();
            if !is_ephemeral
                && let Err(err) = self.deps.db.clear_pending(&session_id, QueueType::Btw)
            {
                tracing::warn!(
                    session_id = %session_id,
                    error = %err,
                    "failed to clear btw input messages from database"
                );
            }
        }

        let (record, session_context, turn_context) = {
            let session = session_arc.lock().await;
            let core_session = session.core_session.lock().await;
            (
                session.record.clone(),
                core_session.session_context.clone(),
                core_session.latest_turn_context.clone(),
            )
        };
        if let Some(record) = record
            && let Err(error) = self.rollout_store.append_turn(
                &record,
                build_turn_record(&final_turn, session_context, turn_context),
            )
        {
            tracing::warn!(session_id = %session_id, error = %error, "failed to persist terminal turn line");
        }
        // Emit the terminal result before we look at queued follow-up input.
        if let Err(error) = result {
            tracing::warn!(
                session_id = %session_id,
                turn_id = %final_turn.turn_id,
                status = ?final_turn.status,
                error = %error,
                "turn execution failed"
            );
            self.emit_turn_item(
                session_id,
                final_turn.turn_id,
                ItemKind::AgentMessage,
                TurnItem::AgentMessage(TextItem {
                    text: error.to_string(),
                }),
                serde_json::json!({ "title": "Error", "text": error.to_string() }),
            )
            .await;
            self.broadcast_event(ServerEvent::TurnFailed(TurnEventPayload {
                session_id,
                turn: final_turn.clone(),
            }))
            .await;
        } else {
            tracing::info!(
                session_id = %session_id,
                turn_id = %final_turn.turn_id,
                status = ?final_turn.status,
                total_input_tokens = final_turn.usage.as_ref().map(|usage| usage.input_tokens),
                total_output_tokens = final_turn.usage.as_ref().map(|usage| usage.output_tokens),
                "turn execution completed"
            );
        }
        self.broadcast_event(ServerEvent::TurnCompleted(TurnEventPayload {
            session_id,
            turn: final_turn,
        }))
        .await;
        self.broadcast_event(ServerEvent::SessionStatusChanged(
            SessionStatusChangedPayload {
                session_id,
                status: SessionRuntimeStatus::Idle,
            },
        ))
        .await;

        // After the turn completes, check for queued inputs and start the next turn.
        let input_text = {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let pending_turn_queue = {
                let session = session_arc.lock().await;
                if session.active_turn.is_some() {
                    return;
                }
                Arc::clone(&session.pending_turn_queue)
            };
            let mut queue = pending_turn_queue
                .lock()
                .expect("pending turn queue mutex should not be poisoned");
            match queue.pop_front() {
                Some(devo_core::PendingInputItem {
                    kind: devo_core::PendingInputKind::UserText { text },
                    ..
                }) => text,
                _ => return,
            }
        };
        let display_input = input_text.clone();
        // Update clients before starting the next turn so dequeued input is
        // removed from any pending queue display.
        self.broadcast_updated_queue(session_id).await;

        let (turn_config, resolved_request) = {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let session = session_arc.lock().await;
            let model_override = session.summary.model.as_deref();
            let thinking_override = session.summary.thinking.clone();
            let turn_config = self
                .deps
                .resolve_turn_config(model_override, thinking_override);
            let resolved_request = turn_config
                .model
                .resolve_thinking_selection(turn_config.thinking_selection.as_deref());
            (turn_config, resolved_request)
        };

        let sequence = {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let session = session_arc.lock().await;
            session.latest_turn.as_ref().map_or(1, |t| t.sequence + 1)
        };

        let now = Utc::now();
        let turn = TurnMetadata {
            turn_id: TurnId::new(),
            session_id,
            sequence,
            status: TurnStatus::Running,
            kind: devo_core::TurnKind::Regular,
            model: turn_config.model.slug.clone(),
            thinking: turn_config.thinking_selection.clone(),
            reasoning_effort: resolved_request.effective_reasoning_effort,
            request_model: resolved_request.request_model.clone(),
            request_thinking: resolved_request.request_thinking.clone(),
            started_at: now,
            completed_at: None,
            usage: None,
        };
        {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let mut session = session_arc.lock().await;
            session.summary.status = SessionRuntimeStatus::ActiveTurn;
            session.summary.updated_at = now;
            session.active_turn = Some(turn.clone());
        }
        self.broadcast_event(ServerEvent::TurnStarted(TurnEventPayload {
            session_id,
            turn: turn.clone(),
        }))
        .await;
        // Chain directly instead of spawning so this drain loop can keep
        // consuming queued input until the queue is empty.
        Box::pin(Arc::clone(&self).execute_turn(
            session_id,
            turn,
            turn_config,
            display_input,
            input_text,
        ))
        .await;
    }

    /// Pop the first queued input and start a new turn in a background task.
    /// Used from the interrupt handler where the calling function must return
    /// its response immediately.
    pub(super) async fn spawn_next_turn_from_queue(self: &Arc<Self>, session_id: SessionId) {
        // Pop one queued input.
        let (display_input, input_text) = {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let pending_turn_queue = {
                let session = session_arc.lock().await;
                Arc::clone(&session.pending_turn_queue)
            };
            let mut guard = pending_turn_queue
                .lock()
                .expect("pending turn queue mutex should not be poisoned");
            match guard.pop_front() {
                Some(devo_core::PendingInputItem {
                    kind: devo_core::PendingInputKind::UserText { text },
                    ..
                }) => (text.clone(), text),
                _ => return,
            }
        };
        // Broadcast the updated queue state so the TUI removes this item
        // from its pending cells list.
        self.broadcast_updated_queue(session_id).await;

        // Resolve turn config from session metadata.
        let (turn_config, resolved_request) = {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let session = session_arc.lock().await;
            let model = session.summary.model.as_deref();
            let thinking = session.summary.thinking.clone();
            let tc = self.deps.resolve_turn_config(model, thinking);
            let rr = tc
                .model
                .resolve_thinking_selection(tc.thinking_selection.as_deref());
            (tc, rr)
        };

        let sequence = {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let session = session_arc.lock().await;
            session.latest_turn.as_ref().map_or(1, |t| t.sequence + 1)
        };

        let now = Utc::now();
        let turn = TurnMetadata {
            turn_id: TurnId::new(),
            session_id,
            sequence,
            status: TurnStatus::Running,
            kind: devo_core::TurnKind::Regular,
            model: turn_config.model.slug.clone(),
            thinking: turn_config.thinking_selection.clone(),
            reasoning_effort: resolved_request.effective_reasoning_effort,
            request_model: resolved_request.request_model.clone(),
            request_thinking: resolved_request.request_thinking.clone(),
            started_at: now,
            completed_at: None,
            usage: None,
        };
        {
            let session_arc = match self.sessions.lock().await.get(&session_id).cloned() {
                Some(s) => s,
                None => return,
            };
            let mut session = session_arc.lock().await;
            session.summary.status = SessionRuntimeStatus::ActiveTurn;
            session.summary.updated_at = now;
            session.active_turn = Some(turn.clone());
        }
        self.broadcast_event(ServerEvent::TurnStarted(TurnEventPayload {
            session_id,
            turn: turn.clone(),
        }))
        .await;
        // Spawn the turn in the background so the caller (interrupt handler)
        // can return its response immediately. The spawned task will call
        // drain_and_start_next_turn on completion, draining the entire queue.
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            runtime
                .execute_turn(session_id, turn, turn_config, display_input, input_text)
                .await;
        });
    }

    /// Read the current steering queue and broadcast its state to connected clients.
    /// Called after any queue mutation (enqueue, dequeue, clear) so the TUI preview
    /// stays in sync.
    pub(super) async fn broadcast_updated_queue(&self, session_id: SessionId) {
        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };
        let (pending_count, pending_texts) = {
            let pending_turn_queue = {
                let session = session_arc.lock().await;
                Arc::clone(&session.pending_turn_queue)
            };
            let queue = pending_turn_queue
                .lock()
                .expect("pending turn queue mutex should not be poisoned");
            let texts: Vec<String> = queue
                .iter()
                .filter_map(|item| match &item.kind {
                    devo_core::PendingInputKind::UserText { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            (texts.len(), texts)
        };
        self.broadcast_event(ServerEvent::InputQueueUpdated(
            devo_core::InputQueueUpdatedPayload {
                session_id,
                pending_count,
                pending_texts,
            },
        ))
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn command_progress_uses_command_execution_item_id() {
        let command_item_id = ItemId::new();
        let tool_item_id = ItemId::new();
        let mut pending_tool_calls = HashMap::new();
        pending_tool_calls.insert(
            "exec".to_string(),
            PendingToolCall {
                item_id: command_item_id,
                item_seq: 1,
                input: serde_json::json!({}),
                is_command_execution: true,
                command: "cargo test".to_string(),
            },
        );
        pending_tool_calls.insert(
            "read".to_string(),
            PendingToolCall {
                item_id: tool_item_id,
                item_seq: 2,
                input: serde_json::json!({}),
                is_command_execution: false,
                command: String::new(),
            },
        );

        assert_eq!(
            command_execution_item_id_for_progress(&pending_tool_calls, "exec"),
            Some(command_item_id)
        );
        assert_eq!(
            command_execution_item_id_for_progress(&pending_tool_calls, "read"),
            None
        );
        assert_eq!(
            command_execution_item_id_for_progress(&pending_tool_calls, "missing"),
            None
        );
    }

    #[test]
    fn file_change_tool_detection_matches_apply_patch_and_write() {
        assert!(is_file_change_tool("apply_patch"));
        assert!(is_file_change_tool("write"));
        assert!(!is_file_change_tool("read"));
    }

    #[test]
    fn plan_tool_detection_matches_update_plan() {
        assert!(is_plan_tool("update_plan"));
        assert!(!is_plan_tool("read"));
    }

    #[test]
    fn command_actions_from_read_tool_input_builds_read_action() {
        let actions = command_actions_from_tool_input(
            "read",
            "read crates/tui/src/chatwidget.rs",
            &serde_json::json!({
                "filePath": "crates/tui/src/chatwidget.rs"
            }),
        );
        assert_eq!(
            actions,
            vec![devo_protocol::parse_command::ParsedCommand::Read {
                cmd: "read crates/tui/src/chatwidget.rs".to_string(),
                name: "chatwidget.rs".to_string(),
                path: std::path::PathBuf::from("crates/tui/src/chatwidget.rs"),
            }]
        );
    }

    #[test]
    fn command_actions_from_grep_tool_input_builds_search_action() {
        let actions = command_actions_from_tool_input(
            "grep",
            "grep rebuild_restored_session in crates/tui/src",
            &serde_json::json!({
                "pattern": "rebuild_restored_session",
                "path": "crates/tui/src"
            }),
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            devo_protocol::parse_command::ParsedCommand::Search { query, path, .. }
            if query.as_deref() == Some("rebuild_restored_session")
                && path.as_deref() == Some("crates/tui/src")
        ));
    }
}
