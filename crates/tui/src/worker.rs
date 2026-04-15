use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    sync::mpsc,
    task::{JoinError, JoinHandle},
};

use clawcr_core::{
    Model, ModelCatalog, PresetModelCatalog, SessionId, TurnId, TurnStatus, test_model_connection,
};
use clawcr_protocol::ProviderFamily;
use clawcr_provider::{ModelProviderSDK, anthropic::AnthropicProvider, openai::OpenAIProvider};
use clawcr_server::{
    InputItem, ItemEnvelope, ItemEventPayload, ItemKind, ServerEvent, SessionHistoryItem,
    SessionHistoryItemKind, SessionListParams, SessionResumeParams, SessionStartParams,
    SessionTitleUpdateParams, StdioServerClient, StdioServerClientConfig, TurnEventPayload,
    TurnInterruptParams, TurnStartParams,
};

use crate::events::{SessionListEntry, TranscriptItem, TranscriptItemKind, WorkerEvent};

struct EnsureSessionOutcome {
    session_id: SessionId,
    resolved_model: Option<String>,
}

/// Immutable runtime configuration used to construct the background server client worker.
pub(crate) struct QueryWorkerConfig {
    /// Model identifier used for new turns.
    pub(crate) model: String,
    /// Working directory used for the server session.
    pub(crate) cwd: PathBuf,
    /// Environment overrides applied to the spawned server child process.
    pub(crate) server_env: Vec<(String, String)>,
    /// Optional log-level override forwarded to the server child process.
    pub(crate) server_log_level: Option<String>,
    /// Initial thinking mode used for new turns.
    pub(crate) thinking_selection: Option<String>,
}

/// Commands accepted by the background query worker.
enum OperationCommand {
    /// Submit a new user prompt to the session.
    SubmitPrompt(String),
    /// Update the model used for future turns.
    SetModel(String),
    /// Update the thinking mode used for future turns.
    SetThinking(Option<String>),
    /// Replace the provider connection settings and restart the server client.
    ReconfigureProvider {
        /// Model identifier to use for future turns.
        model: String,
        /// Optional provider base URL override.
        base_url: Option<String>,
        /// Optional provider API key override.
        api_key: Option<String>,
    },
    /// Validates provider settings with a temporary probe request.
    ValidateProvider {
        provider: ProviderFamily,
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    },
    /// Request a session list from the server.
    ListSessions,
    /// Clear the active session so the next prompt starts a fresh one lazily.
    StartNewSession,
    /// Switch the active session to a persisted session identifier.
    SwitchSession(SessionId),
    /// Rename the current active session.
    RenameSession(String),
    /// Interrupt the active turn when one is running.
    InterruptTurn,
    /// Stop the worker loop.
    Shutdown,
}

/// Handle used by the UI thread to interact with the background query worker.
pub(crate) struct QueryWorkerHandle {
    /// Sender used to submit commands to the worker.
    command_tx: mpsc::UnboundedSender<OperationCommand>,
    /// Receiver used by the UI to consume worker events.
    pub(crate) event_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    /// Background task running the worker loop.
    join_handle: JoinHandle<()>,
}

impl QueryWorkerHandle {
    /// Spawns the background worker and returns the UI-facing handle.
    pub(crate) fn spawn(config: QueryWorkerConfig) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let join_handle = tokio::spawn(run_worker(config, command_rx, event_tx));
        Self {
            command_tx,
            event_rx,
            join_handle,
        }
    }

    /// Submits one prompt to the worker.
    pub(crate) fn submit_prompt(&self, prompt: String) -> Result<()> {
        self.command_tx
            .send(OperationCommand::SubmitPrompt(prompt))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Updates the active session model for future turns.
    pub(crate) fn set_model(&self, model: String) -> Result<()> {
        self.command_tx
            .send(OperationCommand::SetModel(model))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Updates the thinking mode used for future turns.
    pub(crate) fn set_thinking(&self, thinking: Option<String>) -> Result<()> {
        self.command_tx
            .send(OperationCommand::SetThinking(thinking))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Reconfigures the provider connection used by the background server client.
    pub(crate) fn reconfigure_provider(
        &self,
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ReconfigureProvider {
                model,
                base_url,
                api_key,
            })
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Validates provider settings with a temporary probe request.
    pub(crate) fn validate_provider(
        &self,
        provider: ProviderFamily,
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ValidateProvider {
                provider,
                model,
                base_url,
                api_key,
            })
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Requests the current persisted session list from the background worker.
    pub(crate) fn list_sessions(&self) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ListSessions)
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Clears the active session so the next submitted prompt starts a fresh one lazily.
    pub(crate) fn start_new_session(&self) -> Result<()> {
        self.command_tx
            .send(OperationCommand::StartNewSession)
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Switches the active session to a persisted session identifier.
    pub(crate) fn switch_session(&self, session_id: SessionId) -> Result<()> {
        self.command_tx
            .send(OperationCommand::SwitchSession(session_id))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Renames the current active session.
    pub(crate) fn rename_session(&self, title: String) -> Result<()> {
        self.command_tx
            .send(OperationCommand::RenameSession(title))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Interrupts the active turn when one exists.
    pub(crate) fn interrupt_turn(&self) -> Result<()> {
        self.command_tx
            .send(OperationCommand::InterruptTurn)
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Stops the worker task and waits for it to finish.
    pub(crate) async fn shutdown(self) -> Result<()> {
        let _ = self.command_tx.send(OperationCommand::Shutdown);
        match self.join_handle.await {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(map_join_error(error)),
        }
    }
}

#[cfg(test)]
impl QueryWorkerHandle {
    /// Creates a lightweight stub worker handle for unit tests that exercise UI logic only.
    pub(crate) fn stub() -> Self {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let (_event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            command_tx,
            event_rx,
            join_handle: tokio::spawn(async move { while command_rx.recv().await.is_some() {} }),
        }
    }
}

async fn run_worker(
    config: QueryWorkerConfig,
    mut command_rx: mpsc::UnboundedReceiver<OperationCommand>,
    event_tx: mpsc::UnboundedSender<WorkerEvent>,
) {
    if let Err(error) = run_worker_inner(config, &mut command_rx, &event_tx).await {
        let _ = event_tx.send(WorkerEvent::TurnFailed {
            message: error.to_string(),
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
        });
    }
}

async fn run_worker_inner(
    config: QueryWorkerConfig,
    command_rx: &mut mpsc::UnboundedReceiver<OperationCommand>,
    event_tx: &mpsc::UnboundedSender<WorkerEvent>,
) -> Result<()> {
    // The worker owns the server client and translates UI commands into server
    // calls, then turns server notifications back into lightweight UI events.
    let mut server_env = config.server_env;
    let mut client = spawn_client(
        &config.cwd,
        server_env.clone(),
        config.server_log_level.clone(),
    )
    .await?;
    let _ = client.initialize().await?;
    let mut session_id: Option<SessionId> = None;
    let mut model = config.model;
    let mut thinking_selection = config.thinking_selection;
    let mut active_turn_id: Option<TurnId> = None;
    let mut turn_count = 0usize;
    let mut total_input_tokens = 0usize;
    let mut total_output_tokens = 0usize;
    let mut latest_completed_agent_message: Option<String> = None;

    loop {
        tokio::select! {
            maybe_command = command_rx.recv() => {
                match maybe_command {
                    Some(OperationCommand::SubmitPrompt(prompt)) => {
                        let session_start = ensure_session_started(
                            &mut client,
                            &config.cwd,
                            &model,
                            &mut session_id,
                        )
                        .await?;
                        if let Some(resolved_model) = session_start.resolved_model.clone() {
                            model = resolved_model;
                        }
                        let active_session_id = session_start.session_id;
                        let start_result = client.turn_start(TurnStartParams {
                            session_id: active_session_id,
                            input: vec![InputItem::Text { text: prompt }],
                            model: Some(model.clone()),
                            thinking: thinking_selection.clone(),
                            sandbox: None,
                            approval_policy: None,
                            cwd: None,
                        }).await;
                        match start_result {
                            Ok(result) => {
                                active_turn_id = Some(result.turn_id);
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::SetModel(next_model)) => {
                        model = next_model;
                    }
                    Some(OperationCommand::SetThinking(next_thinking)) => {
                        thinking_selection = next_thinking;
                    }
                    Some(OperationCommand::ValidateProvider {
                        provider,
                        model: next_model,
                        base_url,
                        api_key,
                    }) => {
                        match validate_provider_connection(
                            provider,
                            &next_model,
                            base_url,
                            api_key,
                        ).await {
                            Ok(reply_preview) => {
                                let _ = event_tx.send(WorkerEvent::ProviderValidationSucceeded {
                                    reply_preview,
                                });
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::ProviderValidationFailed {
                                    message: error.to_string(),
                                });
                            }
                        }
                    }
                Some(OperationCommand::ReconfigureProvider {
                    model: next_model,
                    base_url,
                    api_key,
                }) => {
                        // Recreate the client so new provider credentials take effect
                        // without requiring the whole app to restart.
                        model = next_model;
                        apply_env_override(&mut server_env, "CLAWCR_MODEL", &model);
                        apply_optional_env_override(&mut server_env, "CLAWCR_BASE_URL", base_url);
                        apply_optional_env_override(&mut server_env, "CLAWCR_API_KEY", api_key);
                        client.shutdown().await?;
                        client = spawn_client(
                            &config.cwd,
                            server_env.clone(),
                            config.server_log_level.clone(),
                        )
                        .await?;
                        client.initialize().await?;
                        session_id = None;
                        active_turn_id = None;
                    }
                    Some(OperationCommand::ListSessions) => {
                        match tokio::time::timeout(
                            Duration::from_secs(5),
                            client.session_list(SessionListParams::default()),
                        )
                        .await
                        {
                            Ok(Ok(result)) => {
                                let sessions = result
                                    .sessions
                                    .iter()
                                    .map(|session| SessionListEntry {
                                        session_id: session.session_id,
                                        title: session
                                            .title
                                            .clone()
                                            .unwrap_or_else(|| "(untitled)".to_string()),
                                        updated_at: session
                                            .updated_at
                                            .format("%Y-%m-%d %H:%M:%S UTC")
                                            .to_string(),
                                        is_active: Some(session.session_id) == session_id,
                                    })
                                    .collect();
                                let _ = event_tx.send(WorkerEvent::SessionsListed { sessions });
                            }
                            Ok(Err(error)) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                });
                            }
                            Err(_) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: "session list request timed out".to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::StartNewSession) => {
                        active_turn_id = None;
                        session_id = None;
                        let _ = event_tx.send(WorkerEvent::NewSessionPrepared);
                    }
                    Some(OperationCommand::SwitchSession(next_session_id)) => {
                        match client
                            .session_resume(SessionResumeParams {
                                session_id: next_session_id,
                            })
                            .await
                        {
                            Ok(result) => {
                                active_turn_id = None;
                                session_id = Some(next_session_id);
                                let _ = event_tx.send(WorkerEvent::SessionSwitched {
                                    session_id: next_session_id.to_string(),
                                    title: result.session.title,
                                    model: result.session.resolved_model,
                                    total_input_tokens: result.session.total_input_tokens,
                                    total_output_tokens: result.session.total_output_tokens,
                                    history_items: project_history_items(&result.history_items),
                                    loaded_item_count: result.loaded_item_count,
                                });
                                total_input_tokens = result.session.total_input_tokens;
                                total_output_tokens = result.session.total_output_tokens;
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::RenameSession(title)) => {
                        let Some(active_session_id) = session_id else {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: "no active session exists yet; send a prompt or switch to a saved session first".to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                            });
                            continue;
                        };
                        match client
                            .session_title_update(SessionTitleUpdateParams {
                                session_id: active_session_id,
                                title: title.clone(),
                            })
                            .await
                        {
                            Ok(result) => {
                                let _ = event_tx.send(WorkerEvent::SessionRenamed {
                                    session_id: active_session_id.to_string(),
                                    title: result
                                        .session
                                        .title
                                        .unwrap_or(title),
                                });
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::InterruptTurn) => {
                        if let (Some(turn_id), Some(active_session_id)) = (active_turn_id, session_id) {
                            if let Err(error) = client
                                .turn_interrupt(TurnInterruptParams {
                                    session_id: active_session_id,
                                    turn_id,
                                    reason: Some("user requested interrupt".to_string()),
                                })
                                .await
                            {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::Shutdown) | None => {
                        break;
                    }
                }
            }
            notification = client.recv_event() => {
                match notification? {
                    Some((method, event)) => {
                        match method.as_str() {
                            "turn/started" => {
                                if let ServerEvent::TurnStarted(payload) = event {
                                    active_turn_id = Some(payload.turn.turn_id);
                                    model = payload.turn.model_slug.clone();
                                    let _ = event_tx.send(WorkerEvent::TurnStarted {
                                        model: payload.turn.model_slug,
                                    });
                                }
                                latest_completed_agent_message = None;
                            }
                            "item/agentMessage/delta" => {
                                if let ServerEvent::ItemDelta { payload, .. } = event {
                                    let _ = event_tx.send(WorkerEvent::TextDelta(payload.delta));
                                }
                            }
                            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                                if let ServerEvent::ItemDelta { payload, .. } = event {
                                    let _ = event_tx.send(WorkerEvent::ReasoningDelta(payload.delta));
                                }
                            }
                            "item/completed" => {
                                if let ServerEvent::ItemCompleted(payload) = event {
                                    if let Some(text) = completed_agent_message_text(&payload) {
                                        latest_completed_agent_message = Some(text);
                                    }
                                    // Completed tool items are mapped into compact UI events
                                    // with pre-rendered summaries and previews.
                                    handle_completed_item(payload, event_tx);
                                }
                            }
                            "turn/completed" => {
                                if let ServerEvent::TurnCompleted(payload) = event {
                                    active_turn_id = None;
                                    let completed = payload.turn.status == TurnStatus::Completed
                                        || payload.turn.status == TurnStatus::Interrupted;
                                    if completed {
                                        turn_count += 1;
                                        if let Some(usage) = &payload.turn.usage {
                                            total_input_tokens = usage.input_tokens as usize;
                                            total_output_tokens = usage.output_tokens as usize;
                                        }
                                        let _ = event_tx.send(WorkerEvent::TurnFinished {
                                            stop_reason: format!("{:?}", payload.turn.status),
                                            turn_count,
                                            total_input_tokens,
                                            total_output_tokens,
                                        });
                                        latest_completed_agent_message = None;
                                    }
                                }
                            }
                            "turn/usage/updated" => {
                                if let ServerEvent::TurnUsageUpdated(payload) = event {
                                    total_input_tokens = payload.total_input_tokens;
                                    total_output_tokens = payload.total_output_tokens;
                                    let _ = event_tx.send(WorkerEvent::UsageUpdated {
                                        total_input_tokens: payload.total_input_tokens,
                                        total_output_tokens: payload.total_output_tokens,
                                    });
                                }
                            }
                            "turn/failed" => {
                                if let ServerEvent::TurnFailed(TurnEventPayload { turn, .. }) = event {
                                    active_turn_id = None;
                                    let message = latest_completed_agent_message
                                        .take()
                                        .unwrap_or_else(|| format!("turn failed with status {:?}", turn.status));
                                    if let Some(usage) = &turn.usage {
                                        total_input_tokens = usage.input_tokens as usize;
                                        total_output_tokens = usage.output_tokens as usize;
                                    }
                                    let _ = event_tx.send(WorkerEvent::TurnFailed {
                                        message,
                                        turn_count,
                                        total_input_tokens,
                                        total_output_tokens,
                                    });
                                }
                            }
                            "session/title/updated" => {
                                if let ServerEvent::SessionTitleUpdated(payload) = event {
                                    if let Some(title) = payload.session.title {
                                        let _ = event_tx.send(WorkerEvent::SessionTitleUpdated {
                                            session_id: payload.session.session_id.to_string(),
                                            title,
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    None => break,
                }
            }
        }
    }

    client.shutdown().await?;
    Ok(())
}

async fn ensure_session_started(
    client: &mut StdioServerClient,
    cwd: &PathBuf,
    model: &str,
    session_id: &mut Option<SessionId>,
) -> Result<EnsureSessionOutcome> {
    if let Some(session_id) = session_id {
        return Ok(EnsureSessionOutcome {
            session_id: *session_id,
            resolved_model: Some(model.to_string()),
        });
    }

    let session = client
        .session_start(SessionStartParams {
            cwd: cwd.clone(),
            ephemeral: false,
            title: None,
            model: Some(model.to_string()),
        })
        .await?;
    *session_id = Some(session.session_id);
    Ok(EnsureSessionOutcome {
        session_id: session.session_id,
        resolved_model: session.resolved_model,
    })
}

async fn spawn_client(
    cwd: &PathBuf,
    env: Vec<(String, String)>,
    server_log_level: Option<String>,
) -> Result<StdioServerClient> {
    StdioServerClient::spawn(StdioServerClientConfig {
        program: std::env::current_exe().context("resolve current executable for server launch")?,
        workspace_root: Some(cwd.clone()),
        env,
        args: server_log_level
            .into_iter()
            .flat_map(|level| ["--log-level".to_string(), level])
            .collect(),
    })
    .await
}

fn apply_env_override(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some((_, existing)) = env.iter_mut().find(|(existing_key, _)| existing_key == key) {
        *existing = value.to_string();
    } else {
        env.push((key.to_string(), value.to_string()));
    }
}

fn apply_optional_env_override(env: &mut Vec<(String, String)>, key: &str, value: Option<String>) {
    match value {
        Some(value) => apply_env_override(env, key, &value),
        None => env.retain(|(existing_key, _)| existing_key != key),
    }
}

fn completed_agent_message_text(payload: &ItemEventPayload) -> Option<String> {
    match &payload.item {
        ItemEnvelope {
            item_kind: ItemKind::AgentMessage,
            payload,
            ..
        } => payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn handle_completed_item(payload: ItemEventPayload, event_tx: &mpsc::UnboundedSender<WorkerEvent>) {
    match payload.item {
        ItemEnvelope {
            item_kind: ItemKind::AgentMessage,
            payload,
            ..
        } => {
            let text = payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned);
            if let Some(text) = text {
                let _ = event_tx.send(WorkerEvent::AssistantMessageCompleted(text));
            }
        }
        ItemEnvelope {
            item_kind: ItemKind::Reasoning,
            payload,
            ..
        } => {
            let text = payload
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned);
            if let Some(text) = text {
                let _ = event_tx.send(WorkerEvent::ReasoningCompleted(text));
            }
        }
        ItemEnvelope {
            item_kind: ItemKind::ToolCall,
            payload,
            ..
        } => {
            let tool_use_id = payload
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let summary = summarize_tool_call(&payload);
            let detail = payload
                .get("input")
                .map(render_json_preview)
                .filter(|detail| !detail.is_empty());
            let _ = event_tx.send(WorkerEvent::ToolCall {
                tool_use_id,
                summary,
                detail,
            });
        }
        ItemEnvelope {
            item_kind: ItemKind::ToolResult,
            payload,
            ..
        } => {
            let content = payload
                .get("content")
                .map(render_json_value_text)
                .unwrap_or_default();
            let is_error = payload
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let tool_use_id = payload
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let _ = event_tx.send(WorkerEvent::ToolResult {
                tool_use_id,
                preview: content,
                is_error,
                truncated: false,
            });
        }
        _ => {}
    }
}

fn project_history_items(items: &[SessionHistoryItem]) -> Vec<TranscriptItem> {
    let mut transcript = Vec::new();
    let mut index = 0usize;

    while index < items.len() {
        let item = &items[index];
        if item.kind == SessionHistoryItemKind::ToolCall {
            if let Some(next) = items.get(index + 1) {
                if matches!(
                    next.kind,
                    SessionHistoryItemKind::ToolResult | SessionHistoryItemKind::Error
                ) {
                    let merged = if next.kind == SessionHistoryItemKind::Error {
                        TranscriptItem::tool_error(item.title.clone(), next.body.clone())
                    } else {
                        TranscriptItem::restored_tool_result(item.title.clone(), next.body.clone())
                    };
                    transcript.push(merged);
                    index += 2;
                    continue;
                }
            }
        }

        let kind = match item.kind {
            SessionHistoryItemKind::User => TranscriptItemKind::User,
            SessionHistoryItemKind::Assistant => TranscriptItemKind::Assistant,
            SessionHistoryItemKind::ToolCall => TranscriptItemKind::ToolCall,
            SessionHistoryItemKind::ToolResult => TranscriptItemKind::ToolResult,
            SessionHistoryItemKind::Error => TranscriptItemKind::Error,
        };
        let transcript_item = match item.kind {
            SessionHistoryItemKind::ToolCall => TranscriptItem::tool_call(item.title.clone()),
            SessionHistoryItemKind::ToolResult => {
                TranscriptItem::restored_tool_result(item.title.clone(), item.body.clone())
            }
            SessionHistoryItemKind::Error => {
                TranscriptItem::tool_error(item.title.clone(), item.body.clone())
            }
            SessionHistoryItemKind::User | SessionHistoryItemKind::Assistant => {
                TranscriptItem::new(kind, item.title.clone(), item.body.clone())
            }
        };
        transcript.push(transcript_item);
        index += 1;
    }

    transcript
}

fn summarize_tool_call(payload: &serde_json::Value) -> String {
    let tool_name = payload
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool");
    let input = payload.get("input").unwrap_or(&serde_json::Value::Null);
    let detail = summarize_tool_input(tool_name, input);
    if detail.is_empty() {
        tool_name.to_string()
    } else {
        format!("{tool_name}: {detail}")
    }
}

fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    let candidate = match tool_name {
        "bash" => input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("cmd").and_then(serde_json::Value::as_str)),
        "read" => input
            .get("filePath")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("path").and_then(serde_json::Value::as_str)),
        "write" | "edit" | "apply_patch" => input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("filePath").and_then(serde_json::Value::as_str)),
        "webfetch" | "websearch" => input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("query").and_then(serde_json::Value::as_str)),
        _ => None,
    };

    candidate
        .map(|text| compact_tool_summary(text, 96))
        .unwrap_or_else(|| compact_tool_summary(&render_json_preview(input), 96))
}

fn compact_tool_summary(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated = compact.chars().count() > max_chars;
    let mut out = compact.chars().take(max_chars).collect::<String>();
    if truncated {
        out.push('…');
    }
    out
}

fn render_json_preview(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => truncate_tool_output(text),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
            truncate_tool_output(&pretty)
        }
        _ => truncate_tool_output(&value.to_string()),
    }
}

fn render_json_value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

fn truncate_tool_output(content: &str) -> String {
    const MAX_LINES: usize = 8;
    const MAX_CHARS: usize = 1200;
    let content = normalize_display_output(content);
    let content = content.as_str();

    let mut lines = Vec::new();
    let mut chars = 0usize;
    for line in content.lines() {
        if lines.len() >= MAX_LINES || chars >= MAX_CHARS {
            break;
        }
        let remaining = MAX_CHARS.saturating_sub(chars);
        if line.chars().count() > remaining {
            let preview = line.chars().take(remaining).collect::<String>();
            lines.push(preview);
            break;
        }
        chars += line.chars().count();
        lines.push(line.to_string());
    }

    if lines.is_empty() && !content.is_empty() {
        let preview = content.chars().take(MAX_CHARS).collect::<String>();
        return if preview == content {
            preview
        } else {
            format!("{preview}\n… ")
        };
    }

    let preview = lines.join("\n");
    if preview == content {
        preview
    } else if preview.is_empty() {
        "… ".to_string()
    } else {
        format!("{preview}\n… ")
    }
}

async fn validate_provider_connection(
    provider: ProviderFamily,
    model: &str,
    base_url: Option<String>,
    api_key: Option<String>,
) -> Result<String> {
    let validation_model = resolve_validation_model(provider, model)?;
    let validation_provider = build_validation_provider(provider, base_url, api_key)?;
    tokio::time::timeout(
        Duration::from_secs(20),
        test_model_connection(
            validation_provider.as_ref(),
            &validation_model,
            "Reply with OK only.",
        ),
    )
    .await
    .context("provider validation timed out after 20s")?
    .map_err(Into::into)
}

fn resolve_validation_model(provider: ProviderFamily, model: &str) -> Result<Model> {
    let catalog = PresetModelCatalog::load()?;
    if let Some(entry) = catalog.get(model) {
        return Ok(entry.clone());
    }
    Ok(Model {
        slug: model.to_string(),
        provider: provider,
        ..Model::default()
    })
}

fn build_validation_provider(
    provider: ProviderFamily,
    base_url: Option<String>,
    api_key: Option<String>,
) -> Result<std::sync::Arc<dyn ModelProviderSDK>> {
    match provider {
        ProviderFamily::Anthropic { .. } => {
            let api_key = api_key.context("anthropic provider requires an API key")?;
            let base_url = base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string());
            Ok(std::sync::Arc::new(
                AnthropicProvider::new(base_url).with_api_key(api_key),
            ))
        }
        ProviderFamily::Openai { .. } => {
            let base_url = normalize_openai_base_url(
                &base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            );
            let provider = if let Some(api_key) = api_key {
                OpenAIProvider::new(base_url).with_api_key(api_key)
            } else {
                OpenAIProvider::new(base_url)
            };
            Ok(std::sync::Arc::new(provider))
        }
    }
}

fn normalize_openai_base_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let Some(scheme_sep) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let has_explicit_path = trimmed[scheme_sep + 3..].contains('/');
    if has_explicit_path {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

fn normalize_display_output(content: &str) -> String {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_matches('\n')
        .to_string()
}

fn map_join_error(error: JoinError) -> anyhow::Error {
    if error.is_cancelled() {
        anyhow::anyhow!("interactive worker task was cancelled")
    } else if error.is_panic() {
        anyhow::anyhow!("interactive worker task panicked")
    } else {
        anyhow::Error::new(error)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pretty_assertions::assert_eq;

    use clawcr_core::{SessionId, SessionTitleState};
    use clawcr_server::{SessionRuntimeStatus, SessionSummary};

    use super::{
        normalize_display_output, project_history_items, summarize_tool_call, truncate_tool_output,
    };
    use crate::events::SessionListEntry;
    use crate::events::TranscriptItem;
    use clawcr_server::{SessionHistoryItem, SessionHistoryItemKind};

    #[test]
    fn bash_tool_summary_uses_command_text() {
        let payload = serde_json::json!({
            "tool_name": "bash",
            "input": {
                "command": "Get-Date -Format \"yyyy-MM-dd\""
            }
        });

        assert_eq!(
            summarize_tool_call(&payload),
            "bash: Get-Date -Format \"yyyy-MM-dd\""
        );
    }

    #[test]
    fn tool_output_preview_truncates_large_content() {
        let content = (1..=12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            truncate_tool_output(&content),
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n… "
        );
    }

    #[test]
    fn session_list_entries_keep_title_before_identifier() {
        let active_session_id = SessionId::new();
        let summary = SessionSummary {
            session_id: active_session_id,
            cwd: ".".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Saved conversation".to_string()),
            title_state: SessionTitleState::Provisional,
            ephemeral: false,
            resolved_model: Some("test-model".to_string()),
            total_input_tokens: 0,
            total_output_tokens: 0,
            status: SessionRuntimeStatus::Idle,
        };
        let entry = SessionListEntry {
            session_id: summary.session_id,
            title: summary.title.clone().unwrap_or_default(),
            updated_at: summary
                .updated_at
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string(),
            is_active: true,
        };

        assert_eq!(entry.title, "Saved conversation");
        assert!(entry.updated_at.contains("UTC"));
    }

    #[test]
    fn session_list_entries_mark_inactive_sessions() {
        let summary = SessionSummary {
            session_id: SessionId::new(),
            cwd: ".".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Saved conversation".to_string()),
            title_state: SessionTitleState::Provisional,
            ephemeral: false,
            resolved_model: Some("test-model".to_string()),
            total_input_tokens: 0,
            total_output_tokens: 0,
            status: SessionRuntimeStatus::Idle,
        };
        let entry = SessionListEntry {
            session_id: summary.session_id,
            title: summary.title.clone().unwrap_or_default(),
            updated_at: summary
                .updated_at
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string(),
            is_active: false,
        };

        assert!(!entry.is_active);
    }

    #[test]
    fn display_output_normalization_trims_crlf_padding() {
        assert_eq!(
            normalize_display_output("\r\n\r\nhello\r\nworld\r\n\r\n"),
            "hello\nworld"
        );
    }

    #[test]
    fn project_history_merges_tool_call_and_result() {
        let items = vec![
            SessionHistoryItem {
                kind: SessionHistoryItemKind::ToolCall,
                title: "Ran powershell -Command \"Get-Date\"".to_string(),
                body: String::new(),
            },
            SessionHistoryItem {
                kind: SessionHistoryItemKind::ToolResult,
                title: "Tool output".to_string(),
                body: "2026-04-09".to_string(),
            },
        ];

        assert_eq!(
            project_history_items(&items),
            vec![TranscriptItem::restored_tool_result(
                "Ran powershell -Command \"Get-Date\"",
                "2026-04-09"
            )]
        );
    }
}
