use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::*;

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
            let mut pending_tool_calls: HashMap<String, (ItemId, u64, serde_json::Value)> =
                HashMap::new();
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
                session.deferred_assistant.take()
            } {
                runtime
                    .complete_item(
                        session_id,
                        turn_for_events.turn_id,
                        item_id,
                        item_seq,
                        ItemKind::AgentMessage,
                        TurnItem::AgentMessage(TextItem { text: text.clone() }),
                        serde_json::json!({ "title": "Assistant", "text": text }),
                    )
                    .await;
            }
            if let Some((item_id, item_seq, text)) = {
                let mut session = event_session_arc.lock().await;
                session.deferred_reasoning.take()
            } {
                runtime
                    .complete_item(
                        session_id,
                        turn_for_events.turn_id,
                        item_id,
                        item_seq,
                        ItemKind::Reasoning,
                        TurnItem::Reasoning(TextItem { text: text.clone() }),
                        serde_json::json!({ "title": "Reasoning", "text": text }),
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
            let permission_profile = core_session.config.permission_profile.clone();
            let runtime = ToolRuntime::new_with_context(
                Arc::clone(&registry),
                self.build_permission_checker(
                    session_id,
                    turn_for_events.turn_id,
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
