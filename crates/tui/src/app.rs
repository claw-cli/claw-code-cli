use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;

use crate::{
    events::{TranscriptItem, TranscriptItemKind, WorkerEvent},
    input::InputBuffer,
    render,
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
    /// Human-readable provider name shown in the header.
    pub(crate) provider_name: String,
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
    /// Most recent per-turn token usage summary.
    pub(crate) last_turn_usage: Option<(usize, usize)>,
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
            system_prompt: config.system_prompt,
            max_turns: config.max_turns,
            permission_mode: config.permission_mode,
            cwd: config.cwd.clone(),
            provider: config.provider,
        });

        let mut app = Self {
            provider_name: config.provider_name,
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
            last_turn_usage: None,
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
                let prompt = self.input.take();
                if let Err(error) = self.submit_prompt(prompt) {
                    self.push_item(
                        TranscriptItemKind::Error,
                        "Submit failed",
                        error.to_string(),
                    );
                    self.status_message = "Failed to submit prompt".to_string();
                }
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => {
                self.input.move_end();
                self.follow_output = true;
            }
            KeyCode::Up => {
                self.follow_output = false;
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.follow_output = false;
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.follow_output = false;
                self.scroll = self.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.follow_output = false;
                self.scroll = self.scroll.saturating_add(10);
            }
            KeyCode::Esc => self.input.clear(),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.insert_char(ch);
            }
            _ => {}
        }
    }

    fn submit_prompt(&mut self, prompt: String) -> Result<()> {
        if self.input.is_blank() && prompt.trim().is_empty() {
            return Ok(());
        }

        self.push_item(TranscriptItemKind::User, "You", prompt.clone());
        self.follow_output = true;
        self.busy = true;
        self.pending_assistant_index = None;
        self.status_message = "Waiting for model response".to_string();
        self.worker.submit_prompt(prompt)
    }

    fn handle_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::TurnStarted => {
                self.busy = true;
                self.status_message = "Thinking".to_string();
                self.pending_assistant_index = None;
            }
            WorkerEvent::TextDelta(text) => {
                let index = self.ensure_assistant_item();
                self.transcript[index].body.push_str(&text);
                self.status_message = "Streaming response".to_string();
            }
            WorkerEvent::ToolCall { name } => {
                self.push_item(
                    TranscriptItemKind::ToolCall,
                    format!("Tool: {name}"),
                    "Running tool".to_string(),
                );
                self.status_message = format!("Running tool {name}");
            }
            WorkerEvent::ToolResult { content, is_error } => {
                let kind = if is_error {
                    TranscriptItemKind::Error
                } else {
                    TranscriptItemKind::ToolResult
                };
                let title = if is_error {
                    "Tool error"
                } else {
                    "Tool result"
                };
                self.push_item(kind, title, content);
                self.status_message = if is_error {
                    "Tool returned an error".to_string()
                } else {
                    "Tool completed".to_string()
                };
            }
            WorkerEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                self.last_turn_usage = Some((input_tokens, output_tokens));
            }
            WorkerEvent::TurnFinished {
                stop_reason,
                turn_count,
                total_input_tokens,
                total_output_tokens,
            } => {
                self.busy = false;
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
    ) {
        self.transcript.push(TranscriptItem::new(kind, title, body));
        if self.follow_output {
            self.scroll = 0;
        }
    }
}
