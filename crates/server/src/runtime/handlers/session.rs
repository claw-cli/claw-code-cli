use super::super::*;

impl ServerRuntime {
    pub(crate) async fn handle_initialize(
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

    pub(crate) async fn handle_session_start(
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

    pub(crate) async fn handle_session_list(
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

    pub(crate) async fn handle_session_metadata_update(
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

    pub(crate) async fn handle_session_title_update(
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

    pub(crate) async fn handle_session_resume(
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

    pub(crate) async fn handle_session_fork(
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
        let now = Utc::now();
        let forked_id = SessionId::new();
        let forked_runtime = match self
            .build_runtime_session_from_user_turn_cut(
                &source,
                forked_id,
                params.user_turn_index,
                params.cwd.clone(),
                params.title.clone(),
                now,
            )
            .await
        {
            Ok(runtime) => runtime,
            Err(message) => {
                return self.error_response(request_id, ProtocolErrorCode::InvalidParams, message);
            }
        };
        let summary = forked_runtime.summary.clone();
        drop(source);
        self.sessions
            .lock()
            .await
            .insert(forked_id, forked_runtime.shared());
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

    pub(crate) async fn handle_session_rollback(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: SessionRollbackParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid session/rollback params: {error}"),
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
        let source = session_arc.lock().await;
        let rebuilt = match self
            .build_runtime_session_from_user_turn_cut(
                &source,
                params.session_id,
                Some(params.user_turn_index),
                None,
                source.summary.title.clone(),
                source.summary.created_at,
            )
            .await
        {
            Ok(runtime) => runtime,
            Err(message) => {
                return self.error_response(request_id, ProtocolErrorCode::InvalidParams, message);
            }
        };
        let summary = rebuilt.summary.clone();
        let latest_turn = rebuilt.latest_turn.clone();
        let loaded_item_count = rebuilt.loaded_item_count;
        let history_items = rebuilt.history_items.clone();
        drop(source);
        *session_arc.lock().await = rebuilt;
        self.subscribe_connection_to_session(connection_id, params.session_id, None)
            .await;
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: SessionRollbackResult {
                session: summary,
                latest_turn,
                loaded_item_count,
                history_items,
                pending_texts: Vec::new(),
            },
        })
        .expect("serialize session/rollback response")
    }

    pub(crate) async fn build_runtime_session_from_user_turn_cut(
        &self,
        source: &RuntimeSession,
        session_id: SessionId,
        user_turn_index: Option<u32>,
        cwd_override: Option<PathBuf>,
        title_override: Option<String>,
        created_at: chrono::DateTime<Utc>,
    ) -> Result<RuntimeSession, String> {
        let source_core_session = source.core_session.lock().await;
        let kept_items = if let Some(user_turn_index) = user_turn_index {
            let mut user_turn_ids: Vec<TurnId> = Vec::new();
            for item in &source.persisted_turn_items {
                if matches!(item.turn_item, TurnItem::UserMessage(_))
                    && user_turn_ids.last().copied() != Some(item.turn_id)
                {
                    user_turn_ids.push(item.turn_id);
                }
            }
            let selected_idx = usize::try_from(user_turn_index)
                .map_err(|_| "selected turn index is invalid".to_string())?;
            let Some(selected_turn_id) = user_turn_ids.get(selected_idx).copied() else {
                return Err("selected turn does not exist".to_string());
            };
            source
                .persisted_turn_items
                .iter()
                .take_while(|item| item.turn_id != selected_turn_id)
                .cloned()
                .chain(
                    source
                        .persisted_turn_items
                        .iter()
                        .skip_while(|item| item.turn_id != selected_turn_id)
                        .take_while(|item| item.turn_id == selected_turn_id)
                        .cloned(),
                )
                .collect::<Vec<_>>()
        } else {
            source.persisted_turn_items.clone()
        };

        let cwd = cwd_override.unwrap_or_else(|| source.summary.cwd.clone());
        let mut core_session = self.deps.new_session_state(session_id, cwd.clone());
        core_session.session_context = source_core_session.session_context.clone();
        core_session.latest_turn_context = None;
        core_session.total_input_tokens = source_core_session.total_input_tokens;
        core_session.total_output_tokens = source_core_session.total_output_tokens;
        core_session.total_cache_creation_tokens = source_core_session.total_cache_creation_tokens;
        core_session.total_cache_read_tokens = source_core_session.total_cache_read_tokens;
        core_session.last_input_tokens = source_core_session.last_input_tokens;

        let mut rebuilt_history_items = Vec::new();
        let mut rebuilt_messages = Vec::new();
        let mut tool_names_by_id = HashMap::new();
        for item in &kept_items {
            crate::persistence::apply_turn_item(
                &mut rebuilt_messages,
                &mut rebuilt_history_items,
                &mut tool_names_by_id,
                item.turn_item.clone(),
            );
        }
        core_session.messages = rebuilt_messages;
        core_session.prompt_messages = None;
        core_session.turn_count = kept_items
            .iter()
            .filter(|item| matches!(item.turn_item, TurnItem::UserMessage(_)))
            .count();

        let latest_turn = if let Some(last_turn_id) = kept_items.last().map(|item| item.turn_id) {
            source
                .latest_turn
                .clone()
                .filter(|turn| turn.turn_id == last_turn_id)
                .or_else(|| {
                    let sequence = kept_items
                        .iter()
                        .filter(|item| matches!(item.turn_item, TurnItem::UserMessage(_)))
                        .count() as u32;
                    Some(TurnMetadata {
                        turn_id: last_turn_id,
                        session_id,
                        sequence,
                        status: TurnStatus::Completed,
                        kind: devo_protocol::TurnKind::Regular,
                        model: source
                            .summary
                            .model
                            .clone()
                            .unwrap_or_else(|| self.deps.default_model.clone()),
                        thinking: source.summary.thinking.clone(),
                        reasoning_effort: source.summary.reasoning_effort,
                        request_model: source
                            .summary
                            .model
                            .clone()
                            .unwrap_or_else(|| self.deps.default_model.clone()),
                        request_thinking: source.summary.thinking.clone(),
                        started_at: source.summary.created_at,
                        completed_at: Some(source.summary.updated_at),
                        usage: None,
                    })
                })
        } else {
            None
        };

        let updated_at = Utc::now();
        let summary = crate::SessionMetadata {
            session_id,
            cwd: cwd.clone(),
            created_at,
            updated_at,
            title: title_override.or_else(|| source.summary.title.clone()),
            title_state: source.summary.title_state.clone(),
            ephemeral: source.summary.ephemeral,
            model: source.summary.model.clone(),
            thinking: source.summary.thinking.clone(),
            reasoning_effort: source.summary.reasoning_effort,
            total_input_tokens: source_core_session.total_input_tokens,
            total_output_tokens: source_core_session.total_output_tokens,
            prompt_token_estimate: source_core_session.prompt_token_estimate,
            status: SessionRuntimeStatus::Idle,
        };
        drop(source_core_session);

        let pending_turn_queue = Arc::clone(&core_session.pending_turn_queue);
        let btw_input_queue = Arc::clone(&core_session.btw_input_queue);
        Ok(RuntimeSession {
            record: None,
            summary,
            core_session: Arc::new(Mutex::new(core_session)),
            active_turn: None,
            latest_turn,
            loaded_item_count: u64::try_from(kept_items.len()).unwrap_or(u64::MAX),
            history_items: rebuilt_history_items,
            persisted_turn_items: kept_items,
            latest_compaction_snapshot: None,
            pending_turn_queue,
            btw_input_queue,
            active_task: None,
            deferred_assistant: None,
            deferred_reasoning: None,
            next_item_seq: u64::try_from(source.persisted_turn_items.len().saturating_add(1))
                .unwrap_or(u64::MAX),
            first_user_input: source.first_user_input.clone(),
        })
    }
}
