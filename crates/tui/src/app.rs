use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use crate::{
    events::{TranscriptItem, TranscriptItemKind, WorkerEvent},
    input::InputBuffer,
    render,
    slash::{matching_slash_commands, SlashCommandSpec},
    terminal::ManagedTerminal,
    worker::{QueryWorkerConfig, QueryWorkerHandle},
    InteractiveTuiConfig,
};

/// Summary returned when the interactive TUI exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppExit {
    /// Total turns completed in the session.
    pub turn_count: usize,
    /// Total input tokens accumulated in the session.
    pub total_input_tokens: usize,
    /// Total output tokens accumulated in the session.
    pub total_output_tokens: usize,
}

/// In-memory application state for the interactive terminal UI.
pub(crate) struct TuiApp {
    /// Model identifier shown in the header.
    pub(crate) model: String,
    /// Current working directory shown in the header.
    pub(crate) cwd: PathBuf,
    /// Scrollable chat history pane.
    pub(crate) transcript: Vec<TranscriptItem>,
    /// Current composer buffer.
    pub(crate) input: InputBuffer,
    /// Current status bar text.
    pub(crate) status_message: String,
    /// Whether the model is currently producing output.
    pub(crate) busy: bool,
    /// Current spinner frame index.
    pub(crate) spinner_index: usize,
    /// Manual transcript scroll offset when follow mode is disabled.
    pub(crate) scroll: u16,
    /// Whether the transcript should stay pinned to the latest output.
    pub(crate) follow_output: bool,
    /// Total turns completed in the session.
    pub(crate) turn_count: usize,
    /// Total input tokens accumulated in the session.
    pub(crate) total_input_tokens: usize,
    /// Total output tokens accumulated in the session.
    pub(crate) total_output_tokens: usize,
    /// Currently selected slash-command suggestion row.
    pub(crate) slash_selection: usize,
    /// Index of the current turn status line rendered below the latest user message.
    pending_status_index: Option<usize>,
    /// Index of the assistant transcript item currently receiving streamed text.
    pending_assistant_index: Option<usize>,
    /// Background query worker owned by the UI.
    worker: QueryWorkerHandle,
    /// Whether the app should exit after the current loop iteration.
    should_quit: bool,
}

impl TuiApp {
    /// Runs the full interactive UI until the user exits.
    pub(crate) async fn run(config: InteractiveTuiConfig) -> Result<AppExit> {
        let startup_prompt = config.startup_prompt.clone();
        let worker = QueryWorkerHandle::spawn(QueryWorkerConfig {
            model: config.model.clone(),
            cwd: config.cwd.clone(),
            server_env: config.server_env,
        });

        let mut app = Self {
            model: config.model,
            cwd: config.cwd,
            transcript: Vec::new(),
            input: InputBuffer::new(),
            status_message: "Ready".to_string(),
            busy: false,
            spinner_index: 0,
            scroll: 0,
            follow_output: true,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            slash_selection: 0,
            pending_status_index: None,
            pending_assistant_index: None,
            worker,
            should_quit: false,
        };

        if let Some(prompt) = startup_prompt {
            app.submit_prompt(prompt)?;
        }

        let mut terminal = ManagedTerminal::new()?;
        let mut event_stream = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(80));

        loop {
            terminal
                .terminal_mut()
                .draw(|frame| render::draw(frame, &app))?;

            if app.should_quit {
                break;
            }

            tokio::select! {
                maybe_event = event_stream.next() => {
                    match maybe_event {
                        Some(Ok(event)) => app.handle_terminal_event(event)?,
                        Some(Err(error)) => {
                            app.push_item(
                                TranscriptItemKind::Error,
                                "Terminal error",
                                error.to_string(),
                            );
                            app.status_message = "Terminal input error".to_string();
                        }
                        None => break,
                    }
                }
                maybe_event = app.worker.event_rx.recv() => {
                    match maybe_event {
                        Some(event) => app.handle_worker_event(event),
                        None => {
                            app.status_message = "Background worker stopped".to_string();
                            break;
                        }
                    }
                }
                _ = tick.tick() => {
                    app.spinner_index = app.spinner_index.wrapping_add(1);
                }
            }
        }

        app.worker.shutdown().await?;
        Ok(AppExit {
            turn_count: app.turn_count,
            total_input_tokens: app.total_input_tokens,
            total_output_tokens: app.total_output_tokens,
        })
    }

    fn handle_terminal_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.handle_key(key)
            }
            Event::Paste(text) => {
                self.input.insert_str(&text);
                self.reset_slash_selection();
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.transcript.clear();
                self.pending_status_index = None;
                self.pending_assistant_index = None;
                self.status_message = "Transcript cleared".to_string();
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.input.insert_char('\n');
            }
            KeyCode::Enter if !self.busy => {
                if self.try_accept_slash_selection() {
                    return;
                }
                let prompt = self.input.take();
                if let Err(error) = self.handle_submission(prompt) {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Submit failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to submit prompt".to_string();
                }
            }
            KeyCode::Backspace => {
                self.input.backspace();
                self.reset_slash_selection();
            }
            KeyCode::Delete => {
                self.input.delete();
                self.reset_slash_selection();
            }
            KeyCode::Tab if self.try_apply_slash_suggestion() => {}
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => {
                self.input.move_end();
                self.follow_output = true;
            }
            KeyCode::Up => {
                if self.has_slash_suggestions() {
                    self.move_slash_selection(-1);
                } else {
                    self.follow_output = false;
                    self.scroll = self.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if self.has_slash_suggestions() {
                    self.move_slash_selection(1);
                } else {
                    self.follow_output = false;
                    self.scroll = self.scroll.saturating_add(1);
                }
            }
            KeyCode::PageUp => {
                self.follow_output = false;
                self.scroll = self.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.follow_output = false;
                self.scroll = self.scroll.saturating_add(10);
            }
            KeyCode::Esc => {
                self.input.clear();
                self.reset_slash_selection();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.insert_char(ch);
                self.reset_slash_selection();
            }
            _ => {}
        }
    }

    fn handle_submission(&mut self, prompt: String) -> Result<()> {
        if prompt.trim_start().starts_with('/') {
            return self.handle_slash_command(prompt);
        }
        self.submit_prompt(prompt)
    }

    fn submit_prompt(&mut self, prompt: String) -> Result<()> {
        if self.input.is_blank() && prompt.trim().is_empty() {
            return Ok(());
        }

        self.push_item(TranscriptItemKind::User, "You", prompt.clone());
        self.pending_status_index =
            Some(self.push_item(TranscriptItemKind::System, "Thinking", ""));
        self.follow_output = true;
        self.busy = true;
        self.reset_slash_selection();
        self.pending_assistant_index = None;
        self.status_message = "Waiting for model response".to_string();
        self.worker.submit_prompt(prompt)
    }

    fn handle_slash_command(&mut self, prompt: String) -> Result<()> {
        let trimmed = prompt.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default();
        let argument = parts.next().map(str::trim).unwrap_or_default();

        match command {
            "/exit" => {
                self.push_item(
                    TranscriptItemKind::System,
                    "Command",
                    "Exiting interactive session",
                );
                self.status_message = "Exiting".to_string();
                self.should_quit = true;
                Ok(())
            }
            "/status" => {
                self.push_item(
                    TranscriptItemKind::System,
                    "Status",
                    format!(
                        "turns: {}\nmodel: {}\ntokens: {} in / {} out\nbusy: {}",
                        self.turn_count,
                        self.model,
                        self.total_input_tokens,
                        self.total_output_tokens,
                        self.busy
                    ),
                );
                self.status_message = "Session status shown".to_string();
                Ok(())
            }
            "/model" => {
                if argument.is_empty() {
                    self.push_item(
                        TranscriptItemKind::System,
                        "Model",
                        format!("current model: {}", self.model),
                    );
                    self.status_message = "Current model shown".to_string();
                    return Ok(());
                }

                self.worker.set_model(argument.to_string())?;
                self.model = argument.to_string();
                self.push_item(
                    TranscriptItemKind::System,
                    "Model",
                    format!("switched model to {}", self.model),
                );
                self.status_message = format!("Model set to {}", self.model);
                Ok(())
            }
            _ => {
                self.push_item(
                    TranscriptItemKind::Error,
                    "Unknown command",
                    "Available commands: /model [name], /status, /exit",
                );
                self.status_message = "Unknown command".to_string();
                Ok(())
            }
        }
    }

    fn handle_worker_event(&mut self, event: WorkerEvent) {
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
            }
            WorkerEvent::ToolCall { summary, detail } => {
                self.pending_assistant_index = None;
                self.push_item(
                    TranscriptItemKind::ToolCall,
                    summary.clone(),
                    detail.unwrap_or_default(),
                );
                self.status_message = format!("{summary}...");
            }
            WorkerEvent::ToolResult {
                preview,
                is_error,
                truncated: _,
            } => {
                let kind = if is_error {
                    TranscriptItemKind::Error
                } else {
                    TranscriptItemKind::ToolResult
                };
                let title = if is_error {
                    "Tool error"
                } else {
                    "Tool output"
                };
                let body = preview;
                self.push_item(kind, title, body);
                self.status_message = if is_error {
                    "Tool returned an error".to_string()
                } else {
                    "Tool completed".to_string()
                };
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
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
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
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.push_item(TranscriptItemKind::Error, "Error", message);
                self.status_message = "Query failed".to_string();
            }
        }
    }

    fn ensure_assistant_item(&mut self) -> usize {
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

    fn push_item(
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

    fn set_turn_status_line(&mut self, title: impl Into<String>) {
        if let Some(index) = self.pending_status_index {
            if let Some(item) = self.transcript.get_mut(index) {
                item.title = title.into();
                item.body.clear();
            }
        }
    }

    fn clear_turn_status_line(&mut self) {
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

    pub(crate) fn slash_suggestions(&self) -> Vec<SlashCommandSpec> {
        matching_slash_commands(self.input.text())
    }

    fn has_slash_suggestions(&self) -> bool {
        !self.slash_suggestions().is_empty()
    }

    fn reset_slash_selection(&mut self) {
        self.slash_selection = 0;
    }

    fn move_slash_selection(&mut self, delta: isize) {
        let suggestions = self.slash_suggestions();
        if suggestions.is_empty() {
            self.slash_selection = 0;
            return;
        }
        let len = suggestions.len() as isize;
        let next = (self.slash_selection as isize + delta).clamp(0, len - 1);
        self.slash_selection = next as usize;
    }

    fn try_apply_slash_suggestion(&mut self) -> bool {
        let suggestions = self.slash_suggestions();
        if suggestions.is_empty() {
            return false;
        }
        let selected = suggestions[self.slash_selection.min(suggestions.len() - 1)];
        self.input.replace(selected.name);
        self.reset_slash_selection();
        true
    }

    fn try_accept_slash_selection(&mut self) -> bool {
        let trimmed = self.input.text().trim();
        if trimmed.starts_with('/') && !trimmed.contains(char::is_whitespace) {
            let suggestions = self.slash_suggestions();
            if suggestions.len() == 1 && suggestions[0].name != trimmed {
                self.input.replace(suggestions[0].name);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::TuiApp;
    use crate::{
        events::{TranscriptItem, TranscriptItemKind, WorkerEvent},
        input::InputBuffer,
        worker::QueryWorkerHandle,
    };

    fn test_app() -> TuiApp {
        TuiApp {
            model: "test-model".to_string(),
            cwd: PathBuf::from("."),
            transcript: Vec::new(),
            input: InputBuffer::new(),
            status_message: "Ready".to_string(),
            busy: false,
            spinner_index: 0,
            scroll: 0,
            follow_output: true,
            turn_count: 3,
            total_input_tokens: 10,
            total_output_tokens: 20,
            slash_selection: 0,
            pending_status_index: None,
            pending_assistant_index: None,
            worker: QueryWorkerHandle::stub(),
            should_quit: false,
        }
    }

    #[tokio::test]
    async fn assistant_text_deltas_append_to_same_item() {
        let mut app = test_app();
        app.handle_worker_event(WorkerEvent::TextDelta("hel".to_string()));
        app.handle_worker_event(WorkerEvent::TextDelta("lo".to_string()));

        assert_eq!(app.transcript.len(), 1);
        assert_eq!(app.transcript[0].kind, TranscriptItemKind::Assistant);
        assert_eq!(app.transcript[0].body, "hello");
    }

    #[tokio::test]
    async fn tool_results_create_separate_items() {
        let mut app = test_app();
        app.handle_worker_event(WorkerEvent::ToolResult {
            preview: "done".to_string(),
            is_error: false,
            truncated: false,
        });

        assert_eq!(app.transcript.len(), 1);
        assert_eq!(app.transcript[0].kind, TranscriptItemKind::ToolResult);
        assert_eq!(app.transcript[0].body, "done");
    }

    #[tokio::test]
    async fn slash_status_adds_system_transcript_item() {
        let mut app = test_app();

        app.handle_slash_command("/status".to_string())
            .expect("status command should succeed");

        assert_eq!(
            app.transcript,
            vec![TranscriptItem::new(
                TranscriptItemKind::System,
                "Status",
                "turns: 3\nmodel: test-model\ntokens: 10 in / 20 out\nbusy: false",
            )]
        );
    }

    #[tokio::test]
    async fn slash_exit_requests_shutdown() {
        let mut app = test_app();

        app.handle_slash_command("/exit".to_string())
            .expect("exit command should succeed");

        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn slash_completion_applies_selected_command() {
        let mut app = test_app();
        app.input.replace("/e");

        assert!(app.try_apply_slash_suggestion());
        assert_eq!(app.input.text(), "/exit");
    }

    #[tokio::test]
    async fn tool_call_breaks_assistant_stream_into_new_segment() {
        let mut app = test_app();
        app.handle_worker_event(WorkerEvent::TextDelta("before".to_string()));
        app.handle_worker_event(WorkerEvent::ToolCall {
            summary: "Ran date".to_string(),
            detail: Some("{\n  \"command\": \"date\"\n}".to_string()),
        });
        app.handle_worker_event(WorkerEvent::TextDelta("after".to_string()));

        assert_eq!(
            app.transcript,
            vec![
                TranscriptItem::new(TranscriptItemKind::Assistant, "Assistant", "before"),
                TranscriptItem::new(
                    TranscriptItemKind::ToolCall,
                    "Ran date",
                    "{\n  \"command\": \"date\"\n}"
                ),
                TranscriptItem::new(TranscriptItemKind::Assistant, "Assistant", "after"),
            ]
        );
    }

    #[tokio::test]
    async fn submit_prompt_inserts_status_line_below_user_message() {
        let mut app = test_app();

        app.submit_prompt("hello".to_string())
            .expect("submit should succeed");

        assert_eq!(
            app.transcript,
            vec![
                TranscriptItem::new(TranscriptItemKind::User, "You", "hello"),
                TranscriptItem::new(TranscriptItemKind::System, "Thinking", ""),
            ]
        );
    }
}
