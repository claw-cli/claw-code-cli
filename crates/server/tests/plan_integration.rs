use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use clawcr_core::{PresetModelCatalog, RolloutLine, SessionId, TextItem, ToolResultItem, TurnItem};
use clawcr_provider::{
    ModelProviderSDK, ModelRequest, ModelResponse, RequestContent, RequestMessage, ResponseContent,
    ResponseMetadata, StopReason, StreamEvent, Usage,
};
use clawcr_server::{
    ClientTransportKind, ServerRuntime, ServerRuntimeDependencies, SessionHistoryItemKind,
    SuccessResponse,
};
use clawcr_tools::{ToolRegistry, register_builtin_tools};

struct ToolThenReplyProvider {
    final_text: &'static str,
    follow_up_text: &'static str,
    stream_requests: Mutex<Vec<ModelRequest>>,
    stream_calls: AtomicUsize,
    tool_input: serde_json::Value,
    tool_name: &'static str,
}

impl ToolThenReplyProvider {
    fn new(
        tool_name: &'static str,
        tool_input: serde_json::Value,
        final_text: &'static str,
        follow_up_text: &'static str,
    ) -> Self {
        Self {
            final_text,
            follow_up_text,
            stream_requests: Mutex::new(Vec::new()),
            stream_calls: AtomicUsize::new(0),
            tool_input,
            tool_name,
        }
    }

    fn stream_requests(&self) -> Vec<ModelRequest> {
        self.stream_requests
            .lock()
            .expect("stream requests lock")
            .clone()
    }
}

#[async_trait]
impl ModelProviderSDK for ToolThenReplyProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            id: "title-1".into(),
            content: vec![ResponseContent::Text("Generated plan test title".into())],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            metadata: ResponseMetadata::default(),
        })
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        self.stream_requests
            .lock()
            .expect("stream requests lock")
            .push(request);
        let call_index = self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            let tool_input = self.tool_input.clone();
            let tool_name = self.tool_name.to_string();
            return Ok(Box::pin(stream::iter(vec![
                Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    content: ResponseContent::ToolUse {
                        id: "tool-1".into(),
                        name: tool_name.clone(),
                        input: json!({}),
                    },
                }),
                Ok(StreamEvent::InputJsonDelta {
                    index: 0,
                    partial_json: tool_input.to_string(),
                }),
                Ok(StreamEvent::ContentBlockStop { index: 0 }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-tool".into(),
                        content: vec![ResponseContent::ToolUse {
                            id: "tool-1".into(),
                            name: tool_name,
                            input: tool_input,
                        }],
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Usage::default(),
                        metadata: ResponseMetadata::default(),
                    },
                }),
            ])));
        }

        let text = if call_index == 1 {
            self.final_text
        } else {
            self.follow_up_text
        };

        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: text.into(),
            }),
            Ok(StreamEvent::MessageDone {
                response: ModelResponse {
                    id: "resp-final".into(),
                    content: vec![ResponseContent::Text(text.into())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: ResponseMetadata::default(),
                },
            }),
        ])))
    }

    fn name(&self) -> &str {
        "tool-then-reply-provider"
    }
}

#[tokio::test]
async fn update_plan_emits_plan_events_and_persists_plan_items() -> Result<()> {
    let data_root = TempDir::new()?;
    let plan = json!([
        {
            "status": "completed",
            "step": "Inspect runtime"
        },
        {
            "status": "in_progress",
            "step": "Emit plan item"
        }
    ]);
    let plan_input = json!({
        "explanation": "Tracking server work",
        "plan": plan
    });
    let expected_plan_text = format!(
        "Tracking server work\n\n{}",
        serde_json::to_string_pretty(plan_input["plan"].as_array().expect("plan array"))?
    );
    let runtime = build_runtime(
        data_root.path(),
        Arc::new(ToolThenReplyProvider::new(
            "update_plan",
            plan_input,
            "Plan updated.",
            "Follow-up completed.",
        )),
    );
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, data_root.path()).await?;

    start_turn(&runtime, connection_id, session_id, "track the rollout").await?;
    let notifications = collect_notifications_until_turn_completed(&mut notifications_rx).await?;

    let plan_started_index = notifications
        .iter()
        .position(|value| {
            value.get("method") == Some(&json!("item/started"))
                && value["params"]["item"]["item_kind"] == json!("plan")
        })
        .context("plan item/started notification")?;
    let plan_delta_index = notifications
        .iter()
        .position(|value| value.get("method") == Some(&json!("item/plan/delta")))
        .context("plan delta notification")?;
    let plan_completed_index = notifications
        .iter()
        .position(|value| {
            value.get("method") == Some(&json!("item/completed"))
                && value["params"]["item"]["item_kind"] == json!("plan")
        })
        .context("plan item/completed notification")?;
    let turn_plan_updated_index = notifications
        .iter()
        .position(|value| value.get("method") == Some(&json!("turn/plan/updated")))
        .context("turn/plan/updated notification")?;
    let turn_usage_updated = notifications
        .iter()
        .find(|value| value.get("method") == Some(&json!("turn/usage/updated")))
        .context("turn/usage/updated notification")?;

    assert!(plan_started_index < plan_delta_index);
    assert!(plan_delta_index < plan_completed_index);
    assert!(plan_completed_index < turn_plan_updated_index);
    assert_eq!(
        notifications[plan_started_index]["params"]["item"]["payload"],
        json!({ "title": "Plan", "text": "", "tool_use_id": "tool-1" })
    );
    assert_eq!(
        notifications[plan_delta_index]["params"]["payload"]["delta"],
        json!(expected_plan_text)
    );
    assert_eq!(
        notifications[plan_completed_index]["params"]["item"]["payload"],
        json!({ "title": "Plan", "text": expected_plan_text, "tool_use_id": "tool-1" })
    );
    assert!(
        !notifications.iter().any(|value| {
            value.get("method") == Some(&json!("item/completed"))
                && value["params"]["item"]["item_kind"] == json!("tool_result")
                && value["params"]["item"]["payload"]["tool_use_id"] == json!("tool-1")
        }),
        "successful update_plan must not emit a live tool_result item"
    );
    assert_eq!(
        notifications[turn_plan_updated_index]["params"]["turn"]["usage"],
        turn_usage_updated["params"]["usage"]
    );

    let rollout_lines = read_rollout_lines(data_root.path())?;
    let persisted_outputs = rollout_lines
        .iter()
        .find_map(|line| match line {
            RolloutLine::Item(item_line)
                if item_line.item.output_items.iter().any(|item| {
                    item == &TurnItem::Plan(TextItem {
                        text: expected_plan_text.clone(),
                    })
                }) =>
            {
                Some(item_line.item.output_items.clone())
            }
            _ => None,
        })
        .context("expected rollout to persist a plan item")?;
    assert_eq!(
        persisted_outputs,
        vec![
            TurnItem::ToolResult(ToolResultItem {
                tool_call_id: "tool-1".into(),
                output: serde_json::Value::String(expected_plan_text.clone()),
                is_error: false,
            }),
            TurnItem::Plan(TextItem {
                text: expected_plan_text.clone(),
            }),
        ],
        "successful update_plan persistence must keep replay and visible outputs in one item record"
    );
    let resumed = resume_session(&runtime, connection_id, session_id, 4).await?;
    assert!(contains_history_item(
        &resumed.history_items,
        SessionHistoryItemKind::Assistant,
        &expected_plan_text
    ));
    assert!(
        !contains_history_item(
            &resumed.history_items,
            SessionHistoryItemKind::ToolResult,
            &expected_plan_text
        ),
        "successful update_plan tool results must stay out of user-facing resume history"
    );

    Ok(())
}

#[tokio::test]
async fn update_plan_replay_preserves_tool_result_prompt_shape_after_reload() -> Result<()> {
    let data_root = TempDir::new()?;
    let plan = json!([
        {
            "status": "completed",
            "step": "Inspect runtime"
        },
        {
            "status": "in_progress",
            "step": "Emit plan item"
        }
    ]);
    let plan_input = json!({
        "explanation": "Tracking server work",
        "plan": plan
    });
    let expected_plan_text = format!(
        "Tracking server work\n\n{}",
        serde_json::to_string_pretty(plan_input["plan"].as_array().expect("plan array"))?
    );
    let provider = Arc::new(ToolThenReplyProvider::new(
        "update_plan",
        plan_input,
        "Plan updated.",
        "Follow-up completed.",
    ));
    let runtime = build_runtime(data_root.path(), provider.clone());
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, data_root.path()).await?;

    start_turn(&runtime, connection_id, session_id, "track the rollout").await?;
    let _ = collect_notifications_until_turn_completed(&mut notifications_rx).await?;

    let rebuilt_runtime = build_runtime(data_root.path(), provider.clone());
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, mut rebuilt_notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;
    let rebuilt_resume =
        resume_session(&rebuilt_runtime, rebuilt_connection_id, session_id, 4).await?;

    assert!(contains_history_item(
        &rebuilt_resume.history_items,
        SessionHistoryItemKind::Assistant,
        &expected_plan_text
    ));
    assert!(
        !contains_history_item(
            &rebuilt_resume.history_items,
            SessionHistoryItemKind::ToolResult,
            &expected_plan_text
        ),
        "rebuilt resume history must not duplicate successful update_plan output as a tool result"
    );

    start_turn(
        &rebuilt_runtime,
        rebuilt_connection_id,
        session_id,
        "continue after reload",
    )
    .await?;
    let _ = collect_notifications_until_turn_completed(&mut rebuilt_notifications_rx).await?;

    let stream_requests = provider.stream_requests();
    let follow_up_request = stream_requests
        .last()
        .context("expected follow-up stream request after reload")?;

    assert!(contains_tool_use(
        &follow_up_request.messages,
        "tool-1",
        "update_plan"
    ));
    assert!(contains_tool_result(
        &follow_up_request.messages,
        "tool-1",
        &expected_plan_text
    ));
    assert!(
        !contains_assistant_text(&follow_up_request.messages, &expected_plan_text),
        "replayed plan items must not be injected into provider-facing assistant text history"
    );

    Ok(())
}

#[tokio::test]
async fn update_plan_preserves_large_plan_text_without_micro_compaction() -> Result<()> {
    let data_root = TempDir::new()?;
    let large_step = "wire raw plan persistence ".repeat(512);
    let plan = json!([
        {
            "status": "in_progress",
            "step": large_step
        }
    ]);
    let plan_input = json!({
        "explanation": "Tracking large server work",
        "plan": plan
    });
    let expected_plan_text = format!(
        "Tracking large server work\n\n{}",
        serde_json::to_string_pretty(plan_input["plan"].as_array().expect("plan array"))?
    );
    assert!(expected_plan_text.len() > 10_000);
    let provider = Arc::new(ToolThenReplyProvider::new(
        "update_plan",
        plan_input,
        "Plan updated.",
        "Follow-up completed.",
    ));
    let runtime = build_runtime(data_root.path(), provider.clone());
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, data_root.path()).await?;

    start_turn(&runtime, connection_id, session_id, "track a large rollout").await?;
    let notifications = collect_notifications_until_turn_completed(&mut notifications_rx).await?;

    let plan_delta_notification = notifications
        .iter()
        .find(|value| value.get("method") == Some(&json!("item/plan/delta")))
        .context("plan delta notification")?;
    assert_eq!(
        plan_delta_notification["params"]["payload"]["delta"],
        json!(expected_plan_text)
    );
    assert!(
        !plan_delta_notification["params"]["payload"]["delta"]
            .as_str()
            .is_some_and(|delta| delta.contains("...[truncated]")),
        "successful update_plan plan text must stay lossless"
    );

    let rollout_lines = read_rollout_lines(data_root.path())?;
    assert!(
        rollout_lines.iter().any(|line| matches!(
            line,
            RolloutLine::Item(item_line)
                if item_line.item.output_items == vec![
                    TurnItem::ToolResult(ToolResultItem {
                        tool_call_id: "tool-1".into(),
                        output: serde_json::Value::String(expected_plan_text.clone()),
                        is_error: false,
                    }),
                    TurnItem::Plan(TextItem {
                        text: expected_plan_text.clone(),
                    }),
                ]
        )),
        "expected rollout to persist the full untruncated plan payload"
    );

    let follow_up_request = provider
        .stream_requests()
        .last()
        .cloned()
        .context("expected follow-up stream request")?;
    assert!(contains_tool_result(
        &follow_up_request.messages,
        "tool-1",
        &expected_plan_text
    ));

    Ok(())
}

#[tokio::test]
async fn errored_update_plan_stays_on_tool_result_path() -> Result<()> {
    let data_root = TempDir::new()?;
    let invalid_plan_input = json!({
        "plan": [
            {
                "status": "in_progress",
                "step": "One"
            },
            {
                "status": "in_progress",
                "step": "Two"
            }
        ]
    });
    let expected_error = "At most one step can be in_progress at a time.";
    let runtime = build_runtime(
        data_root.path(),
        Arc::new(ToolThenReplyProvider::new(
            "update_plan",
            invalid_plan_input,
            "Plan rejected.",
            "Follow-up completed.",
        )),
    );
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, data_root.path()).await?;

    start_turn(&runtime, connection_id, session_id, "track the rollout").await?;
    let notifications = collect_notifications_until_turn_completed(&mut notifications_rx).await?;

    assert!(
        !notifications
            .iter()
            .any(|value| value.get("method") == Some(&json!("item/plan/delta"))),
        "errored update_plan must not emit plan deltas"
    );
    assert!(
        !notifications
            .iter()
            .any(|value| value.get("method") == Some(&json!("turn/plan/updated"))),
        "errored update_plan must not emit turn/plan/updated"
    );
    let tool_result_notification = notifications
        .iter()
        .find(|value| {
            value.get("method") == Some(&json!("item/completed"))
                && value["params"]["item"]["item_kind"] == json!("tool_result")
                && value["params"]["item"]["payload"]["tool_use_id"] == json!("tool-1")
        })
        .context("errored update_plan tool_result notification")?;
    assert_eq!(
        tool_result_notification["params"]["item"]["payload"]["content"],
        json!(expected_error)
    );
    let resumed = resume_session(&runtime, connection_id, session_id, 4).await?;
    assert!(contains_history_item(
        &resumed.history_items,
        SessionHistoryItemKind::Error,
        expected_error
    ));
    assert!(
        !contains_history_item(
            &resumed.history_items,
            SessionHistoryItemKind::Assistant,
            expected_error
        ),
        "errored update_plan must not create a plan history item"
    );

    Ok(())
}

#[tokio::test]
async fn non_plan_tools_keep_generic_tool_result_items() -> Result<()> {
    let data_root = TempDir::new()?;
    let todos = json!([
        {
            "id": "todo-1",
            "status": "pending",
            "step": "Keep generic tool result"
        }
    ]);
    let tool_input = json!({ "todos": todos });
    let expected_output =
        serde_json::to_string_pretty(tool_input["todos"].as_array().expect("todos array"))?;
    let runtime = build_runtime(
        data_root.path(),
        Arc::new(ToolThenReplyProvider::new(
            "todowrite",
            tool_input,
            "Todo recorded.",
            "Follow-up completed.",
        )),
    );
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, data_root.path()).await?;

    start_turn(&runtime, connection_id, session_id, "record the todo").await?;
    let notifications = collect_notifications_until_turn_completed(&mut notifications_rx).await?;

    let tool_result_notification = notifications
        .iter()
        .find(|value| {
            value.get("method") == Some(&json!("item/completed"))
                && value["params"]["item"]["item_kind"] == json!("tool_result")
                && value["params"]["item"]["payload"]["tool_use_id"] == json!("tool-1")
        })
        .context("generic tool_result item notification")?;
    assert_eq!(
        tool_result_notification["params"]["item"]["payload"]["content"],
        json!(expected_output)
    );
    assert!(
        !notifications
            .iter()
            .any(|value| value.get("method") == Some(&json!("item/plan/delta"))),
        "non-plan tools must not emit plan deltas"
    );
    assert!(
        !notifications
            .iter()
            .any(|value| value.get("method") == Some(&json!("turn/plan/updated"))),
        "non-plan tools must not emit turn/plan/updated"
    );

    let rollout_lines = read_rollout_lines(data_root.path())?;
    assert!(
        rollout_lines.iter().any(|line| matches!(
            line,
            RolloutLine::Item(item_line)
                if item_line.item.output_items.iter().any(|item| {
                    item == &TurnItem::ToolResult(ToolResultItem {
                        tool_call_id: "tool-1".into(),
                        output: serde_json::Value::String(expected_output.clone()),
                        is_error: false,
                    })
                })
        )),
        "expected rollout to persist a generic tool_result item"
    );

    Ok(())
}

fn build_runtime(data_root: &Path, provider: Arc<dyn ModelProviderSDK>) -> Arc<ServerRuntime> {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    ServerRuntime::new(
        data_root.to_path_buf(),
        ServerRuntimeDependencies::new(
            provider,
            Arc::new(registry),
            "test-model".to_string(),
            Arc::new(PresetModelCatalog::default()),
        ),
    )
}

async fn initialize_connection(
    runtime: &Arc<ServerRuntime>,
) -> Result<(u64, mpsc::UnboundedReceiver<serde_json::Value>)> {
    let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
    let connection_id = runtime
        .register_connection(ClientTransportKind::Stdio, notifications_tx)
        .await;
    let initialize_response = runtime
        .handle_incoming(
            connection_id,
            json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "client_name": "test",
                    "client_version": "1.0.0",
                    "transport": "stdio",
                    "supports_streaming": true,
                    "supports_binary_images": false,
                    "opt_out_notification_methods": []
                }
            }),
        )
        .await
        .context("initialize response")?;
    let response: SuccessResponse<clawcr_server::InitializeResult> =
        serde_json::from_value(initialize_response)?;
    assert_eq!(response.result.server_name, "clawcr-server");

    let _ = runtime
        .handle_incoming(connection_id, json!({ "method": "initialized" }))
        .await;
    Ok((connection_id, notifications_rx))
}

async fn start_session(
    runtime: &Arc<ServerRuntime>,
    connection_id: u64,
    cwd: &Path,
) -> Result<SessionId> {
    let response = runtime
        .handle_incoming(
            connection_id,
            json!({
                "id": 2,
                "method": "session/start",
                "params": {
                    "cwd": cwd,
                    "ephemeral": false,
                    "title": "Plan integration",
                    "model": "test-model"
                }
            }),
        )
        .await
        .context("session/start response")?;
    let result: SuccessResponse<clawcr_server::SessionStartResult> =
        serde_json::from_value(response)?;
    Ok(result.result.session_id)
}

async fn start_turn(
    runtime: &Arc<ServerRuntime>,
    connection_id: u64,
    session_id: SessionId,
    text: &str,
) -> Result<()> {
    let response = runtime
        .handle_incoming(
            connection_id,
            json!({
                "id": 3,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": text }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response")?;
    let _: SuccessResponse<clawcr_server::TurnStartResult> = serde_json::from_value(response)?;
    Ok(())
}

async fn collect_notifications_until_turn_completed(
    notifications_rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
) -> Result<Vec<serde_json::Value>> {
    timeout(Duration::from_secs(5), async {
        let mut notifications = Vec::new();
        while let Some(value) = notifications_rx.recv().await {
            let is_turn_completed = value.get("method") == Some(&json!("turn/completed"));
            notifications.push(value);
            if is_turn_completed {
                return Ok(notifications);
            }
        }
        anyhow::bail!("notification channel closed before turn/completed")
    })
    .await
    .context("timed out waiting for turn/completed")?
}

async fn resume_session(
    runtime: &Arc<ServerRuntime>,
    connection_id: u64,
    session_id: SessionId,
    request_id: u64,
) -> Result<clawcr_server::SessionResumeResult> {
    let response = runtime
        .handle_incoming(
            connection_id,
            json!({
                "id": request_id,
                "method": "session/resume",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/resume response")?;
    Ok(
        serde_json::from_value::<SuccessResponse<clawcr_server::SessionResumeResult>>(response)?
            .result,
    )
}

fn read_rollout_lines(data_root: &Path) -> Result<Vec<RolloutLine>> {
    let mut rollout_paths = Vec::new();
    collect_rollout_paths(&data_root.join("sessions"), &mut rollout_paths)?;
    let rollout_path = rollout_paths
        .into_iter()
        .next()
        .context("expected one rollout file")?;
    let contents = std::fs::read_to_string(&rollout_path)
        .with_context(|| format!("read rollout file {}", rollout_path.display()))?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parse rollout line"))
        .collect()
}

fn collect_rollout_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(root)
        .with_context(|| format!("read rollout directory {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rollout_paths(&path, paths)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            paths.push(path);
        }
    }

    Ok(())
}

fn contains_tool_use(messages: &[RequestMessage], tool_use_id: &str, tool_name: &str) -> bool {
    messages.iter().any(|message| {
        message.content.iter().any(|content| {
            matches!(
                content,
                RequestContent::ToolUse { id, name, .. }
                    if id == tool_use_id && name == tool_name
            )
        })
    })
}

fn contains_tool_result(
    messages: &[RequestMessage],
    tool_use_id: &str,
    expected_text: &str,
) -> bool {
    messages.iter().any(|message| {
        message.content.iter().any(|content| {
            matches!(
                content,
                RequestContent::ToolResult {
                    tool_use_id: id,
                    content,
                    ..
                } if id == tool_use_id && content == expected_text
            )
        })
    })
}

fn contains_assistant_text(messages: &[RequestMessage], expected_text: &str) -> bool {
    messages.iter().any(|message| {
        message.role == "assistant"
            && message.content.iter().any(
                |content| matches!(content, RequestContent::Text { text } if text == expected_text),
            )
    })
}

fn contains_history_item(
    history_items: &[clawcr_server::SessionHistoryItem],
    kind: SessionHistoryItemKind,
    body: &str,
) -> bool {
    history_items
        .iter()
        .any(|item| item.kind == kind && item.body == body)
}
