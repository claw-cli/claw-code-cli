use super::*;

impl TuiApp {
    pub(crate) fn handle_worker_event(&mut self, event: WorkerEvent) {
        // Worker events are intentionally reduced to UI state transitions here so the
        // rendering layer stays a pure projection of application state.
        match event {
            WorkerEvent::TurnStarted => {
                self.busy = true;
                self.set_turn_status_line("Thinking");
                self.status_message = "Thinking".to_string();
                self.pending_assistant_index = None;
            }
            WorkerEvent::TextDelta(text) => {
                let index = self.ensure_assistant_item();
                self.transcript[index].body.push_str(&text);
                self.status_message = "Streaming response".to_string();
                if self.follow_output {
                    self.scroll = 0;
                }
            }
            WorkerEvent::ToolCall {
                tool_use_id,
                summary,
                detail: _detail,
            } => {
                self.pending_assistant_index = None;
                self.transcript
                    .push(TranscriptItem::tool_call(summary.clone()));
                let index = self.transcript.len() - 1;
                if self.follow_output {
                    self.scroll = 0;
                }
                self.pending_tool_items.insert(tool_use_id, index);
                if self.busy {
                    self.show_turn_status_line("Thinking");
                }
                self.status_message = format!("{summary}...");
            }
            WorkerEvent::ToolResult {
                tool_use_id,
                preview,
                is_error,
                truncated: _,
            } => {
                let kind = if is_error {
                    TranscriptItemKind::Error
                } else {
                    TranscriptItemKind::ToolResult
                };
                let body = preview.trim().to_string();
                if let Some(index) = self.pending_tool_items.remove(&tool_use_id) {
                    if let Some(item) = self.transcript.get_mut(index) {
                        if kind == TranscriptItemKind::ToolResult {
                            *item = TranscriptItem::live_tool_result(item.title.clone(), body);
                        } else {
                            *item = TranscriptItem::tool_error(item.title.clone(), body);
                        }
                    }
                    if self.follow_output {
                        self.scroll = 0;
                    }
                } else if let Some(item) = self.transcript.last_mut() {
                    if item.kind == TranscriptItemKind::ToolCall {
                        if kind == TranscriptItemKind::ToolResult {
                            *item = TranscriptItem::live_tool_result(item.title.clone(), body);
                        } else {
                            *item = TranscriptItem::tool_error(item.title.clone(), body);
                        }
                        if self.follow_output {
                            self.scroll = 0;
                        }
                    } else {
                        let title = if is_error {
                            "Tool error"
                        } else {
                            "Tool output"
                        };
                        if kind == TranscriptItemKind::ToolResult {
                            self.transcript
                                .push(TranscriptItem::live_tool_result(title, body));
                            if self.follow_output {
                                self.scroll = 0;
                            }
                        } else {
                            self.transcript
                                .push(TranscriptItem::tool_error(title, body));
                            if self.follow_output {
                                self.scroll = 0;
                            }
                        }
                    }
                } else {
                    let title = if is_error {
                        "Tool error"
                    } else {
                        "Tool output"
                    };
                    if kind == TranscriptItemKind::ToolResult {
                        self.transcript
                            .push(TranscriptItem::live_tool_result(title, body));
                        if self.follow_output {
                            self.scroll = 0;
                        }
                    } else {
                        self.transcript
                            .push(TranscriptItem::tool_error(title, body));
                        if self.follow_output {
                            self.scroll = 0;
                        }
                    }
                }
                if self.busy {
                    self.show_turn_status_line("Thinking");
                }
                self.status_message = if is_error {
                    "Tool returned an error".to_string()
                } else {
                    "Tool completed".to_string()
                };
            }
            WorkerEvent::UsageUpdated {
                total_input_tokens,
                total_output_tokens,
            } => {
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
            }
            WorkerEvent::TurnFinished {
                stop_reason,
                turn_count,
                total_input_tokens,
                total_output_tokens,
            } => {
                self.busy = false;
                self.clear_turn_status_line();
                self.pending_assistant_index = None;
                self.pending_tool_items.clear();
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                if stop_reason == "Interrupted" {
                    self.push_item(TranscriptItemKind::System, "Interrupted", "");
                } else {
                    self.push_item(TranscriptItemKind::System, "Complete", "");
                }
                self.status_message = format!("Turn completed ({stop_reason})");
            }
            WorkerEvent::TurnFailed {
                message,
                turn_count,
                total_input_tokens,
                total_output_tokens,
            } => {
                self.busy = false;
                self.clear_turn_status_line();
                self.pending_assistant_index = None;
                self.pending_tool_items.clear();
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.push_item(TranscriptItemKind::Error, "Error", message);
                self.status_message = "Query failed; see error above".to_string();
            }
            WorkerEvent::ProviderValidationSucceeded { reply_preview } => {
                self.busy = false;
                self.push_item(
                    TranscriptItemKind::System,
                    "Onboarding",
                    format!("Validation reply: {reply_preview}"),
                );
                if let Err(error) = self.finish_onboarding_selection() {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Onboarding failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to save onboarding settings".to_string();
                    self.onboarding_api_key_pending = true;
                    self.onboarding_prompt = Some("api key".to_string());
                }
            }
            WorkerEvent::ProviderValidationFailed { message } => {
                self.busy = false;
                self.push_item(
                    TranscriptItemKind::Error,
                    "Validation failed",
                    message.clone(),
                );
                self.onboarding_api_key_pending = true;
                self.onboarding_prompt = Some("api key".to_string());
                self.input.clear();
                self.status_message = format!("Validation failed: {message}");
            }
            WorkerEvent::SessionsListed { sessions } => {
                self.show_session_panel(sessions);
                self.status_message = "Sessions loaded".to_string();
            }
            WorkerEvent::NewSessionPrepared => {
                self.aux_panel = None;
                self.aux_panel_selection = 0;
                self.pending_status_index = None;
                self.pending_assistant_index = None;
                self.pending_tool_items.clear();
                self.busy = false;
                self.total_input_tokens = 0;
                self.total_output_tokens = 0;
                self.transcript.clear();
                self.follow_output = true;
                self.scroll = 0;
                self.status_message = "New session ready; send a prompt to start it".to_string();
            }
            WorkerEvent::SessionSwitched {
                session_id,
                title,
                model: _model,
                total_input_tokens,
                total_output_tokens,
                history_items,
                loaded_item_count,
            } => {
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.aux_panel = None;
                self.aux_panel_selection = 0;
                self.pending_status_index = None;
                self.pending_assistant_index = None;
                self.busy = false;
                self.transcript = history_items;
                self.pending_tool_items.clear();
                self.follow_output = true;
                self.scroll = 0;
                self.status_message = format!("Active session: {session_id}");
                if self.transcript.is_empty() {
                    self.push_item(
                        TranscriptItemKind::System,
                        "Session",
                        format!(
                            "switched to {}\ntitle: {}\nloaded items: {}",
                            session_id,
                            title.unwrap_or_else(|| "(untitled)".to_string()),
                            loaded_item_count
                        ),
                    );
                }
            }
            WorkerEvent::SessionRenamed { session_id, title } => {
                self.push_item(
                    TranscriptItemKind::System,
                    "Session",
                    format!("renamed {} to {}", session_id, title),
                );
                self.status_message = "Session renamed".to_string();
            }
            WorkerEvent::SessionTitleUpdated { session_id, title } => {
                if let Some(AuxPanel {
                    content: AuxPanelContent::SessionList(entries),
                    ..
                }) = self.aux_panel.as_mut()
                {
                    if let Some(entry) = entries
                        .iter_mut()
                        .find(|entry| entry.session_id.to_string() == session_id)
                    {
                        entry.title = title.clone();
                    }
                }
                self.status_message = format!("Session titled: {title}");
            }
        }
    }

    pub(crate) fn ensure_assistant_item(&mut self) -> usize {
        if let Some(index) = self.pending_assistant_index {
            return index;
        }

        self.transcript.push(TranscriptItem::new(
            TranscriptItemKind::Assistant,
            "Assistant",
            String::new(),
        ));
        let index = self.transcript.len() - 1;
        self.pending_assistant_index = Some(index);
        index
    }

    pub(crate) fn push_item(
        &mut self,
        kind: TranscriptItemKind,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> usize {
        self.transcript.push(TranscriptItem::new(kind, title, body));
        if self.follow_output {
            self.scroll = 0;
        }
        self.transcript.len() - 1
    }

    pub(crate) fn advance_transcript_folds(&mut self, now: Instant) -> bool {
        // Tool results compact over time so long outputs briefly stay readable before
        // collapsing to a smaller transcript footprint.
        let mut changed = false;
        for item in &mut self.transcript {
            if item.advance_fold(now) {
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn set_turn_status_line(&mut self, title: impl Into<String>) {
        if let Some(index) = self.pending_status_index {
            if let Some(item) = self.transcript.get_mut(index) {
                item.title = title.into();
                item.body.clear();
            }
        }
    }

    pub(crate) fn show_turn_status_line(&mut self, title: impl Into<String>) {
        self.clear_turn_status_line();
        self.pending_status_index =
            Some(self.push_item(TranscriptItemKind::System, title.into(), ""));
    }

    pub(crate) fn clear_turn_status_line(&mut self) {
        if let Some(index) = self.pending_status_index.take() {
            if index < self.transcript.len() {
                self.transcript.remove(index);
            }
            if let Some(pending_assistant_index) = self.pending_assistant_index {
                if pending_assistant_index > index {
                    self.pending_assistant_index = Some(pending_assistant_index - 1);
                } else if pending_assistant_index == index {
                    self.pending_assistant_index = None;
                }
            }
        }
    }
}

pub(crate) fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        "(empty)".to_string()
    } else {
        "*".repeat(value.chars().count().min(8))
    }
}
