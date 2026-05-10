use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tokio::task::JoinHandle;

use devo_core::Model;
use devo_core::ModelCatalog;
use devo_core::PermissionPreset;
use devo_core::PresetModelCatalog;
use devo_core::ProviderWireApi;
use devo_core::ReasoningEffort;
use devo_core::SessionId;
use devo_core::TurnId;
use devo_core::TurnStatus;
use devo_core::test_model_connection;
use devo_provider::ModelProviderSDK;
use devo_provider::anthropic::AnthropicProvider;
use devo_provider::openai::OpenAIProvider;
use devo_provider::openai::OpenAIResponsesProvider;
use devo_server::ApprovalDecisionPayload;
use devo_server::ApprovalRequestPayload;
use devo_server::ApprovalRespondParams;
use devo_server::InputItem;
use devo_server::ItemEnvelope;
use devo_server::ItemEventPayload;
use devo_server::ItemKind;
use devo_server::ServerEvent;
use devo_server::SessionCompactParams;
use devo_server::SessionHistoryItem;
use devo_server::SessionHistoryItemKind;
use devo_server::SessionListParams;
use devo_server::SessionResumeParams;
use devo_server::SessionRollbackParams;
use devo_server::SessionStartParams;
use devo_server::SessionTitleUpdateParams;
use devo_server::SkillListParams;
use devo_server::SkillSource;
use devo_server::StdioServerClient;
use devo_server::StdioServerClientConfig;
use devo_server::ToolCallPayload;
use devo_server::ToolResultPayload;
use devo_server::TurnEventPayload;
use devo_server::TurnInterruptParams;
use devo_server::TurnStartParams;
use devo_server::TurnSteerParams;

use crate::app_command::InputHistoryDirection;
use crate::events::SessionListEntry;
use crate::events::TranscriptItem;
use crate::events::TranscriptItemKind;
use crate::events::WorkerEvent;

struct EnsureSessionOutcome {
    session_id: SessionId,
    model: Option<String>,
    thinking: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    created: bool,
}

/// Immutable runtime configuration used to construct the background server client worker.
pub(crate) struct QueryWorkerConfig {
    /// Model identifier used for new turns.
    pub(crate) model: String,
    /// Working directory used for the server session.
    pub(crate) cwd: PathBuf,
    /// Optional log-level override forwarded to the server child process.
    pub(crate) server_log_level: Option<String>,
    /// Initial thinking mode used for new turns.
    pub(crate) thinking_selection: Option<String>,
    /// Permission preset to apply to the server session when it exists.
    pub(crate) permission_preset: PermissionPreset,
}

/// TODO: Should we extract the OperationCommand to the `protocol` crate? Since it can be shareable.
/// Commands accepted by the background query worker.
enum OperationCommand {
    /// Submit a new user prompt to the session.
    SubmitPrompt {
        prompt: String,
        approval_policy: Option<String>,
    },
    /// Update the model used for future turns.
    /// TODO: Model should be bind at Session Metadata, not turn, indicate to the model utilized to generate
    /// at next turn. However, we can still bind a model at turn, to indicate what model is utlized generated.
    /// User can change session metadata model to decide what the next turn model is utlized.
    SetModel(String),
    /// TODO: Same with model, should bind at session metadata.
    /// Update the thinking mode used for future turns.
    SetThinking(Option<String>),
    /// Replace the provider connection settings and restart the server client.
    ReconfigureProvider {
        /// Provider wire protocol to use for future turns.
        wire_api: ProviderWireApi,
        /// Model identifier to use for future turns.
        model: String,
        /// Optional provider base URL override.
        base_url: Option<String>,
        /// Optional provider API key override.
        api_key: Option<String>,
    },
    /// Validates provider settings with a temporary probe request.
    ValidateProvider {
        provider: ProviderWireApi,
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    },
    /// Request a session list from the server.
    ListSessions,
    /// Request a skills list from the server.
    ListSkills,
    /// Request proactive compaction for the active session.
    CompactSession,
    /// Clear the active session so the next prompt starts a fresh one lazily.
    StartNewSession,
    /// Switch the active session to a persisted session identifier.
    SwitchSession(SessionId),
    /// Rename the current active session.
    RenameSession(String),
    /// Roll back the active session to a selected user turn.
    RollbackToUserTurn(u32),
    /// Fork a new session at a selected user turn.
    ForkAtUserTurn(u32),
    /// Interrupt the active turn when one is running.
    InterruptTurn,
    /// Steer text into the currently active turn.
    SteerTurn {
        input: Vec<InputItem>,
        expected_turn_id: TurnId,
    },
    ApprovalRespond {
        session_id: SessionId,
        turn_id: TurnId,
        approval_id: String,
        decision: devo_server::ApprovalDecisionValue,
        scope: devo_server::ApprovalScopeValue,
    },
    UpdatePermissions {
        preset: devo_protocol::PermissionPreset,
    },
    /// Browse persisted input history via the server/runtime session state.
    BrowseInputHistory(InputHistoryDirection),
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
    pub(crate) fn submit_prompt(
        &self,
        prompt: String,
        approval_policy: Option<String>,
    ) -> Result<()> {
        self.command_tx
            .send(OperationCommand::SubmitPrompt {
                prompt,
                approval_policy,
            })
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
        wire_api: ProviderWireApi,
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ReconfigureProvider {
                wire_api,
                model,
                base_url,
                api_key,
            })
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Validates provider settings with a temporary probe request.
    pub(crate) fn validate_provider(
        &self,
        provider: ProviderWireApi,
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

    /// Requests the current skill list from the background worker.
    pub(crate) fn list_skills(&self) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ListSkills)
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Requests proactive compaction for the current active session.
    pub(crate) fn compact_session(&self) -> Result<()> {
        self.command_tx
            .send(OperationCommand::CompactSession)
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

    pub(crate) fn rollback_to_user_turn(&self, user_turn_index: u32) -> Result<()> {
        self.command_tx
            .send(OperationCommand::RollbackToUserTurn(user_turn_index))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    pub(crate) fn fork_at_user_turn(&self, user_turn_index: u32) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ForkAtUserTurn(user_turn_index))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Interrupts the active turn when one exists.
    pub(crate) fn interrupt_turn(&self) -> Result<()> {
        self.command_tx
            .send(OperationCommand::InterruptTurn)
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Steer text into the currently active turn.
    pub(crate) fn submit_steer(&self, text: String, expected_turn_id: TurnId) -> Result<()> {
        self.command_tx
            .send(OperationCommand::SteerTurn {
                input: vec![devo_server::InputItem::Text { text }],
                expected_turn_id,
            })
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    pub(crate) fn approval_respond(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        approval_id: String,
        decision: devo_server::ApprovalDecisionValue,
        scope: devo_server::ApprovalScopeValue,
    ) -> Result<()> {
        self.command_tx
            .send(OperationCommand::ApprovalRespond {
                session_id,
                turn_id,
                approval_id,
                decision,
                scope,
            })
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    pub(crate) fn update_permissions(&self, preset: devo_protocol::PermissionPreset) -> Result<()> {
        self.command_tx
            .send(OperationCommand::UpdatePermissions { preset })
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    pub(crate) fn browse_input_history(&self, direction: InputHistoryDirection) -> Result<()> {
        self.command_tx
            .send(OperationCommand::BrowseInputHistory(direction))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Stops the worker task and waits briefly for it to finish.
    pub(crate) async fn shutdown(self) -> Result<()> {
        let _ = self.command_tx.send(OperationCommand::Shutdown);
        let mut join_handle = self.join_handle;
        tokio::select! {
            result = &mut join_handle => {
                match result {
                    Ok(()) => Ok(()),
                    Err(error) if error.is_cancelled() => Ok(()),
                    Err(error) => Err(map_join_error(error)),
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                join_handle.abort();
                match join_handle.await {
                    Ok(()) => Ok(()),
                    Err(error) if error.is_cancelled() => Ok(()),
                    Err(error) => Err(map_join_error(error)),
                }
            }
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
            total_cache_read_tokens: 0,
            prompt_token_estimate: 0,
            last_query_input_tokens: 0,
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
    let mut client = spawn_client(&config.cwd, config.server_log_level.clone()).await?;
    let _ = client.initialize().await?;
    let mut session_id: Option<SessionId> = None;
    let mut session_cwd = config.cwd.clone();
    let mut model = config.model;
    let mut thinking_selection = config.thinking_selection;
    let mut permission_preset = config.permission_preset;
    let mut active_turn_id: Option<TurnId> = None;
    let mut turn_count = 0usize;
    let mut total_input_tokens = 0usize;
    let mut total_output_tokens = 0usize;
    let mut total_cache_read_tokens = 0usize;
    let mut last_query_total_tokens = 0usize;
    let mut last_query_input_tokens = 0usize;
    let mut saw_usage_update_for_turn = false;
    let mut latest_completed_agent_message: Option<String> = None;
    let mut input_history_cursor: Option<usize> = None;

    loop {
        tokio::select! {
            maybe_command = command_rx.recv() => {
                match maybe_command {
                    Some(OperationCommand::SubmitPrompt {
                        prompt,
                        approval_policy,
                    }) => {
                        let session_start = ensure_session_started(
                            &mut client,
                            &config.cwd,
                            &model,
                            &mut session_id,
                        )
                        .await?;
                        if let Some(start_model) = session_start.model.clone() {
                            model = start_model;
                        }
                        thinking_selection = session_start
                            .thinking
                            .clone()
                            .or(thinking_selection);
                        let active_session_id = session_start.session_id;
                        if session_start.created {
                            apply_session_permissions(
                                &mut client,
                                active_session_id,
                                permission_preset,
                            )
                            .await?;
                        }
                        let start_result = client.turn_start(TurnStartParams {
                            session_id: active_session_id,
                            input: vec![InputItem::Text { text: prompt }],
                            model: Some(model.clone()),
                            thinking: thinking_selection.clone(),
                            sandbox: None,
                            approval_policy,
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
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::SetModel(next_model)) => {
                        model = next_model;
                        input_history_cursor = None;
                        if let Some(active_session_id) = session_id {
                            let _ = client
                                .session_metadata_update(devo_server::SessionMetadataUpdateParams {
                                    session_id: active_session_id,
                                    model: Some(model.clone()),
                                    thinking: thinking_selection.clone(),
                                })
                                .await;
                        }
                    }
                    Some(OperationCommand::SetThinking(next_thinking)) => {
                        thinking_selection = next_thinking;
                        if let Some(active_session_id) = session_id {
                            let _ = client
                                .session_metadata_update(devo_server::SessionMetadataUpdateParams {
                                    session_id: active_session_id,
                                    model: Some(model.clone()),
                                    thinking: thinking_selection.clone(),
                                })
                                .await;
                        }
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
                    wire_api: _,
                    model: next_model,
                    base_url: _,
                    api_key: _,
                }) => {
                        // Recreate the client so new provider credentials take effect
                        // without requiring the whole app to restart.
                        model = next_model;
                        client.shutdown().await?;
                        client = spawn_client(
                            &config.cwd,
                            config.server_log_level.clone(),
                        )
                        .await?;
                        client.initialize().await?;
                        session_id = None;
                        active_turn_id = None;
                        last_query_total_tokens = 0;
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
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                            Err(_) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: "session list request timed out".to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::ListSkills) => {
                        match tokio::time::timeout(
                            Duration::from_secs(5),
                            client.skills_list(SkillListParams {
                                cwd: Some(session_cwd.clone()),
                            }),
                        )
                        .await
                        {
                            Ok(Ok(result)) => {
                                let body = render_skill_list_body(&result.skills);
                                let _ = event_tx.send(WorkerEvent::SkillsListed { body });
                            }
                            Ok(Err(error)) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                            Err(_) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: "skills list request timed out".to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::CompactSession) => {
                        let Some(active_session_id) = session_id else {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: "no active session exists yet; send a prompt or switch to a saved session first".to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                            continue;
                        };
                        if active_turn_id.is_some() {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: "cannot compact while a turn is in progress".to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                            continue;
                        }
                        match client
                            .session_compact(SessionCompactParams {
                                session_id: active_session_id,
                            })
                            .await
                        {
                            Ok(result) => {
                                model = result
                                    .session
                                    .model
                                    .clone()
                                    .unwrap_or(model);
                                thinking_selection = result.session.thinking.clone();
                                let _ = event_tx.send(WorkerEvent::SessionCompactionStarted);
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::StartNewSession) => {
                        active_turn_id = None;
                        session_id = None;
                        session_cwd = config.cwd.clone();
                        input_history_cursor = None;
                        last_query_total_tokens = 0;
                        let _ = event_tx.send(WorkerEvent::NewSessionPrepared {
                            cwd: session_cwd.clone(),
                            model: model.clone(),
                            thinking: thinking_selection.clone(),
                            reasoning_effort: None,
                            last_query_total_tokens,
                            last_query_input_tokens,
                            total_cache_read_tokens,
                        });
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
                                session_cwd = result.session.cwd.clone();
                                input_history_cursor = None;

                                let _ = event_tx.send(WorkerEvent::SessionSwitched {
                                    session_id: next_session_id.to_string(),
                                    cwd: result.session.cwd,
                                    title: result.session.title,
                                    model: result.session.model.clone(),
                                    thinking: result.session.thinking.clone(),
                                    reasoning_effort: result.session.reasoning_effort,
                                    total_input_tokens: result.session.total_input_tokens,
                                    total_output_tokens: result.session.total_output_tokens,
                                    total_cache_read_tokens: result.session.total_cache_read_tokens,
                                    last_query_total_tokens: result
                                        .session
                                        .last_query_total_tokens,
                                    last_query_input_tokens: result
                                        .latest_turn
                                        .as_ref()
                                        .and_then(|turn| turn.usage.as_ref())
                                        .map(|usage| usage.input_tokens as usize)
                                        .unwrap_or(0),
                                    prompt_token_estimate: result.session.prompt_token_estimate,
                                    history_items: project_history_items(&result.history_items),
                                    loaded_item_count: result.loaded_item_count,
                                    pending_texts: result.pending_texts,
                                });
                                model = result
                                    .session
                                    .model
                                    .clone()
                                    .unwrap_or(model);
                                thinking_selection = result.session.thinking.clone();
                                total_input_tokens = result.session.total_input_tokens;
                                total_output_tokens = result.session.total_output_tokens;
                                last_query_total_tokens =
                                    result.session.last_query_total_tokens;
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
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
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
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
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::RollbackToUserTurn(user_turn_index)) => {
                        let Some(active_session_id) = session_id else {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: "no active session exists yet; send a prompt or switch to a saved session first".to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                            continue;
                        };
                        match client
                            .session_rollback(SessionRollbackParams {
                                session_id: active_session_id,
                                user_turn_index,
                            })
                            .await
                        {
                            Ok(result) => {
                                active_turn_id = None;
                                session_cwd = result.session.cwd.clone();
                                input_history_cursor = None;
                                let _ = event_tx.send(WorkerEvent::SessionSwitched {
                                    session_id: active_session_id.to_string(),
                                    cwd: result.session.cwd,
                                    title: result.session.title,
                                    model: result.session.model.clone(),
                                    thinking: result.session.thinking.clone(),
                                    reasoning_effort: result.session.reasoning_effort,
                                    total_input_tokens: result.session.total_input_tokens,
                                    total_output_tokens: result.session.total_output_tokens,
                                    total_cache_read_tokens: result.session.total_cache_read_tokens,
                                    last_query_total_tokens: result
                                        .session
                                        .last_query_total_tokens,
                                    last_query_input_tokens: result
                                        .latest_turn
                                        .as_ref()
                                        .and_then(|turn| turn.usage.as_ref())
                                        .map(|usage| usage.input_tokens as usize)
                                        .unwrap_or(0),
                                    prompt_token_estimate: result.session.prompt_token_estimate,
                                    history_items: project_history_items(&result.history_items),
                                    loaded_item_count: result.loaded_item_count,
                                    pending_texts: result.pending_texts,
                                });
                                model = result.session.model.clone().unwrap_or(model);
                                thinking_selection = result.session.thinking.clone();
                                total_input_tokens = result.session.total_input_tokens;
                                total_output_tokens = result.session.total_output_tokens;
                                last_query_total_tokens =
                                    result.session.last_query_total_tokens;
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::ForkAtUserTurn(user_turn_index)) => {
                        let Some(active_session_id) = session_id else {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: "no active session exists yet; send a prompt or switch to a saved session first".to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                            continue;
                        };
                        match client
                            .session_fork(devo_server::SessionForkParams {
                                session_id: active_session_id,
                                title: None,
                                cwd: None,
                                user_turn_index: Some(user_turn_index),
                            })
                            .await
                        {
                            Ok(result) => {
                                let next_session_id = result.session.session_id;
                                match client
                                    .session_resume(SessionResumeParams {
                                        session_id: next_session_id,
                                    })
                                    .await
                                {
                                    Ok(resumed) => {
                                        active_turn_id = None;
                                        session_id = Some(next_session_id);
                                        session_cwd = resumed.session.cwd.clone();
                                        input_history_cursor = None;
                                        let _ = event_tx.send(WorkerEvent::SessionSwitched {
                                            session_id: next_session_id.to_string(),
                                            cwd: resumed.session.cwd,
                                            title: resumed.session.title,
                                            model: resumed.session.model.clone(),
                                            thinking: resumed.session.thinking.clone(),
                                            reasoning_effort: resumed.session.reasoning_effort,
                                            total_input_tokens: resumed.session.total_input_tokens,
                                            total_output_tokens: resumed.session.total_output_tokens,
                                            total_cache_read_tokens: resumed.session.total_cache_read_tokens,
                                            last_query_total_tokens: resumed
                                                .session
                                                .last_query_total_tokens,
                                            last_query_input_tokens: resumed
                                                .latest_turn
                                                .as_ref()
                                                .and_then(|turn| turn.usage.as_ref())
                                                .map(|usage| usage.input_tokens as usize)
                                                .unwrap_or(0),
                                            prompt_token_estimate: resumed.session.prompt_token_estimate,
                                            history_items: project_history_items(&resumed.history_items),
                                            loaded_item_count: resumed.loaded_item_count,
                                            pending_texts: resumed.pending_texts,
                                        });
                                        model = resumed.session.model.clone().unwrap_or(model);
                                        thinking_selection = resumed.session.thinking.clone();
                                        total_input_tokens = resumed.session.total_input_tokens;
                                        total_output_tokens = resumed.session.total_output_tokens;
                                        last_query_total_tokens =
                                            resumed.session.last_query_total_tokens;
                                    }
                                    Err(error) => {
                                        let _ = event_tx.send(WorkerEvent::TurnFailed {
                                            message: error.to_string(),
                                            turn_count,
                                            total_input_tokens,
                                            total_output_tokens,
                                            total_cache_read_tokens,
                                            prompt_token_estimate: total_input_tokens,
                                            last_query_input_tokens,
                                        });
                                    }
                                }
                            }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                    }
                    Some(OperationCommand::InterruptTurn) => {
                        if let (Some(turn_id), Some(active_session_id)) = (active_turn_id, session_id)
                            && let Err(error) = client
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
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                            }
                    }
                    Some(OperationCommand::SteerTurn {
                        input,
                        expected_turn_id,
                    }) => {
                        if let Some(active_session_id) = session_id {
                            match client
                                .turn_steer(TurnSteerParams {
                                    session_id: active_session_id,
                                    expected_turn_id,
                                    input,
                                })
                                .await
                            {
                                Ok(result) => {
                                    let _ = event_tx.send(WorkerEvent::SteerAccepted {
                                        turn_id: result.turn_id,
                                    });
                                }
                            Err(error) => {
                                let _ = event_tx.send(WorkerEvent::TurnFailed {
                                    message: error.to_string(),
                                    turn_count,
                                    total_input_tokens,
                                    total_output_tokens,
                                    total_cache_read_tokens,
                                    prompt_token_estimate: total_input_tokens,
                                    last_query_input_tokens,
                                });
                            }
                        }
                        }
                    }
                    Some(OperationCommand::ApprovalRespond {
                        session_id,
                        turn_id,
                        approval_id,
                        decision,
                        scope,
                    }) => {
                        if let Err(error) = client
                            .approval_respond(ApprovalRespondParams {
                                session_id,
                                turn_id,
                                approval_id: approval_id.into(),
                                decision,
                                scope,
                            })
                            .await
                        {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: error.to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                        }
                    }
                    Some(OperationCommand::UpdatePermissions { preset }) => {
                        permission_preset = preset;
                        let Some(active_session_id) = session_id else {
                            continue;
                        };
                        if let Err(error) =
                            apply_session_permissions(&mut client, active_session_id, preset).await
                        {
                            let _ = event_tx.send(WorkerEvent::TurnFailed {
                                message: error.to_string(),
                                turn_count,
                                total_input_tokens,
                                total_output_tokens,
                                total_cache_read_tokens,
                                prompt_token_estimate: total_input_tokens,
                                last_query_input_tokens,
                            });
                        }
                    }
                    Some(OperationCommand::BrowseInputHistory(direction)) => {
                        let text = if let Some(active_session_id) = session_id {
                            match client
                                .session_resume(SessionResumeParams {
                                    session_id: active_session_id,
                                })
                                .await
                            {
                                Ok(result) => {
                                    let entries = result
                                        .history_items
                                        .iter()
                                        .filter(|item| item.kind == SessionHistoryItemKind::User)
                                        .map(|item| item.body.clone())
                                        .filter(|body| !body.trim().is_empty())
                                        .collect::<Vec<_>>();
                                    let total = entries.len();
                                    match direction {
                                        InputHistoryDirection::Previous => {
                                            if total == 0 {
                                                None
                                            } else {
                                                let next_index = match input_history_cursor {
                                                    None => total.saturating_sub(1),
                                                    Some(0) => 0,
                                                    Some(index) => index.saturating_sub(1),
                                                };
                                                input_history_cursor = Some(next_index);
                                                entries.get(next_index).cloned()
                                            }
                                        }
                                        InputHistoryDirection::Next => match input_history_cursor {
                                            None => None,
                                            Some(index) if index + 1 >= total => {
                                                input_history_cursor = None;
                                                None
                                            }
                                            Some(index) => {
                                                let next_index = index + 1;
                                                input_history_cursor = Some(next_index);
                                                entries.get(next_index).cloned()
                                            }
                                        },
                                    }
                                }
                                Err(error) => {
                                    let _ = event_tx.send(WorkerEvent::TurnFailed {
                                        message: error.to_string(),
                                        turn_count,
                                        total_input_tokens,
                                        total_output_tokens,
                                        total_cache_read_tokens,
                                        prompt_token_estimate: total_input_tokens,
                                        last_query_input_tokens,
                                    });
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        let _ = event_tx.send(WorkerEvent::InputHistoryLoaded { direction, text });
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
                                    saw_usage_update_for_turn = false;
                                    model = payload.turn.model.clone();
                                    thinking_selection = payload.turn.thinking.clone();
                                    let _ = event_tx.send(WorkerEvent::TurnStarted {
                                        model: payload.turn.model,
                                        thinking: payload.turn.thinking,
                                        reasoning_effort: payload.turn.reasoning_effort,
                                        turn_id: payload.turn.turn_id,
                                    });
                                }
                                latest_completed_agent_message = None;
                            }
                            "item/started" => {
                                if let ServerEvent::ItemStarted(payload) = event
                                    && matches!(payload.item.item_kind, ItemKind::ToolCall)
                                        && let Ok(payload) = serde_json::from_value::<ToolCallPayload>(payload.item.payload) {
                                            let summary = summarize_tool_call(&payload);
                                            let _ = event_tx.send(WorkerEvent::ToolCall {
                                                tool_use_id: payload.tool_call_id,
                                                summary,
                                            });
                                        }
                                }
                            "item/agentMessage/delta" => {
                                if let ServerEvent::ItemDelta { payload, .. } = event {
                                    let _ = event_tx.send(WorkerEvent::TextDelta(payload.delta));
                                }
                            }
                            "item/commandExecution/outputDelta" => {
                                if let ServerEvent::ItemDelta { payload, .. } = event {
                                    let delta_str = &payload.delta;
                                    if let Ok(val) =
                                        serde_json::from_str::<serde_json::Value>(delta_str)
                                    {
                                        let tool_use_id = val
                                            .get("tool_use_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let text =
                                            val.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                        if !tool_use_id.is_empty() {
                                            let _ = event_tx.send(WorkerEvent::ToolOutputDelta {
                                                tool_use_id: tool_use_id.to_string(),
                                                delta: text.to_string(),
                                            });
                                        }
                                    }
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
                                            last_query_input_tokens = usage.input_tokens as usize;
                                            last_query_total_tokens = usage.input_tokens as usize
                                                + usage.output_tokens as usize;
                                            if !saw_usage_update_for_turn {
                                                total_input_tokens += usage.input_tokens as usize;
                                                total_output_tokens += usage.output_tokens as usize;
                                                total_cache_read_tokens += usage
                                                    .cache_read_input_tokens
                                                    .unwrap_or(0) as usize;
                                            }
                                        }
                                    }
                                    let _ = event_tx.send(WorkerEvent::TurnFinished {
                                        stop_reason: format!("{:?}", payload.turn.status),
                                        turn_count,
                                        total_input_tokens,
                                        total_output_tokens,
                                        total_cache_read_tokens,
                                        last_query_total_tokens,
                                        last_query_input_tokens,
                                        prompt_token_estimate: payload
                                            .turn
                                            .usage
                                            .as_ref()
                                            .map(|usage| usage.input_tokens as usize)
                                            .unwrap_or(total_input_tokens),
                                    });
                                    latest_completed_agent_message = None;
                                }
                            }
                            "turn/usage/updated" => {
                                if let ServerEvent::TurnUsageUpdated(payload) = event {
                                    saw_usage_update_for_turn = true;
                                    total_input_tokens = payload.total_input_tokens;
                                    total_output_tokens = payload.total_output_tokens;
                                    total_cache_read_tokens = payload.total_cache_read_tokens;
                                    last_query_input_tokens = payload.last_query_input_tokens;
                                    let _ = event_tx.send(WorkerEvent::UsageUpdated {
                                        total_input_tokens: payload.total_input_tokens,
                                        total_output_tokens: payload.total_output_tokens,
                                        total_cache_read_tokens: payload.total_cache_read_tokens,
                                        last_query_total_tokens: payload.usage.input_tokens as usize
                                            + payload.usage.output_tokens as usize,
                                        last_query_input_tokens: payload.last_query_input_tokens,
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
                                        last_query_input_tokens = usage.input_tokens as usize;
                                        last_query_total_tokens = usage.input_tokens as usize
                                            + usage.output_tokens as usize;
                                        if !saw_usage_update_for_turn {
                                            total_input_tokens += usage.input_tokens as usize;
                                            total_output_tokens += usage.output_tokens as usize;
                                            total_cache_read_tokens += usage
                                                .cache_read_input_tokens
                                                .unwrap_or(0) as usize;
                                        }
                                    }
                                    let _ = event_tx.send(WorkerEvent::TurnFailed {
                                        message,
                                        turn_count,
                                        total_input_tokens,
                                        total_output_tokens,
                                        total_cache_read_tokens,
                                        prompt_token_estimate: turn
                                            .usage
                                            .as_ref()
                                            .map(|usage| usage.input_tokens as usize)
                                            .unwrap_or(total_input_tokens),
                                        last_query_input_tokens: turn
                                            .usage
                                            .as_ref()
                                            .map(|usage| usage.input_tokens as usize)
                                            .unwrap_or(last_query_input_tokens),
                                    });
                                }
                            }
                            "inputQueue/updated" => {
                                if let ServerEvent::InputQueueUpdated(payload) = event {
                                    let _ = event_tx.send(WorkerEvent::InputQueueUpdated {
                                        pending_count: payload.pending_count,
                                        pending_texts: payload.pending_texts,
                                    });
                                }
                            }
                            "session/title/updated" => {
                                if let ServerEvent::SessionTitleUpdated(payload) = event
                                    && let Some(title) = payload.session.title {
                                        let _ = event_tx.send(WorkerEvent::SessionTitleUpdated {
                                            session_id: payload.session.session_id.to_string(),
                                            title,
                                        });
                                    }
                            }
                            "session/compaction/started" => {
                                let _ = event;
                            }
                            "session/compaction/completed" => {
                                if let ServerEvent::SessionCompactionCompleted(payload) = event {
                                    total_input_tokens = payload.session.total_input_tokens;
                                    total_output_tokens = payload.session.total_output_tokens;
                                    let _ = event_tx.send(WorkerEvent::SessionCompacted {
                                        total_input_tokens,
                                        total_output_tokens,
                                        prompt_token_estimate: payload.session.prompt_token_estimate,
                                    });
                                }
                            }
                            "session/compaction/failed" => {
                                if let ServerEvent::SessionCompactionFailed(payload) = event {
                                    let _ = event_tx.send(WorkerEvent::SessionCompactionFailed {
                                        message: payload.message,
                                    });
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
    cwd: &Path,
    model: &str,
    session_id: &mut Option<SessionId>,
) -> Result<EnsureSessionOutcome> {
    if let Some(session_id) = session_id {
        return Ok(EnsureSessionOutcome {
            session_id: *session_id,
            model: Some(model.to_string()),
            thinking: None,
            reasoning_effort: None,
            created: false,
        });
    }

    let session = client
        .session_start(SessionStartParams {
            cwd: cwd.to_path_buf(),
            ephemeral: false,
            title: None,
            model: Some(model.to_string()),
        })
        .await?;
    *session_id = Some(session.session.session_id);
    Ok(EnsureSessionOutcome {
        session_id: session.session.session_id,
        model: session.session.model,
        thinking: session.session.thinking,
        reasoning_effort: session.session.reasoning_effort,
        created: true,
    })
}

async fn apply_session_permissions(
    client: &mut StdioServerClient,
    session_id: SessionId,
    preset: PermissionPreset,
) -> Result<()> {
    client
        .session_permissions_update(devo_server::SessionPermissionsUpdateParams {
            session_id,
            preset,
        })
        .await?;
    Ok(())
}

async fn spawn_client(cwd: &Path, server_log_level: Option<String>) -> Result<StdioServerClient> {
    let program = std::env::current_exe().context("resolve current executable for server child")?;
    StdioServerClient::spawn(StdioServerClientConfig {
        // Re-exec the current binary and enter the hidden server subcommand.
        program,
        workspace_root: Some(cwd.to_path_buf()),
        args: std::iter::once("server".to_string())
            .chain(["--transport".to_string(), "stdio".to_string()])
            .chain(
                server_log_level
                    .into_iter()
                    .flat_map(|level| ["--log-level".to_string(), level]),
            )
            .collect(),
    })
    .await
}

fn render_skill_list_body(skills: &[devo_server::SkillRecord]) -> String {
    if skills.is_empty() {
        return "No skills found".to_string();
    }

    skills
        .iter()
        .map(|skill| {
            let status = if skill.enabled { "enabled" } else { "disabled" };
            format!(
                "{} ({status})\n{}\nsource: {}\npath: {}",
                skill.name,
                skill.description,
                render_skill_source(&skill.source),
                skill.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_skill_source(source: &SkillSource) -> String {
    match source {
        SkillSource::User => "user".to_string(),
        SkillSource::Workspace { cwd } => format!("workspace ({})", cwd.display()),
        SkillSource::Plugin { plugin_id } => format!("plugin ({plugin_id})"),
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
            // ToolCall is now handled via item/started; skip duplicate emission from
            // item/completed since it arrives later (after the tool actually finishes).
            let _ = payload;
        }
        ItemEnvelope {
            item_kind: ItemKind::ToolResult,
            payload,
            ..
        } => {
            let Ok(payload) = serde_json::from_value::<ToolResultPayload>(payload) else {
                return;
            };
            let title = if payload.summary.is_empty() {
                summarize_tool_result_title(payload.tool_name.as_deref(), payload.is_error)
            } else {
                payload.summary
            };
            let _ = event_tx.send(WorkerEvent::ToolResult {
                tool_use_id: payload.tool_call_id,
                title,
                preview: render_json_value_text(&payload.content),
                is_error: payload.is_error,
                truncated: false,
            });
        }
        ItemEnvelope {
            item_kind: ItemKind::ApprovalRequest,
            payload,
            ..
        } => {
            let Ok(payload) = serde_json::from_value::<ApprovalRequestPayload>(payload) else {
                return;
            };
            let Some(turn_id) = payload.request.turn_id else {
                return;
            };
            let _ = event_tx.send(WorkerEvent::ApprovalRequest {
                session_id: payload.request.session_id,
                turn_id,
                approval_id: payload.approval_id.to_string(),
                action_summary: payload.action_summary,
                justification: payload.justification,
                resource: payload.resource,
                available_scopes: payload.available_scopes,
                path: payload.path,
                host: payload.host,
                target: payload.target,
            });
        }
        ItemEnvelope {
            item_kind: ItemKind::ApprovalDecision,
            payload,
            ..
        } => {
            let Ok(payload) = serde_json::from_value::<ApprovalDecisionPayload>(payload) else {
                return;
            };
            let _ = event_tx.send(WorkerEvent::ApprovalDecision {
                approval_id: payload.approval_id.to_string(),
                decision: payload.decision,
                scope: payload.scope,
            });
        }
        _ => {}
    }
}

fn project_history_items(items: &[SessionHistoryItem]) -> Vec<TranscriptItem> {
    use std::collections::HashMap;

    let mut paired_result_by_call_id = HashMap::new();
    let mut consumed_result_indexes = HashMap::new();

    for (index, item) in items.iter().enumerate() {
        if matches!(
            item.kind,
            SessionHistoryItemKind::ToolResult | SessionHistoryItemKind::Error
        ) && let Some(tool_call_id) = item.tool_call_id.as_deref()
        {
            paired_result_by_call_id
                .entry(tool_call_id.to_string())
                .or_insert(index);
        }
    }

    let mut transcript = Vec::new();
    let mut index = 0usize;

    while index < items.len() {
        let item = &items[index];
        if item.kind == SessionHistoryItemKind::ToolCall
            && let Some(tool_call_id) = item.tool_call_id.as_deref()
            && let Some(result_index) = paired_result_by_call_id.get(tool_call_id).copied()
        {
            let result_item = &items[result_index];
            consumed_result_indexes.insert(result_index, ());
            let mut ti = if result_item.kind == SessionHistoryItemKind::Error {
                TranscriptItem::tool_error(item.title.clone(), result_item.body.clone())
            } else {
                TranscriptItem::restored_tool_result(item.title.clone(), result_item.body.clone())
            };
            if let Some(duration_ms) = result_item.duration_ms {
                ti = ti.with_duration(duration_ms);
            }
            transcript.push(ti);
            index += 1;
            continue;
        }

        if consumed_result_indexes.contains_key(&index) {
            index += 1;
            continue;
        }

        let kind = match item.kind {
            SessionHistoryItemKind::User => TranscriptItemKind::User,
            SessionHistoryItemKind::Assistant => TranscriptItemKind::Assistant,
            SessionHistoryItemKind::Reasoning => TranscriptItemKind::Reasoning,
            SessionHistoryItemKind::ToolCall => TranscriptItemKind::ToolCall,
            SessionHistoryItemKind::ToolResult => TranscriptItemKind::ToolResult,
            SessionHistoryItemKind::Error => TranscriptItemKind::Error,
            SessionHistoryItemKind::TurnSummary => TranscriptItemKind::TurnSummary,
        };
        let mut transcript_item = match item.kind {
            SessionHistoryItemKind::ToolCall => TranscriptItem::tool_call(item.title.clone()),
            SessionHistoryItemKind::ToolResult => {
                TranscriptItem::restored_tool_result(item.title.clone(), item.body.clone())
            }
            SessionHistoryItemKind::Error => {
                TranscriptItem::tool_error(item.title.clone(), item.body.clone())
            }
            SessionHistoryItemKind::TurnSummary => {
                // TurnSummary uses title for model name, duration_ms for duration in seconds
                TranscriptItem::new(kind, item.title.clone(), String::new())
            }
            SessionHistoryItemKind::User
            | SessionHistoryItemKind::Assistant
            | SessionHistoryItemKind::Reasoning => {
                TranscriptItem::new(kind, item.title.clone(), item.body.clone())
            }
        };
        if let Some(duration_ms) = item.duration_ms {
            transcript_item = transcript_item.with_duration(duration_ms);
        }
        transcript.push(transcript_item);
        index += 1;
    }

    transcript
}

fn summarize_tool_result_title(tool_name: Option<&str>, is_error: bool) -> String {
    match (tool_name, is_error) {
        (Some(tool_name), true) => format!("{tool_name} error"),
        (Some(tool_name), false) => format!("{tool_name} output"),
        (None, true) => "Tool error".to_string(),
        (None, false) => "Tool output".to_string(),
    }
}

fn summarize_tool_call(payload: &ToolCallPayload) -> String {
    let detail = summarize_tool_input(&payload.tool_name, &payload.parameters);
    if detail.is_empty() {
        payload.tool_name.clone()
    } else {
        format!("{} {detail}", payload.tool_name)
    }
}

fn make_path_relative(path: &str) -> String {
    let p = std::path::PathBuf::from(path);
    if p.is_absolute()
        && let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = p.strip_prefix(&cwd)
    {
        return rel.to_string_lossy().to_string();
    }
    path.to_string()
}

fn fmt_offset_limit(input: &serde_json::Value) -> String {
    let offset = input.get("offset").and_then(|v| v.as_u64());
    let limit = input.get("limit").and_then(|v| v.as_u64());
    match (offset, limit) {
        (Some(o), Some(l)) => format!(" (offset:{o}, limit:{l})"),
        (Some(o), None) => format!(" (offset:{o})"),
        (None, Some(l)) => format!(" (limit:{l})"),
        (None, None) => String::new(),
    }
}

fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    let candidate = match tool_name {
        "bash" | "shell_command" | "exec_command" => input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("cmd").and_then(serde_json::Value::as_str))
            .map(|s| s.to_string()),
        "read" => input
            .get("filePath")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("path").and_then(serde_json::Value::as_str))
            .map(|path| {
                let rel = make_path_relative(path);
                let ext = fmt_offset_limit(input);
                format!("{rel}{ext}")
            }),
        "write" | "edit" | "apply_patch" => input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("filePath").and_then(serde_json::Value::as_str))
            .map(make_path_relative),
        "grep" => {
            let pattern = input
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let path = input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(make_path_relative);
            match path {
                Some(p) => Some(format!("'{pattern}' in {p}")),
                None => Some(format!("'{pattern}'")),
            }
        }
        "glob" => {
            let pattern = input
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let path = input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(make_path_relative);
            match path {
                Some(p) => Some(format!("{pattern} in {p}")),
                None => Some(pattern.to_string()),
            }
        }
        "webfetch" | "websearch" => input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string())
            .or_else(|| {
                input
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .map(|s| s.to_string())
            }),
        "lsp" => {
            let path = input
                .get("filePath")
                .and_then(serde_json::Value::as_str)
                .map(make_path_relative);
            let line = input.get("line").and_then(|v| v.as_i64());
            let col = input.get("character").and_then(|v| v.as_i64());
            match (path, line, col) {
                (Some(p), Some(l), Some(c)) => Some(format!("{p}:{l}:{c}")),
                (Some(p), Some(l), None) => Some(format!("{p}:{l}")),
                (Some(p), None, _) => Some(p),
                _ => None,
            }
        }
        "task" => input
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string()),
        "question" => None,
        "todowrite" => None,
        "skill" => input
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|s| s.to_string()),
        _ => None,
    };

    candidate
        .map(|text| compact_tool_summary(&text, 96))
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
    provider: ProviderWireApi,
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

fn resolve_validation_model(provider: ProviderWireApi, model: &str) -> Result<Model> {
    let catalog = PresetModelCatalog::load()?;
    if let Some(entry) = catalog.get(model) {
        return Ok(entry.clone());
    }
    Ok(Model {
        slug: model.to_string(),
        provider,
        ..Model::default()
    })
}

fn build_validation_provider(
    provider: ProviderWireApi,
    base_url: Option<String>,
    api_key: Option<String>,
) -> Result<std::sync::Arc<dyn ModelProviderSDK>> {
    match provider {
        ProviderWireApi::AnthropicMessages => {
            let api_key = api_key.context("anthropic provider requires an API key")?;
            let base_url = base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string());
            Ok(std::sync::Arc::new(
                AnthropicProvider::new(base_url).with_api_key(api_key),
            ))
        }
        ProviderWireApi::OpenAIChatCompletions => {
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
        ProviderWireApi::OpenAIResponses => {
            let base_url = normalize_openai_base_url(
                &base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            );
            let provider = if let Some(api_key) = api_key {
                OpenAIResponsesProvider::new(base_url).with_api_key(api_key)
            } else {
                OpenAIResponsesProvider::new(base_url)
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

    use devo_core::SessionId;
    use devo_core::SessionTitleState;
    use devo_server::SessionMetadata;
    use devo_server::SessionRuntimeStatus;

    use super::normalize_display_output;
    use super::project_history_items;
    use super::summarize_tool_call;
    use super::truncate_tool_output;
    use crate::events::SessionListEntry;
    use crate::events::TranscriptItem;
    use crate::events::TranscriptItemKind;
    use devo_server::SessionHistoryItem;
    use devo_server::SessionHistoryItemKind;
    use devo_server::ToolCallPayload;

    #[test]
    fn bash_tool_summary_uses_command_text() {
        let payload = ToolCallPayload {
            tool_call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            parameters: serde_json::json!({
                "command": "Get-Date -Format \"yyyy-MM-dd\""
            }),
        };

        assert_eq!(
            summarize_tool_call(&payload),
            "bash Get-Date -Format \"yyyy-MM-dd\""
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
        let summary = SessionMetadata {
            session_id: active_session_id,
            cwd: ".".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Saved conversation".to_string()),
            title_state: SessionTitleState::Provisional,
            ephemeral: false,
            model: Some("test-model".to_string()),
            thinking: None,
            reasoning_effort: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            prompt_token_estimate: 0,
            last_query_total_tokens: 0,
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
        let summary = SessionMetadata {
            session_id: SessionId::new(),
            cwd: ".".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Saved conversation".to_string()),
            title_state: SessionTitleState::Provisional,
            ephemeral: false,
            model: Some("test-model".to_string()),
            thinking: None,
            reasoning_effort: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            prompt_token_estimate: 0,
            last_query_total_tokens: 0,
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
                tool_call_id: Some("call-1".to_string()),
                kind: SessionHistoryItemKind::ToolCall,
                title: "Ran powershell -Command \"Get-Date\"".to_string(),
                body: String::new(),
                duration_ms: None,
            },
            SessionHistoryItem {
                tool_call_id: Some("call-1".to_string()),
                kind: SessionHistoryItemKind::ToolResult,
                title: "Tool output".to_string(),
                body: "2026-04-09".to_string(),
                duration_ms: None,
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

    #[test]
    fn project_history_pairs_tool_results_by_call_id_not_time_adjacency() {
        let items = vec![
            SessionHistoryItem {
                tool_call_id: Some("call-a".to_string()),
                kind: SessionHistoryItemKind::ToolCall,
                title: "Ran read a".to_string(),
                body: String::new(),
                duration_ms: None,
            },
            SessionHistoryItem {
                tool_call_id: Some("call-b".to_string()),
                kind: SessionHistoryItemKind::ToolCall,
                title: "Ran read b".to_string(),
                body: String::new(),
                duration_ms: None,
            },
            SessionHistoryItem {
                tool_call_id: Some("call-b".to_string()),
                kind: SessionHistoryItemKind::ToolResult,
                title: "Tool output".to_string(),
                body: "B".to_string(),
                duration_ms: None,
            },
            SessionHistoryItem {
                tool_call_id: Some("call-a".to_string()),
                kind: SessionHistoryItemKind::ToolResult,
                title: "Tool output".to_string(),
                body: "A".to_string(),
                duration_ms: None,
            },
        ];

        assert_eq!(
            project_history_items(&items),
            vec![
                TranscriptItem::restored_tool_result("Ran read a", "A"),
                TranscriptItem::restored_tool_result("Ran read b", "B"),
            ]
        );
    }

    #[test]
    fn project_history_preserves_reasoning_items() {
        let items = vec![SessionHistoryItem {
            tool_call_id: None,
            kind: SessionHistoryItemKind::Reasoning,
            title: String::new(),
            body: "thinking aloud".to_string(),
            duration_ms: None,
        }];

        assert_eq!(
            project_history_items(&items),
            vec![TranscriptItem::new(
                TranscriptItemKind::Reasoning,
                "",
                "thinking aloud"
            )]
        );
    }
}
