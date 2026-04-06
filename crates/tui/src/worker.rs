use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::{
    sync::mpsc,
    task::{JoinError, JoinHandle},
};

use clawcr_core::{query, Message, QueryEvent, SessionConfig, SessionState};
use clawcr_provider::ModelProvider;
use clawcr_safety::legacy_permissions::PermissionMode;
use clawcr_tools::{ToolOrchestrator, ToolRegistry};

use crate::events::WorkerEvent;

/// Immutable runtime configuration used to construct the background query worker.
pub(crate) struct QueryWorkerConfig {
    /// Model identifier used for requests.
    pub(crate) model: String,
    /// System prompt used for requests.
    pub(crate) system_prompt: String,
    /// Maximum number of turns allowed in the session.
    pub(crate) max_turns: usize,
    /// Permission mode used by tool execution.
    pub(crate) permission_mode: PermissionMode,
    /// Working directory used for the session.
    pub(crate) cwd: PathBuf,
    /// Provider instance used for requests.
    pub(crate) provider: Box<dyn ModelProvider>,
}

/// Commands accepted by the background query worker.
enum WorkerCommand {
    /// Submit a new user prompt to the session.
    SubmitPrompt(String),
    /// Stop the worker loop.
    Shutdown,
}

/// Handle used by the UI thread to interact with the background query worker.
pub(crate) struct QueryWorkerHandle {
    /// Sender used to submit commands to the worker.
    command_tx: mpsc::UnboundedSender<WorkerCommand>,
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
            .send(WorkerCommand::SubmitPrompt(prompt))
            .map_err(|_| anyhow::anyhow!("interactive worker is no longer running"))
    }

    /// Stops the worker task and waits for it to finish.
    pub(crate) async fn shutdown(self) -> Result<()> {
        let _ = self.command_tx.send(WorkerCommand::Shutdown);
        self.join_handle.abort();
        let _ = self.join_handle.await.map_err(map_join_error);
        Ok(())
    }
}

async fn run_worker(
    config: QueryWorkerConfig,
    mut command_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    event_tx: mpsc::UnboundedSender<WorkerEvent>,
) {
    let mut registry = ToolRegistry::new();
    clawcr_tools::register_builtin_tools(&mut registry);
    let registry = Arc::new(registry);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

    let mut session = SessionState::new(
        SessionConfig {
            model: config.model,
            system_prompt: config.system_prompt,
            max_turns: config.max_turns,
            permission_mode: config.permission_mode,
            ..Default::default()
        },
        config.cwd,
    );
    let provider = config.provider;

    while let Some(command) = command_rx.recv().await {
        match command {
            WorkerCommand::SubmitPrompt(prompt) => {
                let _ = event_tx.send(WorkerEvent::TurnStarted);
                session.push_message(Message::user(prompt));

                let callback_tx = event_tx.clone();
                let callback = Arc::new(move |event: QueryEvent| {
                    let mapped = match event {
                        QueryEvent::TextDelta(text) => WorkerEvent::TextDelta(text),
                        QueryEvent::ToolUseStart { name, .. } => WorkerEvent::ToolCall { name },
                        QueryEvent::ToolResult {
                            content, is_error, ..
                        } => WorkerEvent::ToolResult { content, is_error },
                        QueryEvent::TurnComplete { .. } => return,
                        QueryEvent::Usage {
                            input_tokens,
                            output_tokens,
                            ..
                        } => WorkerEvent::Usage {
                            input_tokens,
                            output_tokens,
                        },
                    };
                    let _ = callback_tx.send(mapped);
                });

                let query_result = query(
                    &mut session,
                    provider.as_ref(),
                    Arc::clone(&registry),
                    &orchestrator,
                    Some(callback),
                )
                .await;

                match query_result {
                    Ok(()) => {
                        let _ = event_tx.send(WorkerEvent::TurnFinished {
                            stop_reason: "completed".to_string(),
                            turn_count: session.turn_count,
                            total_input_tokens: session.total_input_tokens,
                            total_output_tokens: session.total_output_tokens,
                        });
                    }
                    Err(error) => {
                        let _ = event_tx.send(WorkerEvent::TurnFailed {
                            message: error.to_string(),
                            turn_count: session.turn_count,
                            total_input_tokens: session.total_input_tokens,
                            total_output_tokens: session.total_output_tokens,
                        });
                    }
                }
            }
            WorkerCommand::Shutdown => break,
        }
    }
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
