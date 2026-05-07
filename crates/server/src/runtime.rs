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
use devo_core::ResponseItem;
use devo_core::SessionId;
use devo_core::SessionTitleFinalSource;
use devo_core::SessionTitleState;
use devo_core::TextItem;
use devo_core::TokenInfo;
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
use crate::SessionRollbackParams;
use crate::SessionRollbackResult;
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

mod handlers;
mod model_api;
mod skills;
mod turn_exec;

pub struct ServerRuntime {
    metadata: InitializeResult,
    deps: ServerRuntimeDependencies,
    rollout_store: RolloutStore,
    /// Thread safe hashmap as sessions container, there are allowed multiple sessions.
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
                    session_rollback: true,
                    turn_interrupt: true,
                    approval_requests: true,
                    event_streaming: true,
                    model_catalog: true,
                    model_saved: true,
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

            // TODO: I think the check here is unnecessary, cause the 'ephemeral' session
            // will not be persisted rollout.
            // Skip ephemeral sessions
            if session.summary.ephemeral {
                continue;
            }

            // TODO: Should add stats field to describe `total input cached tokens` / `total read cached tokens`, should add stats field to describe `context window usage`.
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

        // Before connection enter `Ready` state, only allowed method: "initialized" or "initialize"
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
            "session/rollback" => Some(
                self.handle_session_rollback(connection_id, id?, params)
                    .await,
            ),
            "session/compact" => Some(self.handle_session_compact(id?, params).await),
            "skills/list" => Some(self.handle_skills_list(id?, params).await),
            "skills/changed" => Some(self.handle_skills_changed(id?, params).await),
            "model/catalog" => Some(self.handle_model_catalog(id?, params).await),
            "model/saved" => Some(self.handle_model_saved(id?, params).await),
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
                        turn_id,
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
