use std::collections::HashMap;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use chrono::Utc;
use futures::FutureExt;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use devo_core::ItemId;
use devo_core::Message;
use devo_core::QueryEvent;
use devo_core::SessionId;
use devo_core::SessionTitleFinalSource;
use devo_core::SessionTitleState;
use devo_core::TextItem;
use devo_core::ToolCallItem;
use devo_core::ToolResultItem;
use devo_core::TurnConfig;
use devo_core::TurnId;
use devo_core::TurnItem;
use devo_core::TurnStatus;
use devo_core::TurnUsage;
use devo_core::Worklog;
use devo_core::history::compaction::CompactAction;
use devo_core::history::compaction::CompactionConfig;
use devo_core::history::compaction::CompactionKind;
use devo_core::history::compaction::compact_history;
use devo_core::history::summarizer::DefaultHistorySummarizer;
use devo_core::message_to_response_items;
use devo_core::query;
use devo_core::{ResponseItem, TokenInfo};
use devo_tools::ToolRuntime;

use crate::ClientTransportKind;
use crate::ConnectionState;
use crate::ErrorResponse;
use crate::EventContext;
use crate::EventsSubscribeParams;
use crate::EventsSubscribeResult;
use crate::InitializeParams;
use crate::InitializeResult;
use crate::ItemDeltaKind;
use crate::ItemDeltaPayload;
use crate::ItemEnvelope;
use crate::ItemEventPayload;
use crate::ItemKind;
use crate::NotificationEnvelope;
use crate::ProtocolError;
use crate::ProtocolErrorCode;
use crate::ServerCapabilities;
use crate::ServerEvent;
use crate::ServerRequestResolvedPayload;
use crate::SessionCompactParams;
use crate::SessionCompactResult;
use crate::SessionCompactionFailedPayload;
use crate::SessionEventPayload;
use crate::SessionForkParams;
use crate::SessionForkResult;
use crate::SessionListParams;
use crate::SessionListResult;
use crate::SessionMetadataUpdateParams;
use crate::SessionMetadataUpdateResult;
use crate::SessionResumeParams;
use crate::SessionResumeResult;
use crate::SessionRuntimeStatus;
use crate::SessionStartParams;
use crate::SessionStartResult;
use crate::SessionStatusChangedPayload;
use crate::SessionTitleUpdateParams;
use crate::SessionTitleUpdateResult;
use crate::SuccessResponse;
use crate::ToolCallPayload;
use crate::ToolResultPayload;
use crate::TurnEventPayload;
use crate::TurnInterruptParams;
use crate::TurnInterruptResult;
use crate::TurnMetadata;
use crate::TurnStartParams;
use crate::TurnStartResult;
use crate::TurnSteerParams;
use crate::TurnSteerResult;
use crate::TurnUsageUpdatedPayload;
use crate::db::QueueType;
use crate::db::SessionStats;
use crate::execution::RuntimeSession;
use crate::execution::ServerRuntimeDependencies;
use crate::persistence::RolloutStore;
use crate::persistence::build_item_record;
use crate::persistence::build_turn_record;
use crate::projection::history_item_from_turn_item;
use crate::titles::build_title_generation_request;
use crate::titles::derive_provisional_title;
use crate::titles::normalize_generated_title;

mod skills;

pub struct ServerRuntime {
    metadata: InitializeResult,
    deps: ServerRuntimeDependencies,
    rollout_store: RolloutStore,
    sessions: Mutex<HashMap<SessionId, Arc<Mutex<RuntimeSession>>>>,
    connections: Mutex<HashMap<u64, ConnectionRuntime>>,
    active_tasks: Mutex<HashMap<SessionId, tokio::task::AbortHandle>>,
    next_connection_id: AtomicU64,
    /// High-priority channel for RPC responses that must not be blocked by
    /// event notifications (TextDelta, etc.). Set by the stdio transport on
    /// startup. When set, `handle_turn_start` sends busy-path responses here
    /// so they bypass the shared event channel.
    high_pri_tx: Mutex<Option<mpsc::UnboundedSender<serde_json::Value>>>,
}

impl ServerRuntime {
    pub fn new(server_home: PathBuf, deps: ServerRuntimeDependencies) -> Arc<Self> {
        let rollout_store = RolloutStore::new(server_home.clone());
        Arc::new(Self {
            metadata: InitializeResult {
                server_name: "devo-server".into(),
                server_version: env!("CARGO_PKG_VERSION").into(),
                platform_family: std::env::consts::FAMILY.into(),
                platform_os: std::env::consts::OS.into(),
                server_home,
                capabilities: ServerCapabilities {
                    session_resume: true,
                    session_fork: true,
                    turn_interrupt: true,
                    approval_requests: true,
                    event_streaming: true,
                },
            },
            deps,
            rollout_store,
            sessions: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            active_tasks: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
            high_pri_tx: Mutex::new(None),
        })
    }

    /// Register a high-priority response channel. When set, RPC handlers that
    /// need to bypass the shared event channel (e.g. turn/start during a busy
    /// turn) can send their responses here instead.
    pub async fn set_high_pri_sender(&self, tx: mpsc::UnboundedSender<serde_json::Value>) {
        *self.high_pri_tx.lock().await = Some(tx);
    }

    /// Loads durable sessions from rollout files and installs them into the runtime map.
    /// Also restores token stats and pending queues from SQLite.
    pub async fn load_persisted_sessions(self: &Arc<Self>) -> anyhow::Result<()> {
        let sessions = self.rollout_store.load_sessions(&self.deps)?;
        tracing::info!(session_count = sessions.len(), "loaded persisted sessions");

        // Restore token stats and pending queues from SQLite
        for (session_id, session_arc) in &sessions {
            let mut session = session_arc.lock().await;

            // Skip ephemeral sessions
            if session.summary.ephemeral {
                continue;
            }

            // Restore token stats from SQLite
            match self.deps.db.get_stats(session_id) {
                Ok(Some(stats)) => {
                    session.summary.total_input_tokens = stats.total_input_tokens;
                    session.summary.total_output_tokens = stats.total_output_tokens;
                    session.summary.prompt_token_estimate = stats.prompt_token_estimate;
                    if let Ok(mut core) = session.core_session.try_lock() {
                        core.total_input_tokens = stats.total_input_tokens;
                        core.total_output_tokens = stats.total_output_tokens;
                        core.total_cache_creation_tokens = stats.total_cache_creation_tokens;
                        core.total_cache_read_tokens = stats.total_cache_read_tokens;
                        core.last_input_tokens = stats.last_input_tokens;
                        core.prompt_token_estimate = stats.prompt_token_estimate;
                    }
                    tracing::debug!(
                        session_id = %session_id,
                        "restored token stats from database"
                    );
                }
                Ok(None) => {
                    // No stats in database, persist current stats
                    let stats = crate::db::SessionStats {
                        total_input_tokens: session.summary.total_input_tokens,
                        total_output_tokens: session.summary.total_output_tokens,
                        total_cache_creation_tokens: 0,
                        total_cache_read_tokens: 0,
                        last_input_tokens: 0,
                        turn_count: 0,
                        prompt_token_estimate: session.summary.prompt_token_estimate,
                    };
                    if let Err(err) = self.deps.db.update_stats(session_id, &stats) {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %err,
                            "failed to persist initial token stats to database"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %err,
                        "failed to load token stats from database"
                    );
                }
            }

            // Restore pending turn queue from SQLite
            match self
                .deps
                .db
                .drain_pending(session_id, crate::db::QueueType::Turn)
            {
                Ok(items) => {
                    if !items.is_empty() {
                        let mut queue = session
                            .pending_turn_queue
                            .lock()
                            .expect("pending turn queue mutex should not be poisoned");
                        queue.extend(items);
                        tracing::debug!(
                            session_id = %session_id,
                            pending_count = queue.len(),
                            "restored pending turn queue from database"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %err,
                        "failed to load pending turn queue from database"
                    );
                }
            }

            // Clear any stale btw inputs from previous session
            if let Err(err) = self
                .deps
                .db
                .clear_pending(session_id, crate::db::QueueType::Btw)
            {
                tracing::warn!(
                    session_id = %session_id,
                    error = %err,
                    "failed to clear stale btw inputs from database"
                );
            }
        }

        let mut runtime_sessions = self.sessions.lock().await;
        runtime_sessions.extend(sessions);
        Ok(())
    }

    /// Completes deferred (in-progress) items for all active turns and
    /// persists interrupted turn records. Called on graceful shutdown.
    pub async fn shutdown(self: &Arc<Self>) {
        let session_ids: Vec<SessionId> = {
            let sessions = self.sessions.lock().await;
            sessions.keys().cloned().collect()
        };

        for session_id in session_ids {
            let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
                continue;
            };

            let (deferred_assistant, deferred_reasoning, turn_id, record) = {
                let mut session = session_arc.lock().await;
                let turn_id = session.active_turn.as_ref().map(|t| t.turn_id);
                (
                    session.deferred_assistant.take(),
                    session.deferred_reasoning.take(),
                    turn_id,
                    session.record.clone(),
                )
            };

            let Some(turn_id) = turn_id else {
                continue;
            };

            // Complete deferred items before shutting down
            if let Some((item_id, item_seq, text)) = deferred_assistant {
                self.complete_item(
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
            if let Some((item_id, item_seq, text)) = deferred_reasoning {
                self.complete_item(
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

            // Mark turn as interrupted
            let interrupted_turn = {
                let mut session = session_arc.lock().await;
                let Some(mut turn) = session.active_turn.take() else {
                    continue;
                };
                if turn.turn_id != turn_id {
                    session.active_turn = Some(turn);
                    continue;
                }
                turn.status = TurnStatus::Interrupted;
                turn.completed_at = Some(Utc::now());
                session.latest_turn = Some(turn.clone());
                session.summary.status = SessionRuntimeStatus::Idle;
                session.summary.updated_at = Utc::now();
                let token_totals = session
                    .core_session
                    .try_lock()
                    .ok()
                    .map(|core| (core.total_input_tokens, core.total_output_tokens));
                if let Some((input, output)) = token_totals {
                    session.summary.total_input_tokens = input;
                    session.summary.total_output_tokens = output;
                }
                turn
            };

            // Persist interrupted turn record
            if let Some(record) = record {
                let (session_context, turn_context) = {
                    let session = session_arc.lock().await;
                    let core = session.core_session.lock().await;
                    (
                        core.session_context.clone(),
                        core.latest_turn_context.clone(),
                    )
                };
                if let Err(error) = self.rollout_store.append_turn(
                    &record,
                    build_turn_record(&interrupted_turn, session_context, turn_context),
                ) {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %error,
                        "failed to persist interrupted turn on shutdown"
                    );
                }
            }

            tracing::info!(
                session_id = %session_id,
                turn_id = %interrupted_turn.turn_id,
                "completed deferred items and interrupted turn on shutdown"
            );
        }
    }

    pub async fn register_connection(
        self: &Arc<Self>,
        transport: ClientTransportKind,
        sender: mpsc::UnboundedSender<serde_json::Value>,
    ) -> u64 {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::SeqCst);
        let mut connections = self.connections.lock().await;
        connections.insert(
            connection_id,
            ConnectionRuntime {
                transport,
                state: ConnectionState::Connected,
                sender,
                opt_out_notification_methods: HashSet::new(),
                subscriptions: Vec::new(),
                next_event_seq: 1,
            },
        );
        tracing::info!(
            connection_id,
            transport = ?connections
                .get(&connection_id)
                .map(|connection| connection.transport.clone())
                .expect("connection inserted"),
            active_connections = connections.len(),
            "registered client connection"
        );
        connection_id
    }

    pub async fn unregister_connection(&self, connection_id: u64) {
        let mut connections = self.connections.lock().await;
        let removed = connections.remove(&connection_id);
        tracing::info!(
            connection_id,
            transport = ?removed.as_ref().map(|connection| connection.transport.clone()),
            active_connections = connections.len(),
            "unregistered client connection"
        );
    }

    pub async fn handle_incoming(
        self: &Arc<Self>,
        connection_id: u64,
        message: serde_json::Value,
    ) -> Option<serde_json::Value> {
        let method = message.get("method")?.as_str()?.to_string();
        let id = message.get("id").cloned();
        let params = message
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        tracing::debug!(
            connection_id,
            method,
            has_id = id.is_some(),
            "received client message"
        );

        if method == "initialized" {
            if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
                connection.state = ConnectionState::Ready;
            }
            tracing::info!(connection_id, "client completed initialized handshake");
            return None;
        }
        if method == "initialize" {
            return Some(self.handle_initialize(connection_id, id, params).await);
        }
        if !self.connection_ready(connection_id).await {
            return id.map(|request_id| {
                self.error_response(
                    request_id,
                    ProtocolErrorCode::NotInitialized,
                    "connection has not completed initialize/initialized",
                )
            });
        }

        let response = match method.as_str() {
            "session/start" => Some(self.handle_session_start(connection_id, id?, params).await),
            "session/list" => Some(self.handle_session_list(id?, params).await),
            "session/metadata/update" => {
                Some(self.handle_session_metadata_update(id?, params).await)
            }
            "session/title/update" => Some(self.handle_session_title_update(id?, params).await),
            "session/resume" => Some(self.handle_session_resume(connection_id, id?, params).await),
            "session/fork" => Some(self.handle_session_fork(connection_id, id?, params).await),
            "session/compact" => Some(self.handle_session_compact(id?, params).await),
            "skills/list" => Some(self.handle_skills_list(id?, params).await),
            "skills/changed" => Some(self.handle_skills_changed(id?, params).await),
            "turn/start" => Some(self.handle_turn_start(id?, params).await),
            "turn/interrupt" => Some(self.handle_turn_interrupt(id?, params).await),
            "turn/steer" => Some(self.handle_turn_steer(connection_id, id?, params).await),
            "approval/respond" => Some(self.error_response(
                id?,
                ProtocolErrorCode::ApprovalNotFound,
                "no pending approval request exists for this runtime",
            )),
            "events/subscribe" => Some(
                self.handle_events_subscribe(connection_id, id?, params)
                    .await,
            ),
            _ => Some(self.error_response(
                id?,
                ProtocolErrorCode::InvalidParams,
                format!("unknown method: {method}"),
            )),
        };
        // Filter out responses already dispatched via the high-priority channel.
        match response {
            Some(serde_json::Value::Null) => None,
            other => other,
        }
    }

    async fn handle_initialize(
        &self,
        connection_id: u64,
        id: Option<serde_json::Value>,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let request_id = id.unwrap_or(serde_json::Value::Null);
        match serde_json::from_value::<InitializeParams>(params) {
            Ok(params) => {
                let transport = params.transport.clone();
                let opt_out_notification_count = params.opt_out_notification_methods.len();
                if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
                    connection.state = ConnectionState::Initializing;
                    connection.transport = params.transport;
                    connection.opt_out_notification_methods =
                        params.opt_out_notification_methods.into_iter().collect();
                }
                tracing::info!(
                    connection_id,
                    client_name = %params.client_name,
                    client_version = %params.client_version,
                    transport = ?transport,
                    supports_streaming = params.supports_streaming,
                    supports_binary_images = params.supports_binary_images,
                    opt_out_notification_count,
                    "accepted initialize request"
                );
                serde_json::to_value(SuccessResponse {
                    id: request_id,
                    result: self.metadata.clone(),
                })
                .expect("serialize initialize result")
            }
            Err(error) => self.error_response(
                request_id,
                ProtocolErrorCode::InvalidParams,
                format!("invalid initialize params: {error}"),
            ),
        }
    }

    async fn handle_session_start(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionStartParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/start params: {error}"),
                );
            }
        };

        let now = Utc::now();
        let session_id = SessionId::new();
        let model = params
            .model
            .clone()
            .unwrap_or_else(|| self.deps.default_model.clone());
        let record = (!params.ephemeral).then(|| {
            self.rollout_store.create_session_record(
                session_id,
                now,
                params.cwd.clone(),
                params.title.clone(),
                Some(model.clone()),
                None,
                self.deps.provider.name().to_string(),
                None,
            )
        });
        let summary = crate::SessionMetadata {
            session_id,
            cwd: params.cwd.clone(),
            created_at: now,
            updated_at: now,
            title: params.title.clone(),
            title_state: params
                .title
                .as_ref()
                .map(|_| SessionTitleState::Final(SessionTitleFinalSource::ExplicitCreate))
                .unwrap_or(SessionTitleState::Unset),
            ephemeral: params.ephemeral,
            model: Some(model.clone()),
            thinking: None,
            reasoning_effort: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            prompt_token_estimate: 0,
            status: SessionRuntimeStatus::Idle,
        };
        if let Some(record) = &record
            && let Err(error) = self.rollout_store.append_session_meta(record)
        {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to persist session metadata: {error}"),
            );
        }
        let core_session = self.deps.new_session_state(session_id, params.cwd.clone());
        let pending_turn_queue = Arc::clone(&core_session.pending_turn_queue);
        let btw_input_queue = Arc::clone(&core_session.btw_input_queue);
        self.sessions.lock().await.insert(
            session_id,
            RuntimeSession {
                record,
                summary: summary.clone(),
                core_session: Arc::new(Mutex::new(core_session)),
                active_turn: None,
                latest_turn: None,
                loaded_item_count: 0,
                history_items: Vec::new(),
                persisted_turn_items: Vec::new(),
                latest_compaction_snapshot: None,
                pending_turn_queue,
                btw_input_queue,
                active_task: None,
                deferred_assistant: None,
                deferred_reasoning: None,
                next_item_seq: 1,
                first_user_input: None,
            }
            .shared(),
        );
        self.subscribe_connection_to_session(connection_id, session_id, None)
            .await;

        // Persist session metadata to SQLite (skip for ephemeral sessions)
        if !summary.ephemeral
            && let Err(err) = self.deps.db.upsert_session(&summary)
        {
            tracing::warn!(
                session_id = %session_id,
                error = %err,
                "failed to persist session metadata to database"
            );
        }

        tracing::info!(
            connection_id,
            session_id = %session_id,
            cwd = %summary.cwd.display(),
            ephemeral = summary.ephemeral,
            model = ?summary.model,
            has_title = summary.title.is_some(),
            "started session"
        );
        self.broadcast_event(ServerEvent::SessionStarted(SessionEventPayload {
            session: summary.clone(),
        }))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionStartResult { session: summary },
        })
        .expect("serialize session/start response")
    }

    async fn handle_session_list(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        if let Err(error) = serde_json::from_value::<SessionListParams>(params) {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InvalidParams,
                format!("invalid session/list params: {error}"),
            );
        }
        let sessions = self
            .sessions
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut summaries = Vec::with_capacity(sessions.len());
        for session in sessions {
            summaries.push(session.lock().await.summary.clone());
        }
        summaries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionListResult {
                sessions: summaries,
            },
        })
        .expect("serialize session/list response")
    }

    async fn handle_session_metadata_update(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionMetadataUpdateParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/metadata/update params: {error}"),
                );
            }
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let updated_session = {
            let mut session = session_arc.lock().await;
            session.summary.model = params.model.clone();
            session.summary.thinking = params.thinking.clone();
            let updated_at = Utc::now();
            session.summary.updated_at = updated_at;
            if let Some(record) = session.record.as_mut() {
                record.model = params.model;
                record.thinking = params.thinking;
                record.updated_at = updated_at;
                if let Err(error) = self.rollout_store.append_session_meta(record) {
                    return self.error_response(
                        request_id,
                        ProtocolErrorCode::InternalError,
                        format!("failed to persist session metadata update: {error}"),
                    );
                }
            }
            session.summary.clone()
        };

        // Persist updated session metadata to SQLite
        if !updated_session.ephemeral
            && let Err(err) = self.deps.db.upsert_session(&updated_session)
        {
            tracing::warn!(
                session_id = %params.session_id,
                error = %err,
                "failed to update session metadata in database"
            );
        }

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionMetadataUpdateResult {
                session: updated_session,
            },
        })
        .expect("serialize session/metadata/update response")
    }

    async fn handle_session_title_update(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionTitleUpdateParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/title/update params: {error}"),
                );
            }
        };
        let new_title = params.title.trim();
        if new_title.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InvalidParams,
                "session title cannot be empty",
            );
        }
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };

        let summary = {
            let mut session = session_arc.lock().await;
            let previous_title = session.summary.title.clone();
            let updated_at = Utc::now();
            session.summary.title = Some(new_title.to_string());
            session.summary.title_state =
                SessionTitleState::Final(SessionTitleFinalSource::UserRename);
            session.summary.updated_at = updated_at;
            if let Some(record) = session.record.as_mut() {
                record.title = Some(new_title.to_string());
                record.title_state = SessionTitleState::Final(SessionTitleFinalSource::UserRename);
                record.updated_at = updated_at;
                if let Err(error) = self.rollout_store.append_title_update(
                    record,
                    new_title.to_string(),
                    record.title_state.clone(),
                    previous_title,
                ) {
                    return self.error_response(
                        request_id,
                        ProtocolErrorCode::InternalError,
                        format!("failed to persist session title update: {error}"),
                    );
                }
            }
            session.summary.clone()
        };

        // Persist updated session metadata to SQLite
        if !summary.ephemeral
            && let Err(err) = self.deps.db.upsert_session(&summary)
        {
            tracing::warn!(
                session_id = %params.session_id,
                error = %err,
                "failed to update session title in database"
            );
        }

        self.broadcast_event(ServerEvent::SessionTitleUpdated(SessionEventPayload {
            session: summary.clone(),
        }))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionTitleUpdateResult { session: summary },
        })
        .expect("serialize session/title/update response")
    }

    async fn handle_session_resume(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionResumeParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/resume params: {error}"),
                );
            }
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let session = session_arc.lock().await;
        let session_summary = session.summary.clone();
        let latest_turn = session.latest_turn.clone();
        let loaded_item_count = session.loaded_item_count;
        let history_items = session.history_items.clone();
        let pending_texts: Vec<String> = session
            .pending_turn_queue
            .lock()
            .expect("pending turn queue mutex poisoned")
            .iter()
            .filter_map(|item| match &item.kind {
                devo_core::PendingInputKind::UserText { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        drop(session);
        self.subscribe_connection_to_session(connection_id, params.session_id, None)
            .await;
        tracing::info!(
            connection_id,
            session_id = %params.session_id,
            loaded_item_count,
            has_latest_turn = latest_turn.is_some(),
            pending_count = pending_texts.len(),
            "resumed session"
        );
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionResumeResult {
                session: session_summary,
                latest_turn,
                loaded_item_count,
                history_items,
                pending_texts,
            },
        })
        .expect("serialize session/resume response")
    }

    async fn handle_session_fork(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionForkParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/fork params: {error}"),
                );
            }
        };
        let Some(source_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let source = source_arc.lock().await;
        let source_core_session = source.core_session.lock().await;
        let now = Utc::now();
        let forked_id = SessionId::new();
        let fork_cwd = params.cwd.unwrap_or_else(|| source.summary.cwd.clone());
        let fork_model = source
            .summary
            .model
            .clone()
            .unwrap_or_else(|| self.deps.default_model.clone());
        let summary = crate::SessionMetadata {
            session_id: forked_id,
            cwd: fork_cwd.clone(),
            created_at: now,
            updated_at: now,
            title: params.title.or_else(|| source.summary.title.clone()),
            title_state: source.summary.title_state.clone(),
            ephemeral: source.summary.ephemeral,
            model: Some(fork_model.clone()),
            thinking: source.summary.thinking.clone(),
            reasoning_effort: source.summary.reasoning_effort,
            total_input_tokens: source_core_session.total_input_tokens,
            total_output_tokens: source_core_session.total_output_tokens,
            prompt_token_estimate: source_core_session.prompt_token_estimate,
            status: SessionRuntimeStatus::Idle,
        };
        let mut core_session = self.deps.new_session_state(forked_id, fork_cwd);
        core_session.messages = source_core_session.messages.clone();
        core_session.prompt_messages = source_core_session.prompt_messages.clone();
        core_session.session_context = source_core_session.session_context.clone();
        core_session.latest_turn_context = source_core_session.latest_turn_context.clone();
        core_session.turn_count = source_core_session.turn_count;
        core_session.total_input_tokens = source_core_session.total_input_tokens;
        core_session.total_output_tokens = source_core_session.total_output_tokens;
        core_session.total_cache_creation_tokens = source_core_session.total_cache_creation_tokens;
        core_session.total_cache_read_tokens = source_core_session.total_cache_read_tokens;
        core_session.last_input_tokens = source_core_session.last_input_tokens;
        let latest_turn = source.latest_turn.clone();
        let loaded_item_count = source.loaded_item_count;
        let history_items = source.history_items.clone();
        let latest_compaction_snapshot = source.latest_compaction_snapshot.clone();
        let persisted_turn_items = source.persisted_turn_items.clone();
        drop(source_core_session);
        drop(source);
        let pending_turn_queue = Arc::clone(&core_session.pending_turn_queue);
        let btw_input_queue = Arc::clone(&core_session.btw_input_queue);
        self.sessions.lock().await.insert(
            forked_id,
            RuntimeSession {
                record: None,
                summary: summary.clone(),
                core_session: Arc::new(Mutex::new(core_session)),
                active_turn: None,
                latest_turn,
                loaded_item_count,
                history_items,
                persisted_turn_items,
                latest_compaction_snapshot,
                pending_turn_queue,
                btw_input_queue,
                active_task: None,
                deferred_assistant: None,
                deferred_reasoning: None,
                next_item_seq: loaded_item_count + 1,
                first_user_input: None,
            }
            .shared(),
        );
        let sessions = self.sessions.lock().await;
        if let Some(forked_session) = sessions.get(&forked_id).cloned() {
            drop(sessions);
            let mut forked_session = forked_session.lock().await;
            if !forked_session.summary.ephemeral {
                let record = self.rollout_store.create_session_record(
                    forked_id,
                    now,
                    forked_session.summary.cwd.clone(),
                    forked_session.summary.title.clone(),
                    forked_session.summary.model.clone(),
                    forked_session.summary.thinking.clone(),
                    self.deps.provider.name().to_string(),
                    Some(params.session_id),
                );
                if let Err(error) = self.rollout_store.append_session_meta(&record) {
                    return self.error_response(
                        request_id,
                        ProtocolErrorCode::InternalError,
                        format!("failed to persist forked session metadata: {error}"),
                    );
                }
                forked_session.record = Some(record);
            }
        } else {
            drop(sessions);
        }
        self.subscribe_connection_to_session(connection_id, forked_id, None)
            .await;
        tracing::info!(
            connection_id,
            source_session_id = %params.session_id,
            forked_session_id = %forked_id,
            cwd = %summary.cwd.display(),
            ephemeral = summary.ephemeral,
            model = ?summary.model,
            "forked session"
        );
        self.broadcast_event(ServerEvent::SessionStarted(SessionEventPayload {
            session: summary.clone(),
        }))
        .await;
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionForkResult {
                session: summary,
                forked_from_session_id: params.session_id,
            },
        })
        .expect("serialize session/fork response")
    }

    async fn handle_session_compact(
        self: &Arc<Self>,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionCompactParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/compact params: {error}"),
                );
            }
        };

        let session_arc = match self.sessions.lock().await.get(&params.session_id).cloned() {
            Some(session) => session,
            None => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::SessionNotFound,
                    "session does not exist",
                );
            }
        };

        let summary = {
            let runtime_session = session_arc.lock().await;
            runtime_session.summary.clone()
        };

        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(panic) =
                AssertUnwindSafe(runtime.run_session_compaction(params.session_id, session_arc))
                    .catch_unwind()
                    .await
            {
                tracing::error!(
                    session_id = %params.session_id,
                    panic = ?panic,
                    "session compaction task panicked"
                );
            }
        });
        tracing::info!(session_id = %params.session_id, "accepted async session compaction request");

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionCompactResult { session: summary },
        })
        .expect("serialize session/compact response")
    }

    async fn run_session_compaction(
        self: Arc<Self>,
        session_id: SessionId,
        session_arc: Arc<tokio::sync::Mutex<crate::execution::RuntimeSession>>,
    ) {
        tracing::info!(session_id = %session_id, "session compaction task started");
        let started_summary = {
            let runtime_session = session_arc.lock().await;
            runtime_session.summary.clone()
        };
        self.broadcast_event(ServerEvent::SessionCompactionStarted(SessionEventPayload {
            session: started_summary,
        }))
        .await;

        let result = {
            let runtime_session = session_arc.lock().await;
            let core_session = runtime_session.core_session.lock().await;

            let items: Vec<ResponseItem> = core_session
                .messages
                .iter()
                .flat_map(|msg| message_to_response_items(msg.clone()))
                .collect();

            let token_info = TokenInfo {
                input_tokens: core_session.total_input_tokens,
                cached_input_tokens: core_session.total_cache_read_tokens,
                output_tokens: core_session.total_output_tokens,
            };

            let model_slug = runtime_session
                .summary
                .model
                .as_deref()
                .unwrap_or(&self.deps.default_model);
            let max_tokens = self
                .deps
                .model_catalog
                .get(model_slug)
                .and_then(|m| m.max_tokens.map(|t| t as usize))
                .unwrap_or(4096);

            let summarizer = DefaultHistorySummarizer::with_slug(
                self.deps.provider.clone(),
                model_slug,
                max_tokens,
            );
            tracing::debug!(
                session_id = %session_id,
                model = %model_slug,
                item_count = items.len(),
                input_tokens = token_info.input_tokens,
                cached_input_tokens = token_info.cached_input_tokens,
                output_tokens = token_info.output_tokens,
                "starting compaction summarization"
            );

            let config = CompactionConfig {
                budget: core_session.config.token_budget.clone(),
                kind: CompactionKind::Proactive,
            };

            compact_history(&items, &token_info, &summarizer, &config).await
        };

        match result {
            Ok(CompactAction::Replaced(compacted_items)) => {
                let mut runtime_session = session_arc.lock().await;
                let preserved_item_ids = Self::preserved_item_ids_from_compacted(
                    &runtime_session.persisted_turn_items,
                    &compacted_items,
                );
                let new_messages: Vec<Message> = compacted_items
                    .iter()
                    .filter_map(|item| match item {
                        ResponseItem::Message(msg) => Some(msg.clone()),
                        _ => None,
                    })
                    .collect();

                {
                    let mut core_session = runtime_session.core_session.lock().await;
                    core_session.set_prompt_messages(new_messages);
                    let compacted_total_input_tokens = core_session.total_input_tokens;
                    let compacted_total_output_tokens = core_session.total_output_tokens;
                    let compacted_prompt_token_estimate = core_session
                        .prompt_source_messages()
                        .iter()
                        .map(|message| serde_json::to_string(message).map_or(0, |json| json.len()))
                        .sum::<usize>()
                        .div_ceil(4);
                    core_session.prompt_token_estimate = compacted_prompt_token_estimate;
                    drop(core_session);
                    runtime_session.summary.total_input_tokens = compacted_total_input_tokens;
                    runtime_session.summary.total_output_tokens = compacted_total_output_tokens;
                    runtime_session.summary.prompt_token_estimate = compacted_prompt_token_estimate;
                }

                if let Some(turn_id) = runtime_session
                    .latest_turn
                    .as_ref()
                    .map(|t| t.turn_id)
                    .or_else(|| runtime_session.active_turn.as_ref().map(|t| t.turn_id))
                {
                    let item_id = devo_core::ItemId::new();
                    let item_seq = runtime_session.next_item_seq;
                    runtime_session.loaded_item_count += 1;
                    runtime_session.next_item_seq += 1;

                    let payload = serde_json::json!({ "title": "Context Compaction" });
                    self.broadcast_event(ServerEvent::ItemStarted(ItemEventPayload {
                        context: EventContext {
                            session_id,
                            turn_id: Some(turn_id),
                            item_id: Some(item_id),
                            seq: item_seq,
                        },
                        item: ItemEnvelope {
                            item_id,
                            item_kind: ItemKind::ContextCompaction,
                            payload: payload.clone(),
                        },
                    }))
                    .await;

                    self.broadcast_event(ServerEvent::ItemCompleted(ItemEventPayload {
                        context: EventContext {
                            session_id,
                            turn_id: Some(turn_id),
                            item_id: Some(item_id),
                            seq: item_seq,
                        },
                        item: ItemEnvelope {
                            item_id,
                            item_kind: ItemKind::ContextCompaction,
                            payload,
                        },
                    }))
                    .await;

                    if let Some(record) = runtime_session.record.clone() {
                        let summary_turn_item =
                            Self::summary_turn_item_from_compacted(&compacted_items);
                        runtime_session.latest_compaction_snapshot =
                            Some(devo_core::CompactionSnapshotLine {
                                timestamp: Utc::now(),
                                session_id,
                                turn_id,
                                summary_item_id: item_id,
                                preserved_item_ids: preserved_item_ids.clone(),
                            });
                        runtime_session.persisted_turn_items.push(
                            crate::execution::PersistedTurnItem {
                                item_id,
                                turn_item: summary_turn_item.clone(),
                            },
                        );

                        let item_record = crate::persistence::build_item_record(
                            session_id,
                            turn_id,
                            item_id,
                            item_seq,
                            summary_turn_item,
                            None,
                            None,
                        );
                        if let Err(error) = self.rollout_store.append_item(&record, item_record) {
                            tracing::warn!(
                                session_id = %session_id,
                                error = %error,
                                "failed to persist compaction summary item"
                            );
                        }
                        if let Err(error) = self.rollout_store.append_compaction_snapshot(
                            &record,
                            runtime_session
                                .latest_compaction_snapshot
                                .clone()
                                .expect("compaction snapshot should be set"),
                        ) {
                            tracing::warn!(
                                session_id = %session_id,
                                error = %error,
                                "failed to persist compaction snapshot"
                            );
                        }
                    }
                }

                let summary = runtime_session.summary.clone();
                tracing::info!(session_id = %session_id, "session compaction completed with replacement");
                self.broadcast_event(ServerEvent::SessionCompactionCompleted(
                    SessionEventPayload { session: summary },
                ))
                .await;
            }
            Ok(CompactAction::Skipped) => {
                let runtime_session = session_arc.lock().await;
                let summary = runtime_session.summary.clone();
                tracing::info!(session_id = %session_id, "session compaction completed without replacement");
                self.broadcast_event(ServerEvent::SessionCompactionCompleted(
                    SessionEventPayload { session: summary },
                ))
                .await;
            }
            Err(error) => {
                tracing::warn!(session_id = %session_id, error = %error, "session compaction failed");
                self.broadcast_event(ServerEvent::SessionCompactionFailed(
                    SessionCompactionFailedPayload {
                        session_id,
                        message: format!("compaction failed: {error}"),
                    },
                ))
                .await;
            }
        }
    }

    fn preserved_item_ids_from_compacted(
        persisted_turn_items: &[crate::execution::PersistedTurnItem],
        compacted_items: &[ResponseItem],
    ) -> Vec<ItemId> {
        let normalized_persisted_items = persisted_turn_items
            .iter()
            .filter_map(|item| {
                let response_item = match &item.turn_item {
                    TurnItem::UserMessage(TextItem { text })
                    | TurnItem::SteerInput(TextItem { text }) => {
                        ResponseItem::Message(Message::user(text.clone()))
                    }
                    TurnItem::AgentMessage(TextItem { text })
                    | TurnItem::Plan(TextItem { text })
                    | TurnItem::WebSearch(TextItem { text })
                    | TurnItem::ImageGeneration(TextItem { text })
                    | TurnItem::ContextCompaction(TextItem { text })
                    | TurnItem::HookPrompt(TextItem { text }) => {
                        ResponseItem::Message(Message::assistant_text(text.clone()))
                    }
                    TurnItem::Reasoning(TextItem { text }) => {
                        ResponseItem::Reason { text: text.clone() }
                    }
                    TurnItem::ToolCall(ToolCallItem {
                        tool_call_id,
                        tool_name,
                        input,
                    }) => ResponseItem::ToolCall {
                        id: tool_call_id.clone(),
                        name: tool_name.clone(),
                        input: input.clone(),
                    },
                    TurnItem::ToolResult(ToolResultItem {
                        tool_call_id,
                        output,
                        is_error,
                        ..
                    }) => ResponseItem::ToolCallOutput {
                        tool_use_id: tool_call_id.clone(),
                        content: match output {
                            serde_json::Value::String(text) => text.clone(),
                            other => other.to_string(),
                        },
                        is_error: *is_error,
                    },
                    TurnItem::ToolProgress(_)
                    | TurnItem::ApprovalRequest(_)
                    | TurnItem::ApprovalDecision(_)
                    | TurnItem::TurnSummary(_) => return None,
                };
                (!response_item.is_reason()).then_some((item.item_id, response_item))
            })
            .collect::<Vec<_>>();
        let preserved = compacted_items.get(1..).unwrap_or(&[]);
        if preserved.is_empty() {
            return Vec::new();
        }
        let preserved_len = preserved.len();
        if normalized_persisted_items.len() < preserved_len {
            return Vec::new();
        }
        let suffix =
            &normalized_persisted_items[normalized_persisted_items.len() - preserved_len..];
        if suffix.iter().map(|(_, item)| item).eq(preserved.iter()) {
            suffix.iter().map(|(item_id, _)| *item_id).collect()
        } else {
            Vec::new()
        }
    }

    fn summary_turn_item_from_compacted(compacted_items: &[ResponseItem]) -> TurnItem {
        let summary_text = compacted_items
            .first()
            .and_then(|item| match item {
                ResponseItem::Message(message) => {
                    message.content.iter().find_map(|block| match block {
                        devo_core::ContentBlock::Text { text } => Some(text.clone()),
                        devo_core::ContentBlock::Reasoning { .. }
                        | devo_core::ContentBlock::ToolUse { .. }
                        | devo_core::ContentBlock::ToolResult { .. } => None,
                    })
                }
                ResponseItem::Reason { text } => Some(text.clone()),
                ResponseItem::ToolCall { .. } | ResponseItem::ToolCallOutput { .. } => None,
            })
            .unwrap_or_default();
        TurnItem::ContextCompaction(TextItem { text: summary_text })
    }

    async fn handle_turn_start(
        self: &Arc<Self>,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: TurnStartParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid turn/start params: {error}"),
                );
            }
        };
        if params.input.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        }
        let Some(display_input) = render_input_items(&params.input) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let workspace_root = {
            let session = session_arc.lock().await;
            params
                .cwd
                .clone()
                .unwrap_or_else(|| session.summary.cwd.clone())
        };
        let Some(input_text) = (match self
            .deps
            .resolve_input_items(&params.input, Some(workspace_root.as_path()))
        {
            Ok(input_text) => input_text,
            Err(error) => {
                let code = match error {
                    devo_core::SkillError::SkillNotFound { .. }
                    | devo_core::SkillError::SkillDisabled { .. } => {
                        ProtocolErrorCode::InvalidParams
                    }
                    devo_core::SkillError::SkillParseFailed { .. }
                    | devo_core::SkillError::SkillRootUnavailable { .. }
                    | devo_core::SkillError::DuplicateSkillId { .. } => {
                        ProtocolErrorCode::InternalError
                    }
                };
                return self.error_response(
                    request_id,
                    code,
                    format!("failed to resolve turn input: {error}"),
                );
            }
        }) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        };

        let now = Utc::now();
        let turn = {
            let mut session = session_arc.lock().await;
            if let Some(active_turn) = session.active_turn.as_ref() {
                // Queue instead of rejecting — push directly to the pending_turn_queue
                // (independent lock) so we don't block on core_session which is
                // held by the running query() loop.
                let pending_turn_queue = Arc::clone(&session.pending_turn_queue);
                let active_turn_id = active_turn.turn_id;
                let is_ephemeral = session.summary.ephemeral;
                drop(session);

                {
                    let mut guard = pending_turn_queue
                        .lock()
                        .expect("pending turn queue mutex should not be poisoned");
                    let item = devo_core::PendingInputItem {
                        kind: devo_core::PendingInputKind::UserText {
                            text: input_text.clone(),
                        },
                        metadata: None,
                        created_at: chrono::Utc::now(),
                    };
                    guard.push_back(item.clone());

                    // Persist to SQLite (skip for ephemeral sessions)
                    if !is_ephemeral
                        && let Err(err) =
                            self.deps
                                .db
                                .push_pending(&params.session_id, QueueType::Turn, &item)
                    {
                        tracing::warn!(
                            session_id = %params.session_id,
                            error = %err,
                            "failed to persist pending turn message to database"
                        );
                    }
                }
                // Send response via the high-priority channel so it bypasses
                // any backlog of event notifications on the shared channel.
                // Broadcast the queue update asynchronously in the background.
                if let Some(tx) = self.high_pri_tx.lock().await.as_ref() {
                    let response = serde_json::to_value(SuccessResponse {
                        id: request_id,
                        result: TurnStartResult {
                            turn_id: active_turn_id,
                            status: TurnStatus::Running,
                            accepted_at: now,
                        },
                    })
                    .expect("serialize turn/start response");
                    let _ = tx.send(response);
                }
                let sid = params.session_id;
                let runtime = Arc::clone(self);
                tokio::spawn(async move {
                    runtime.broadcast_updated_queue(sid).await;
                });
                // Signal to handle_incoming that the response was already sent
                // via the high-priority channel, so it should not send a duplicate.
                return serde_json::Value::Null;
            }
            if let Some(cwd) = params.cwd.clone() {
                session.summary.cwd = cwd.clone();
                session.core_session.lock().await.cwd = cwd;
            }
            let requested_model = params.model.as_deref().or(session.summary.model.as_deref());
            let requested_thinking = params
                .thinking
                .clone()
                .or_else(|| session.summary.thinking.clone());
            let turn_config = self
                .deps
                .resolve_turn_config(requested_model, requested_thinking.clone());
            let resolved_request = turn_config
                .model
                .resolve_thinking_selection(turn_config.thinking_selection.as_deref());
            session.summary.model = Some(turn_config.model.slug.clone());
            session.summary.thinking = turn_config.thinking_selection.clone();
            let turn = TurnMetadata {
                turn_id: TurnId::new(),
                session_id: params.session_id,
                sequence: session
                    .latest_turn
                    .as_ref()
                    .map_or(1, |turn| turn.sequence + 1),
                status: TurnStatus::Running,
                kind: devo_core::TurnKind::Regular,
                model: turn_config.model.slug.clone(),
                thinking: turn_config.thinking_selection.clone(),
                reasoning_effort: resolved_request.effective_reasoning_effort,
                request_model: resolved_request.request_model,
                request_thinking: resolved_request.request_thinking,
                started_at: now,
                completed_at: None,
                usage: None,
            };
            session.summary.status = SessionRuntimeStatus::ActiveTurn;
            session.summary.updated_at = now;
            session.active_turn = Some(turn.clone());
            let clear_session_id = params.session_id;
            let runtime_for_broadcast = Arc::clone(self);
            tokio::spawn(async move {
                runtime_for_broadcast
                    .broadcast_event(ServerEvent::InputQueueUpdated(
                        devo_core::InputQueueUpdatedPayload {
                            session_id: clear_session_id,
                            pending_count: 0,
                            pending_texts: vec![],
                        },
                    ))
                    .await;
            });
            let runtime = Arc::clone(self);
            let turn_for_task = turn.clone();
            let display_input_for_task = display_input.clone();
            let input_for_task = input_text.clone();
            let turn_config_for_task = turn_config.clone();
            let task = tokio::spawn(async move {
                runtime
                    .execute_turn(
                        params.session_id,
                        turn_for_task,
                        turn_config_for_task,
                        display_input_for_task,
                        input_for_task,
                    )
                    .await;
            });
            self.active_tasks
                .lock()
                .await
                .insert(params.session_id, task.abort_handle());
            turn
        };
        self.maybe_assign_provisional_title(params.session_id, &display_input)
            .await;
        // Capture first user input for title generation and trigger LLM title.
        // On subsequent turns, retry if previous attempts failed.
        {
            let mut session = session_arc.lock().await;
            if session.first_user_input.is_none() {
                session.first_user_input = Some(display_input.clone());
            }
        }
        let needs_title = {
            let session = session_arc.lock().await;
            let first_input = session.first_user_input.clone();
            let needs = matches!(
                session.summary.title_state,
                SessionTitleState::Unset | SessionTitleState::Provisional
            );
            (needs, first_input)
        };
        if needs_title.0
            && let Some(first_input) = needs_title.1
        {
            let runtime = Arc::clone(self);
            let sid = params.session_id;
            tokio::spawn(async move {
                runtime.maybe_generate_final_title(sid, first_input).await;
            });
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
                build_turn_record(&turn, session_context, turn_context),
            )
        {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to persist turn start: {error}"),
            );
        }

        tracing::info!(
            session_id = %params.session_id,
            turn_id = %turn.turn_id,
            sequence = turn.sequence,
            request_model = %turn.request_model,
            input_chars = input_text.len(),
            "started turn"
        );
        self.broadcast_event(ServerEvent::SessionStatusChanged(
            SessionStatusChangedPayload {
                session_id: params.session_id,
                status: SessionRuntimeStatus::ActiveTurn,
            },
        ))
        .await;
        self.broadcast_event(ServerEvent::TurnStarted(TurnEventPayload {
            session_id: params.session_id,
            turn: turn.clone(),
        }))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnStartResult {
                turn_id: turn.turn_id,
                status: turn.status.clone(),
                accepted_at: now,
            },
        })
        .expect("serialize turn/start response")
    }

    async fn handle_turn_interrupt(
        self: &Arc<Self>,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: TurnInterruptParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid turn/interrupt params: {error}"),
                );
            }
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };

        // Complete any deferred (in-progress) items before aborting the turn task.
        // Take the items out while holding the session lock, then drop the lock
        // before calling complete_item to avoid re-locking session_arc inside
        // persist_item, which would deadlock on the non-reentrant tokio Mutex.
        let deferred_assistant = {
            let mut session = session_arc.lock().await;
            session.deferred_assistant.take()
        };
        let deferred_reasoning = {
            let mut session = session_arc.lock().await;
            session.deferred_reasoning.take()
        };
        if let Some((item_id, item_seq, text)) = deferred_assistant {
            self.complete_item(
                params.session_id,
                params.turn_id,
                item_id,
                item_seq,
                ItemKind::AgentMessage,
                TurnItem::AgentMessage(TextItem { text: text.clone() }),
                serde_json::json!({ "title": "Assistant", "text": text }),
            )
            .await;
        }
        if let Some((item_id, item_seq, text)) = deferred_reasoning {
            self.complete_item(
                params.session_id,
                params.turn_id,
                item_id,
                item_seq,
                ItemKind::Reasoning,
                TurnItem::Reasoning(TextItem { text: text.clone() }),
                serde_json::json!({ "title": "Reasoning", "text": text }),
            )
            .await;
        }

        if let Some(task) = self.active_tasks.lock().await.remove(&params.session_id) {
            task.abort();
        }
        let interrupted_turn = {
            let mut session = session_arc.lock().await;
            let Some(mut turn) = session.active_turn.take() else {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::TurnNotFound,
                    "turn is not active",
                );
            };
            if turn.turn_id != params.turn_id {
                session.active_turn = Some(turn);
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::TurnNotFound,
                    "turn does not exist",
                );
            }
            turn.status = TurnStatus::Interrupted;
            turn.completed_at = Some(Utc::now());
            session.latest_turn = Some(turn.clone());
            session.summary.status = SessionRuntimeStatus::Idle;
            session.summary.updated_at = Utc::now();
            let totals = session.core_session.try_lock().ok().map(|core_session| {
                (
                    core_session.total_input_tokens,
                    core_session.total_output_tokens,
                )
            });
            if let Some((total_input_tokens, total_output_tokens)) = totals {
                session.summary.total_input_tokens = total_input_tokens;
                session.summary.total_output_tokens = total_output_tokens;
            }
            turn
        };
        let (record, session_context, turn_context) = {
            let session = session_arc.lock().await;
            let core_session_lock = session.core_session.try_lock();
            if let Ok(core_session) = core_session_lock {
                (
                    session.record.clone(),
                    core_session.session_context.clone(),
                    core_session.latest_turn_context.clone(),
                )
            } else {
                // core_session is held by the running turn task — skip
                // session context in the rollout record for this interrupt.
                (session.record.clone(), None, None)
            }
        };
        if let Some(record) = record
            && let Err(error) = self.rollout_store.append_turn(
                &record,
                build_turn_record(&interrupted_turn, session_context, turn_context),
            )
        {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to persist interrupted turn: {error}"),
            );
        }

        tracing::info!(
            session_id = %params.session_id,
            turn_id = %interrupted_turn.turn_id,
            status = ?interrupted_turn.status,
            "interrupted turn"
        );
        self.broadcast_event(ServerEvent::TurnInterrupted(TurnEventPayload {
            session_id: params.session_id,
            turn: interrupted_turn.clone(),
        }))
        .await;
        self.broadcast_event(ServerEvent::TurnCompleted(TurnEventPayload {
            session_id: params.session_id,
            turn: interrupted_turn.clone(),
        }))
        .await;
        self.broadcast_event(ServerEvent::SessionStatusChanged(
            SessionStatusChangedPayload {
                session_id: params.session_id,
                status: SessionRuntimeStatus::Idle,
            },
        ))
        .await;

        // After interrupting the current turn, drain the steering queue and
        // start the next turn. Without this, queued inputs would be orphaned
        // because the aborted execute_turn task never reaches its end-of-turn
        // drain logic.
        let runtime = Arc::clone(self);
        let sid = params.session_id;
        tokio::spawn(async move {
            runtime.spawn_next_turn_from_queue(sid).await;
        });

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnInterruptResult {
                turn_id: interrupted_turn.turn_id,
                status: interrupted_turn.status,
            },
        })
        .expect("serialize turn/interrupt response")
    }

    async fn handle_turn_steer(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: TurnSteerParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid turn/steer params: {error}"),
                );
            }
        };
        if params.input.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn steer input is empty",
            );
        }
        let Some(display_input) = render_input_items(&params.input) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn steer input is empty",
            );
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let (turn_id, workspace_root, btw_input_queue) = {
            let session = session_arc.lock().await;
            let Some(turn_id) = session.active_turn.as_ref().map(|turn| turn.turn_id) else {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::NoActiveTurn,
                    "no active turn exists",
                );
            };
            if turn_id != params.expected_turn_id {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::ExpectedTurnMismatch,
                    "active turn did not match expectedTurnId",
                );
            }
            let active_turn = session.active_turn.as_ref().expect("active turn exists");
            if active_turn.kind != devo_core::TurnKind::Regular {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::ActiveTurnNotSteerable,
                    "cannot steer a non-regular turn",
                );
            }
            (
                turn_id,
                session.summary.cwd.clone(),
                Arc::clone(&session.btw_input_queue),
            )
        };
        let prompt_text = match self
            .deps
            .resolve_input_items(&params.input, Some(workspace_root.as_path()))
        {
            Ok(Some(input_text)) => input_text,
            Ok(None) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::EmptyInput,
                    "turn steer input is empty",
                );
            }
            Err(error) => {
                let code = match error {
                    devo_core::SkillError::SkillNotFound { .. }
                    | devo_core::SkillError::SkillDisabled { .. } => {
                        ProtocolErrorCode::InvalidParams
                    }
                    devo_core::SkillError::SkillParseFailed { .. }
                    | devo_core::SkillError::SkillRootUnavailable { .. }
                    | devo_core::SkillError::DuplicateSkillId { .. } => {
                        ProtocolErrorCode::InternalError
                    }
                };
                return self.error_response(
                    request_id,
                    code,
                    format!("failed to resolve turn steer input: {error}"),
                );
            }
        };

        self.emit_turn_item(
            params.session_id,
            turn_id,
            ItemKind::UserMessage,
            TurnItem::SteerInput(TextItem {
                text: display_input.clone(),
            }),
            serde_json::json!({ "title": "You", "text": display_input }),
        )
        .await;
        let item = devo_core::PendingInputItem {
            kind: devo_core::PendingInputKind::UserText { text: prompt_text },
            metadata: None,
            created_at: chrono::Utc::now(),
        };
        btw_input_queue
            .lock()
            .expect("btw input queue mutex should not be poisoned")
            .push_back(item.clone());

        // Persist to SQLite (skip for ephemeral sessions)
        {
            let session = session_arc.lock().await;
            if !session.summary.ephemeral
                && let Err(err) =
                    self.deps
                        .db
                        .push_pending(&params.session_id, QueueType::Btw, &item)
            {
                tracing::warn!(
                    session_id = %params.session_id,
                    error = %err,
                    "failed to persist btw input to database"
                );
            }
        }

        self.emit_to_connection(
            connection_id,
            "serverRequest/resolved",
            ServerEvent::ServerRequestResolved(ServerRequestResolvedPayload {
                session_id: params.session_id,
                request_id: "steer-accepted".into(),
                turn_id: Some(turn_id),
            }),
        )
        .await;
        tracing::info!(
            connection_id,
            session_id = %params.session_id,
            turn_id = %turn_id,
            input_items = params.input.len(),
            "accepted turn steer request"
        );
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnSteerResult { turn_id },
        })
        .expect("serialize turn/steer response")
    }

    async fn handle_events_subscribe(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: EventsSubscribeParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid events/subscribe params: {error}"),
                );
            }
        };
        if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
            connection.subscriptions.push(SubscriptionFilter {
                session_id: params.session_id,
                event_types: params.event_types.unwrap_or_default().into_iter().collect(),
            });
        }
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: EventsSubscribeResult {
                subscription_id: format!("sub-{connection_id}-1").into(),
            },
        })
        .expect("serialize events/subscribe response")
    }

    async fn execute_turn(
        self: Arc<Self>,
        session_id: SessionId,
        turn: TurnMetadata,
        turn_config: TurnConfig,
        display_input: String,
        input: String,
    ) {
        self.emit_text_item(
            session_id,
            turn.turn_id,
            ItemKind::UserMessage,
            TurnItem::UserMessage(TextItem {
                text: display_input.clone(),
            }),
            "You",
            display_input.clone(),
        )
        .await;

        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<QueryEvent>();
        let runtime = Arc::clone(&self);
        let turn_for_events = turn.clone();
        let event_session_arc = Arc::clone(&session_arc);
        let event_task = tokio::spawn(async move {
            let mut assistant_item_id = None;
            let mut assistant_item_seq = None;
            let mut assistant_text = String::new();
            let mut reasoning_item_id = None;
            let mut reasoning_item_seq = None;
            let mut reasoning_text = String::new();
            let mut tool_names_by_id = HashMap::new();
            let mut pending_tool_calls: HashMap<String, (ItemId, u64, serde_json::Value)> =
                HashMap::new();
            let mut latest_usage: Option<TurnUsage> = None;
            let mut usage_base: Option<(usize, usize)> = None;
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
                    QueryEvent::ToolUseStart { id, name, input } => {
                        tool_names_by_id.insert(id.clone(), name.clone());
                        if let (Some(item_id), Some(item_seq)) =
                            (assistant_item_id.take(), assistant_item_seq.take())
                        {
                            runtime
                                .complete_item(
                                    session_id,
                                    turn_for_events.turn_id,
                                    item_id,
                                    item_seq,
                                    ItemKind::AgentMessage,
                                    TurnItem::AgentMessage(TextItem {
                                        text: assistant_text.clone(),
                                    }),
                                    serde_json::json!({
                                        "title": "Assistant",
                                        "text": assistant_text,
                                    }),
                                )
                                .await;
                            assistant_text.clear();
                        }
                        if let (Some(item_id), Some(item_seq)) =
                            (reasoning_item_id.take(), reasoning_item_seq.take())
                        {
                            runtime
                                .complete_item(
                                    session_id,
                                    turn_for_events.turn_id,
                                    item_id,
                                    item_seq,
                                    ItemKind::Reasoning,
                                    TurnItem::Reasoning(TextItem {
                                        text: reasoning_text.clone(),
                                    }),
                                    serde_json::json!({
                                        "title": "Reasoning",
                                        "text": reasoning_text,
                                    }),
                                )
                                .await;
                            reasoning_text.clear();
                        }
                        let (item_id, item_seq) = runtime
                            .start_item(
                                session_id,
                                turn_for_events.turn_id,
                                ItemKind::ToolCall,
                                serde_json::to_value(ToolCallPayload {
                                    tool_call_id: id.clone(),
                                    tool_name: name.clone(),
                                    parameters: input.clone(),
                                })
                                .expect("serialize tool call payload"),
                            )
                            .await;
                        pending_tool_calls.insert(id, (item_id, item_seq, input));
                    }
                    QueryEvent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        summary,
                    } => {
                        let tool_name = tool_names_by_id.get(&tool_use_id).cloned();
                        // First complete the pending ToolCall item so its item/completed
                        // arrives before the ToolResult item/completed.
                        if let Some((item_id, item_seq, tool_input)) =
                            pending_tool_calls.remove(&tool_use_id)
                        {
                            let completed_payload = serde_json::to_value(ToolCallPayload {
                                tool_call_id: tool_use_id.clone(),
                                tool_name: tool_name.clone().unwrap_or_default(),
                                parameters: tool_input.clone(),
                            })
                            .expect("serialize tool call payload");
                            runtime
                                .complete_item(
                                    session_id,
                                    turn_for_events.turn_id,
                                    item_id,
                                    item_seq,
                                    ItemKind::ToolCall,
                                    TurnItem::ToolCall(ToolCallItem {
                                        tool_call_id: tool_use_id.clone(),
                                        tool_name: tool_name.clone().unwrap_or_default(),
                                        input: tool_input,
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
                                    output: serde_json::Value::String(content.clone()),
                                    is_error,
                                }),
                                serde_json::to_value(ToolResultPayload {
                                    tool_call_id: tool_use_id.clone(),
                                    tool_name,
                                    content: serde_json::Value::String(content),
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
                        let _ = runtime
                            .broadcast_event(ServerEvent::ItemDelta {
                                delta_kind: ItemDeltaKind::CommandExecutionOutputDelta,
                                payload: ItemDeltaPayload {
                                    context: EventContext {
                                        session_id,
                                        turn_id: Some(turn_for_events.turn_id),
                                        item_id: None,
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
                    }
                    | QueryEvent::Usage {
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
                                },
                            ))
                            .await;
                    }
                    QueryEvent::TurnComplete { .. } => {}
                }
            }
            if let (Some(item_id), Some(item_seq)) = (assistant_item_id, assistant_item_seq) {
                runtime
                    .complete_item(
                        session_id,
                        turn_for_events.turn_id,
                        item_id,
                        item_seq,
                        ItemKind::AgentMessage,
                        TurnItem::AgentMessage(TextItem {
                            text: assistant_text.clone(),
                        }),
                        serde_json::json!({ "title": "Assistant", "text": assistant_text }),
                    )
                    .await;
            }
            if let (Some(item_id), Some(item_seq)) = (reasoning_item_id, reasoning_item_seq) {
                runtime
                    .complete_item(
                        session_id,
                        turn_for_events.turn_id,
                        item_id,
                        item_seq,
                        ItemKind::Reasoning,
                        TurnItem::Reasoning(TextItem {
                            text: reasoning_text.clone(),
                        }),
                        serde_json::json!({ "title": "Reasoning", "text": reasoning_text }),
                    )
                    .await;
            }
            latest_usage
        });

        let (
            result,
            session_total_input_tokens,
            session_total_output_tokens,
            session_prompt_token_estimate,
        ) = {
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
            let runtime = ToolRuntime::new_without_permissions(Arc::clone(&registry));
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
                core_session.prompt_token_estimate,
            )
        };
        drop(event_tx);
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
            session.active_task = None;
            session.summary.status = SessionRuntimeStatus::Idle;
            session.summary.updated_at = Utc::now();
            session.summary.total_input_tokens = session_total_input_tokens;
            session.summary.total_output_tokens = session_total_output_tokens;
            session.summary.prompt_token_estimate = session_prompt_token_estimate;

            // Persist token stats to SQLite (skip for ephemeral sessions)
            if !session.summary.ephemeral {
                let stats = SessionStats {
                    total_input_tokens: session_total_input_tokens,
                    total_output_tokens: session_total_output_tokens,
                    total_cache_creation_tokens: 0,
                    total_cache_read_tokens: 0,
                    last_input_tokens: final_turn
                        .usage
                        .as_ref()
                        .map(|u| u.input_tokens as usize)
                        .unwrap_or(0),
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

        // Clear btw input queue (both in-memory and SQLite)
        {
            let session = session_arc.lock().await;
            session
                .btw_input_queue
                .lock()
                .expect("btw input queue mutex should not be poisoned")
                .clear();
            if !session.summary.ephemeral
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
        if let Err(error) = result {
            tracing::warn!(
                session_id = %session_id,
                turn_id = %final_turn.turn_id,
                status = ?final_turn.status,
                error = %error,
                "turn execution failed"
            );
            self.emit_text_item(
                session_id,
                final_turn.turn_id,
                ItemKind::AgentMessage,
                TurnItem::AgentMessage(TextItem {
                    text: error.to_string(),
                }),
                "Error",
                error.to_string(),
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
        // Broadcast remaining queue state so the TUI preview stays in sync.
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
        // Chain the next turn directly (not spawned) so it runs after the
        // current turn completes, and calls back into this drain logic.
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
    async fn spawn_next_turn_from_queue(self: &Arc<Self>, session_id: SessionId) {
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
    async fn broadcast_updated_queue(&self, session_id: SessionId) {
        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };
        let (pending_count, pending_texts) = {
            let session = session_arc.lock().await;
            let queue = session
                .pending_turn_queue
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

    async fn maybe_assign_provisional_title(&self, session_id: SessionId, first_user_input: &str) {
        let Some(candidate) = derive_provisional_title(first_user_input) else {
            return;
        };
        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };

        let updated_summary = {
            let mut session = session_arc.lock().await;
            if session.summary.title.is_some()
                || !matches!(session.summary.title_state, SessionTitleState::Unset)
            {
                return;
            }

            let previous_title = session.summary.title.clone();
            let updated_at = Utc::now();
            session.summary.title = Some(candidate.clone());
            session.summary.title_state = SessionTitleState::Provisional;
            session.summary.updated_at = updated_at;

            if let Some(record) = session.record.as_mut() {
                record.title = Some(candidate.clone());
                record.title_state = SessionTitleState::Provisional;
                record.updated_at = updated_at;
                if let Err(error) = self.rollout_store.append_title_update(
                    record,
                    candidate.clone(),
                    SessionTitleState::Provisional,
                    previous_title,
                ) {
                    tracing::warn!(session_id = %session_id, error = %error, "failed to persist provisional title");
                }
            }
            session.summary.clone()
        };

        self.broadcast_event(ServerEvent::SessionTitleUpdated(SessionEventPayload {
            session: updated_summary,
        }))
        .await;
    }

    /// Attempts to generate a final session title by calling the LLM.
    /// Retries up to MAX_TITLE_RETRIES times with exponential backoff.
    /// Exhausting retries leaves the title at `Provisional`; the caller
    /// should re-trigger on the next user message.
    const MAX_TITLE_RETRIES: usize = 5;
    const TITLE_RETRY_BASE_DELAY_SECS: u64 = 1;

    async fn maybe_generate_final_title(
        self: Arc<Self>,
        session_id: SessionId,
        first_user_input: String,
    ) {
        for attempt in 1..=Self::MAX_TITLE_RETRIES {
            let (model, should_skip) = {
                let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
                    return;
                };
                let session = session_arc.lock().await;
                (
                    session
                        .summary
                        .model
                        .clone()
                        .unwrap_or_else(|| self.deps.default_model.clone()),
                    matches!(session.summary.title_state, SessionTitleState::Final(_)),
                )
            };

            if should_skip {
                return;
            }

            let response = match self
                .deps
                .provider
                .completion(build_title_generation_request(model, &first_user_input))
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(session_id = %session_id, attempt, error = %error, "title gen failed");
                    if attempt < Self::MAX_TITLE_RETRIES {
                        let delay = Self::TITLE_RETRY_BASE_DELAY_SECS * (1u64 << (attempt - 1));
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    }
                    continue;
                }
            };

            let Some(generated_title) = normalize_generated_title(&response.content) else {
                tracing::warn!(session_id = %session_id, attempt, "title gen returned no valid title");
                if attempt < Self::MAX_TITLE_RETRIES {
                    let delay = Self::TITLE_RETRY_BASE_DELAY_SECS * (1u64 << (attempt - 1));
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                }
                continue;
            };

            let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
                return;
            };
            let updated_summary = {
                let mut session = session_arc.lock().await;
                if matches!(session.summary.title_state, SessionTitleState::Final(_)) {
                    return;
                }

                let previous_title = session.summary.title.clone();
                let updated_at = Utc::now();
                session.summary.title = Some(generated_title.clone());
                session.summary.title_state =
                    SessionTitleState::Final(SessionTitleFinalSource::ModelGenerated);
                session.summary.updated_at = updated_at;

                if let Some(record) = session.record.as_mut() {
                    record.title = Some(generated_title.clone());
                    record.title_state =
                        SessionTitleState::Final(SessionTitleFinalSource::ModelGenerated);
                    record.updated_at = updated_at;
                    if let Err(error) = self.rollout_store.append_title_update(
                        record,
                        generated_title.clone(),
                        record.title_state.clone(),
                        previous_title,
                    ) {
                        tracing::warn!(session_id = %session_id, error = %error, "failed to persist title");
                    }
                }
                session.summary.clone()
            };

            self.broadcast_event(ServerEvent::SessionTitleUpdated(SessionEventPayload {
                session: updated_summary,
            }))
            .await;
            return;
        }
        tracing::warn!(session_id = %session_id, "title generation exhausted all retries");
    }

    async fn emit_text_item(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_kind: ItemKind,
        turn_item: TurnItem,
        title: impl Into<String>,
        text: String,
    ) {
        self.emit_turn_item(
            session_id,
            turn_id,
            item_kind,
            turn_item,
            serde_json::json!({ "title": title.into(), "text": text }),
        )
        .await;
    }

    async fn emit_turn_item(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_kind: ItemKind,
        turn_item: TurnItem,
        payload: serde_json::Value,
    ) {
        let (item_id, item_seq) = self
            .start_item(session_id, turn_id, item_kind.clone(), payload.clone())
            .await;
        self.complete_item(
            session_id,
            turn_id,
            item_id,
            item_seq,
            item_kind.clone(),
            turn_item,
            payload.clone(),
        )
        .await;
    }

    async fn start_item(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_kind: ItemKind,
        payload: serde_json::Value,
    ) -> (ItemId, u64) {
        let item_id = ItemId::new();
        let item_seq = self.allocate_item_sequence(session_id).await;
        self.emit_item_started(session_id, turn_id, item_id, item_kind, payload)
            .await;
        (item_id, item_seq)
    }

    async fn emit_item_started(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_id: ItemId,
        item_kind: ItemKind,
        payload: serde_json::Value,
    ) {
        self.broadcast_event(ServerEvent::ItemStarted(ItemEventPayload {
            context: EventContext {
                session_id,
                turn_id: Some(turn_id),
                item_id: Some(item_id),
                seq: 0,
            },
            item: ItemEnvelope {
                item_id,
                item_kind,
                payload,
            },
        }))
        .await;
    }

    async fn emit_item_completed(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_id: ItemId,
        item_kind: ItemKind,
        payload: serde_json::Value,
    ) {
        self.broadcast_event(ServerEvent::ItemCompleted(ItemEventPayload {
            context: EventContext {
                session_id,
                turn_id: Some(turn_id),
                item_id: Some(item_id),
                seq: 0,
            },
            item: ItemEnvelope {
                item_id,
                item_kind,
                payload,
            },
        }))
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn complete_item(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_id: ItemId,
        item_seq: u64,
        item_kind: ItemKind,
        turn_item: TurnItem,
        payload: serde_json::Value,
    ) {
        self.persist_item(
            session_id,
            turn_id,
            item_id,
            item_seq,
            turn_item,
            Some(TurnStatus::Running),
            None,
        )
        .await;
        self.emit_item_completed(session_id, turn_id, item_id, item_kind, payload)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_item(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        item_id: ItemId,
        item_seq: u64,
        turn_item: TurnItem,
        turn_status: Option<TurnStatus>,
        worklog: Option<Worklog>,
    ) {
        if let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() {
            let record = {
                let mut session = session_arc.lock().await;
                if let Some(history_item) = history_item_from_turn_item(&turn_item) {
                    session.history_items.push(history_item);
                }
                session
                    .persisted_turn_items
                    .push(crate::execution::PersistedTurnItem {
                        item_id,
                        turn_item: turn_item.clone(),
                    });
                session.record.clone()
            };
            if let Some(record) = record {
                let item = build_item_record(
                    session_id,
                    turn_id,
                    item_id,
                    item_seq,
                    turn_item,
                    turn_status,
                    worklog,
                );
                if let Err(error) = self.rollout_store.append_item(&record, item) {
                    tracing::warn!(session_id = %session_id, error = %error, "failed to persist item line");
                }
            }
        }
    }

    async fn allocate_item_sequence(&self, session_id: SessionId) -> u64 {
        if let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() {
            let mut session = session_arc.lock().await;
            let item_seq = session.next_item_seq;
            session.loaded_item_count += 1;
            session.next_item_seq += 1;
            return item_seq;
        }
        1
    }

    async fn subscribe_connection_to_session(
        &self,
        connection_id: u64,
        session_id: SessionId,
        event_types: Option<HashSet<String>>,
    ) {
        if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
            let desired = event_types.unwrap_or_default();
            let already = connection.subscriptions.iter().any(|subscription| {
                subscription.session_id == Some(session_id) && subscription.event_types == desired
            });
            if already {
                return;
            }
            connection.subscriptions.push(SubscriptionFilter {
                session_id: Some(session_id),
                event_types: desired,
            });
        }
    }

    async fn connection_ready(&self, connection_id: u64) -> bool {
        self.connections
            .lock()
            .await
            .get(&connection_id)
            .is_some_and(|connection| connection.state == ConnectionState::Ready)
    }

    async fn emit_to_connection(&self, connection_id: u64, method: &str, event: ServerEvent) {
        let session_id = event.session_id();
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get_mut(&connection_id) {
            if !connection.should_deliver(method, session_id) {
                return;
            }
            let value = serde_json::to_value(NotificationEnvelope {
                method: method.to_string(),
                params: event.with_seq(connection.next_seq()),
            })
            .expect("serialize notification");
            let _ = connection.sender.send(value);
        }
    }

    async fn broadcast_event(&self, event: ServerEvent) {
        let method = event.method_name();
        let session_id = event.session_id();
        let mut connections = self.connections.lock().await;
        for connection in connections.values_mut() {
            if !connection.should_deliver(method, session_id) {
                continue;
            }
            let value = serde_json::to_value(NotificationEnvelope {
                method: method.to_string(),
                params: event.clone().with_seq(connection.next_seq()),
            })
            .expect("serialize notification");
            let _ = connection.sender.send(value);
        }
    }

    fn error_response(
        &self,
        request_id: serde_json::Value,
        code: ProtocolErrorCode,
        message: impl Into<String>,
    ) -> serde_json::Value {
        let message = message.into();
        tracing::warn!(
            request_id = %request_id,
            code = ?code,
            error_message = %message,
            "returning protocol error"
        );
        serde_json::to_value(ErrorResponse {
            id: request_id,
            error: ProtocolError {
                code,
                message,
                data: serde_json::json!({}),
            },
        })
        .expect("serialize error response")
    }
}

struct ConnectionRuntime {
    transport: ClientTransportKind,
    state: ConnectionState,
    sender: mpsc::UnboundedSender<serde_json::Value>,
    opt_out_notification_methods: HashSet<String>,
    subscriptions: Vec<SubscriptionFilter>,
    next_event_seq: u64,
}

impl ConnectionRuntime {
    fn should_deliver(&self, method: &str, session_id: Option<SessionId>) -> bool {
        if self.opt_out_notification_methods.contains(method) {
            return false;
        }
        if self.transport == ClientTransportKind::Stdio {
            return true;
        }
        if self.subscriptions.is_empty() {
            return false;
        }
        self.subscriptions.iter().any(|subscription| {
            let session_matches = subscription
                .session_id
                .is_none_or(|expected| session_id == Some(expected));
            let event_matches =
                subscription.event_types.is_empty() || subscription.event_types.contains(method);
            session_matches && event_matches
        })
    }

    fn next_seq(&mut self) -> u64 {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
        seq
    }
}

struct SubscriptionFilter {
    session_id: Option<SessionId>,
    event_types: HashSet<String>,
}

fn render_input_items(input: &[crate::InputItem]) -> Option<String> {
    let parts = input
        .iter()
        .map(|item| match item {
            crate::InputItem::Text { text } => text.trim().to_string(),
            crate::InputItem::Skill { id } => format!("[skill:{id}]"),
            crate::InputItem::LocalImage { path } => format!("[image:{}]", path.display()),
            crate::InputItem::Mention { path, name } => {
                format!("[mention:{}]", name.as_deref().unwrap_or(path.as_str()))
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}
