use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{mpsc, Mutex};

use clawcr_core::{ItemId, SessionId, SessionTitleState, TurnId, TurnStatus};

use crate::{
    ClientTransportKind, ConnectionState, ErrorResponse, EventsSubscribeParams,
    EventsSubscribeResult, InitializeParams, InitializeResult, NotificationEnvelope, ProtocolError,
    ProtocolErrorCode, ServerCapabilities, ServerEvent, ServerRequestResolvedPayload,
    SessionEventPayload, SessionForkParams, SessionForkResult, SessionResumeParams,
    SessionResumeResult, SessionRuntimeStatus, SessionStartParams, SessionStartResult,
    SessionSummary, SteerInputRecord, SuccessResponse, TurnEventPayload, TurnInterruptParams,
    TurnInterruptResult, TurnStartParams, TurnStartResult, TurnSteerParams, TurnSteerResult,
    TurnSummary,
};

/// Shared runtime state backing every server transport connection.
pub struct ServerRuntime {
    /// The immutable runtime metadata returned during handshake.
    metadata: InitializeResult,
    /// Mutable sessions keyed by stable session identifier.
    sessions: Mutex<HashMap<SessionId, RuntimeSession>>,
    /// Mutable connection state keyed by stable connection identifier.
    connections: Mutex<HashMap<u64, ConnectionRuntime>>,
    /// Monotonic connection identifier counter.
    next_connection_id: AtomicU64,
}

impl ServerRuntime {
    /// Creates a new transport-facing server runtime.
    pub fn new(server_home: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            metadata: InitializeResult {
                server_name: "clawcr-server".into(),
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
            sessions: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
        })
    }

    /// Registers one new connection and returns its stable identifier.
    pub async fn register_connection(
        self: &Arc<Self>,
        transport: ClientTransportKind,
        sender: mpsc::UnboundedSender<serde_json::Value>,
    ) -> u64 {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::SeqCst);
        self.connections.lock().await.insert(
            connection_id,
            ConnectionRuntime {
                transport,
                state: ConnectionState::Connected,
                sender,
                opt_out_notification_methods: HashSet::new(),
                subscriptions: Vec::new(),
            },
        );
        connection_id
    }

    /// Removes one connection from the runtime.
    pub async fn unregister_connection(&self, connection_id: u64) {
        self.connections.lock().await.remove(&connection_id);
    }

    /// Dispatches one JSON-RPC-like request object and returns an optional response.
    pub async fn handle_incoming(
        &self,
        connection_id: u64,
        message: serde_json::Value,
    ) -> Option<serde_json::Value> {
        let method = message.get("method")?.as_str()?.to_string();
        let id = message.get("id").cloned();
        let params = message
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        if method == "initialized" {
            let mut connections = self.connections.lock().await;
            if let Some(connection) = connections.get_mut(&connection_id) {
                connection.state = ConnectionState::Ready;
            }
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

        match method.as_str() {
            "session/start" => match id {
                Some(request_id) => Some(
                    self.handle_session_start(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            "session/resume" => match id {
                Some(request_id) => Some(
                    self.handle_session_resume(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            "session/fork" => match id {
                Some(request_id) => Some(
                    self.handle_session_fork(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            "turn/start" => match id {
                Some(request_id) => Some(
                    self.handle_turn_start(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            "turn/interrupt" => match id {
                Some(request_id) => Some(
                    self.handle_turn_interrupt(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            "turn/steer" => match id {
                Some(request_id) => Some(
                    self.handle_turn_steer(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            "approval/respond" => id.map(|request_id| {
                self.error_response(
                    request_id,
                    ProtocolErrorCode::ApprovalNotFound,
                    "no pending approval request exists for this runtime",
                )
            }),
            "events/subscribe" => match id {
                Some(request_id) => Some(
                    self.handle_events_subscribe(connection_id, request_id, params)
                        .await,
                ),
                None => None,
            },
            _ => id.map(|request_id| {
                self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("unknown method: {method}"),
                )
            }),
        }
    }

    async fn handle_initialize(
        &self,
        connection_id: u64,
        id: Option<serde_json::Value>,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let request_id = id.unwrap_or_else(|| serde_json::json!(null));
        match serde_json::from_value::<InitializeParams>(params) {
            Ok(params) => {
                let mut connections = self.connections.lock().await;
                if let Some(connection) = connections.get_mut(&connection_id) {
                    connection.state = ConnectionState::Initializing;
                    connection.transport = params.transport;
                    connection.opt_out_notification_methods =
                        params.opt_out_notification_methods.into_iter().collect();
                }
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
                )
            }
        };

        let now = Utc::now();
        let session_id = SessionId::new();
        let summary = SessionSummary {
            session_id,
            cwd: params.cwd.clone(),
            created_at: now,
            updated_at: now,
            title: params.title.clone(),
            title_state: params
                .title
                .as_ref()
                .map(|_| {
                    SessionTitleState::Final(clawcr_core::SessionTitleFinalSource::ExplicitCreate)
                })
                .unwrap_or(SessionTitleState::Unset),
            ephemeral: params.ephemeral,
            resolved_model: params.model.clone(),
            status: SessionRuntimeStatus::Idle,
        };

        self.sessions.lock().await.insert(
            session_id,
            RuntimeSession {
                summary: summary.clone(),
                active_turn: None,
                latest_turn: None,
                loaded_item_count: 0,
                steering_queue: VecDeque::new(),
            },
        );
        self.subscribe_connection_to_session(connection_id, session_id, None)
            .await;

        self.broadcast_event(ServerEvent::SessionStarted(SessionEventPayload {
            session: summary.clone(),
        }))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionStartResult {
                session_id,
                created_at: now,
                cwd: params.cwd,
                ephemeral: params.ephemeral,
                resolved_model: params.model,
            },
        })
        .expect("serialize session/start response")
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
                )
            }
        };

        let sessions = self.sessions.lock().await;
        let Some(session) = sessions.get(&params.session_id) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let session_summary = session.summary.clone();
        let latest_turn = session.latest_turn.clone();
        let loaded_item_count = session.loaded_item_count;
        drop(sessions);
        self.subscribe_connection_to_session(connection_id, params.session_id, None)
            .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionResumeResult {
                session: session_summary,
                latest_turn,
                loaded_item_count,
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
                )
            }
        };

        let mut sessions = self.sessions.lock().await;
        let Some(source) = sessions.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };

        let now = Utc::now();
        let forked_id = SessionId::new();
        let session = SessionSummary {
            session_id: forked_id,
            cwd: params.cwd.unwrap_or_else(|| source.summary.cwd.clone()),
            created_at: now,
            updated_at: now,
            title: params.title.or_else(|| source.summary.title.clone()),
            title_state: source.summary.title_state.clone(),
            ephemeral: source.summary.ephemeral,
            resolved_model: source.summary.resolved_model.clone(),
            status: SessionRuntimeStatus::Idle,
        };
        sessions.insert(
            forked_id,
            RuntimeSession {
                summary: session.clone(),
                active_turn: None,
                latest_turn: source.latest_turn.clone(),
                loaded_item_count: source.loaded_item_count,
                steering_queue: VecDeque::new(),
            },
        );
        drop(sessions);
        self.subscribe_connection_to_session(connection_id, forked_id, None)
            .await;

        self.broadcast_event(ServerEvent::SessionStarted(SessionEventPayload {
            session: session.clone(),
        }))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionForkResult {
                session,
                forked_from_session_id: params.session_id,
            },
        })
        .expect("serialize session/fork response")
    }

    async fn handle_turn_start(
        &self,
        connection_id: u64,
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
                )
            }
        };

        if params.input.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        }

        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(&params.session_id) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };

        if session.active_turn.is_some() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::TurnAlreadyRunning,
                "session already has an active turn",
            );
        }

        let now = Utc::now();
        let turn_summary = TurnSummary {
            turn_id: TurnId::new(),
            session_id: params.session_id,
            sequence: session
                .latest_turn
                .as_ref()
                .map_or(1, |turn| turn.sequence + 1),
            status: TurnStatus::Running,
            model_slug: params
                .model
                .clone()
                .or_else(|| session.summary.resolved_model.clone())
                .unwrap_or_else(|| "default-model".into()),
            started_at: now,
            completed_at: None,
        };

        session.summary.status = SessionRuntimeStatus::ActiveTurn;
        if let Some(cwd) = params.cwd {
            session.summary.cwd = cwd;
        }
        session.active_turn = Some(turn_summary.clone());
        session.steering_queue.clear();
        drop(sessions);

        self.emit_to_connection(
            connection_id,
            "turn/started",
            ServerEvent::TurnStarted(TurnEventPayload {
                session_id: params.session_id,
                turn: turn_summary.clone(),
            }),
        )
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnStartResult {
                turn_id: turn_summary.turn_id,
                status: turn_summary.status.clone(),
                accepted_at: now,
            },
        })
        .expect("serialize turn/start response")
    }

    async fn handle_turn_interrupt(
        &self,
        connection_id: u64,
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
                )
            }
        };

        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(&params.session_id) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };

        let Some(active_turn) = session.active_turn.as_mut() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::TurnNotFound,
                "turn is not active",
            );
        };
        if active_turn.turn_id != params.turn_id {
            return self.error_response(
                request_id,
                ProtocolErrorCode::TurnNotFound,
                "turn does not exist",
            );
        }

        active_turn.status = TurnStatus::Interrupted;
        active_turn.completed_at = Some(Utc::now());
        let interrupted_turn = active_turn.clone();
        session.latest_turn = Some(interrupted_turn.clone());
        session.active_turn = None;
        session.summary.status = SessionRuntimeStatus::Idle;
        drop(sessions);

        self.emit_to_connection(
            connection_id,
            "turn/interrupted",
            ServerEvent::TurnInterrupted(TurnEventPayload {
                session_id: params.session_id,
                turn: interrupted_turn.clone(),
            }),
        )
        .await;
        self.emit_to_connection(
            connection_id,
            "turn/completed",
            ServerEvent::TurnCompleted(TurnEventPayload {
                session_id: params.session_id,
                turn: interrupted_turn.clone(),
            }),
        )
        .await;

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
                )
            }
        };

        if params.input.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn steer input is empty",
            );
        }

        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(&params.session_id) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let Some(active_turn) = session.active_turn.as_ref() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::NoActiveTurn,
                "no active turn exists",
            );
        };
        if active_turn.turn_id != params.expected_turn_id {
            return self.error_response(
                request_id,
                ProtocolErrorCode::ExpectedTurnMismatch,
                "active turn did not match expectedTurnId",
            );
        }

        session.steering_queue.push_back(SteerInputRecord {
            item_id: ItemId::new(),
            received_at: Utc::now(),
            input: params.input,
        });
        let turn_id = active_turn.turn_id;
        drop(sessions);

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
                )
            }
        };

        let subscription_id = format!("sub-{connection_id}-1");
        self.connections
            .lock()
            .await
            .entry(connection_id)
            .and_modify(|connection| {
                connection.subscriptions.push(SubscriptionFilter {
                    session_id: params.session_id,
                    event_types: params.event_types.unwrap_or_default().into_iter().collect(),
                });
            });

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: EventsSubscribeResult {
                subscription_id: subscription_id.into(),
            },
        })
        .expect("serialize events/subscribe response")
    }

    async fn subscribe_connection_to_session(
        &self,
        connection_id: u64,
        session_id: SessionId,
        event_types: Option<HashSet<String>>,
    ) {
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get_mut(&connection_id) {
            let already_subscribed = connection.subscriptions.iter().any(|subscription| {
                subscription.session_id == Some(session_id)
                    && subscription.event_types == event_types.clone().unwrap_or_default()
            });
            if already_subscribed {
                return;
            }
            connection.subscriptions.push(SubscriptionFilter {
                session_id: Some(session_id),
                event_types: event_types.unwrap_or_default(),
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
        let value = serde_json::to_value(NotificationEnvelope {
            method: method.to_string(),
            params: event,
        })
        .expect("serialize notification");
        if let Some(connection) = self.connections.lock().await.get(&connection_id).cloned() {
            if !connection.should_deliver(method, session_id) {
                return;
            }
            let _ = connection.sender.send(value);
        }
    }

    async fn broadcast_event(&self, event: ServerEvent) {
        let session_id = event.session_id();
        let method = match &event {
            ServerEvent::SessionStarted(_) => "session/started",
            ServerEvent::SessionStatusChanged(_) => "session/status/changed",
            ServerEvent::SessionArchived(_) => "session/archived",
            ServerEvent::SessionUnarchived(_) => "session/unarchived",
            ServerEvent::SessionClosed(_) => "session/closed",
            ServerEvent::TurnStarted(_) => "turn/started",
            ServerEvent::TurnCompleted(_) => "turn/completed",
            ServerEvent::TurnInterrupted(_) => "turn/interrupted",
            ServerEvent::TurnFailed(_) => "turn/failed",
            ServerEvent::TurnPlanUpdated(_) => "turn/plan/updated",
            ServerEvent::TurnDiffUpdated(_) => "turn/diff/updated",
            ServerEvent::ItemStarted(_) => "item/started",
            ServerEvent::ItemCompleted(_) => "item/completed",
            ServerEvent::ItemDelta { .. } => "item/delta",
            ServerEvent::ServerRequestResolved(_) => "serverRequest/resolved",
        };

        let value = serde_json::to_value(NotificationEnvelope {
            method: method.to_string(),
            params: event,
        })
        .expect("serialize notification");
        let connections = self.connections.lock().await;
        for connection in connections.values() {
            if !connection.should_deliver(method, session_id) {
                continue;
            }
            let _ = connection.sender.send(value.clone());
        }
    }

    fn error_response(
        &self,
        request_id: serde_json::Value,
        code: ProtocolErrorCode,
        message: impl Into<String>,
    ) -> serde_json::Value {
        serde_json::to_value(ErrorResponse {
            id: request_id,
            error: ProtocolError {
                code,
                message: message.into(),
                data: serde_json::json!({}),
            },
        })
        .expect("serialize error response")
    }
}

#[derive(Clone)]
struct ConnectionRuntime {
    transport: ClientTransportKind,
    state: ConnectionState,
    sender: mpsc::UnboundedSender<serde_json::Value>,
    opt_out_notification_methods: HashSet<String>,
    subscriptions: Vec<SubscriptionFilter>,
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
}

#[derive(Clone)]
struct SubscriptionFilter {
    session_id: Option<SessionId>,
    event_types: HashSet<String>,
}

#[derive(Clone)]
struct RuntimeSession {
    summary: SessionSummary,
    active_turn: Option<TurnSummary>,
    latest_turn: Option<TurnSummary>,
    loaded_item_count: u64,
    steering_queue: VecDeque<SteerInputRecord>,
}

impl ServerEvent {
    fn session_id(&self) -> Option<SessionId> {
        match self {
            ServerEvent::SessionStarted(payload)
            | ServerEvent::SessionArchived(payload)
            | ServerEvent::SessionUnarchived(payload)
            | ServerEvent::SessionClosed(payload) => Some(payload.session.session_id),
            ServerEvent::SessionStatusChanged(payload) => Some(payload.session_id),
            ServerEvent::TurnStarted(payload)
            | ServerEvent::TurnCompleted(payload)
            | ServerEvent::TurnInterrupted(payload)
            | ServerEvent::TurnFailed(payload)
            | ServerEvent::TurnPlanUpdated(payload)
            | ServerEvent::TurnDiffUpdated(payload) => Some(payload.session_id),
            ServerEvent::ItemStarted(payload) | ServerEvent::ItemCompleted(payload) => {
                Some(payload.context.session_id)
            }
            ServerEvent::ItemDelta { payload, .. } => Some(payload.context.session_id),
            ServerEvent::ServerRequestResolved(payload) => Some(payload.session_id),
        }
    }
}
