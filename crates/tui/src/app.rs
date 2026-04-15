use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use clawcr_core::PresetModelCatalog;
use clawcr_protocol::ProviderFamily;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::{
    events::{
        ModelListEntry, SavedModelEntry, SessionListEntry, ThinkingListEntry, TranscriptItem,
        TranscriptItemKind, WorkerEvent,
    },
    input::InputBuffer,
    onboarding::save_onboarding_config,
    paste_burst::PasteBurst,
    render,
    slash::{SlashCommandSpec, matching_slash_commands},
    terminal::{ManagedTerminal, TerminalMode},
    worker::{QueryWorkerConfig, QueryWorkerHandle},
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

/// Temporary auxiliary panel rendered below the composer for non-transcript information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuxPanel {
    /// Short title shown above the panel body.
    pub(crate) title: String,
    /// Structured panel content rendered below the composer.
    pub(crate) content: AuxPanelContent,
}

/// One supported content shape for the temporary auxiliary bottom panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuxPanelContent {
    /// Plain informational text for commands like `/model` and `/status`.
    Text(String),
    /// Selectable session list shown after `/sessions`.
    SessionList(Vec<SessionListEntry>),
    /// Selectable model list shown after `/model` or onboarding.
    ModelList(Vec<ModelListEntry>),
    /// Selectable thinking-mode list shown after `/thinking`.
    ThinkingList(Vec<ThinkingListEntry>),
}

/// In-memory application state for the interactive terminal UI.
pub(crate) struct TuiApp {
    /// Model identifier shown in the header.
    pub(crate) model: String,
    /// Provider family currently driving the active session.
    pub(crate) provider: ProviderFamily,
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
    /// Temporary auxiliary panel rendered below the composer, when visible.
    pub(crate) aux_panel: Option<AuxPanel>,
    /// Selected session row when the session picker panel is visible.
    pub(crate) aux_panel_selection: usize,
    /// Current thinking selection applied to the active session.
    pub(crate) thinking_selection: Option<String>,
    /// Index of the current turn status line rendered below the latest user message.
    pub(crate) pending_status_index: Option<usize>,
    /// Index of the assistant transcript item currently receiving streamed text.
    pub(crate) pending_assistant_index: Option<usize>,
    /// Index of the reasoning transcript item currently receiving streamed text.
    pub(crate) pending_reasoning_index: Option<usize>,
    /// Map from tool call id to the transcript row that should be updated with the result.
    pub(crate) pending_tool_items: HashMap<String, usize>,
    /// Background query worker owned by the UI.
    pub(crate) worker: QueryWorkerHandle,
    /// Built-in model catalog used for onboarding and model selection.
    pub(crate) model_catalog: PresetModelCatalog,
    /// Persisted model entries available for switching in the composer popup.
    pub(crate) saved_models: Vec<SavedModelEntry>,
    /// Whether the app should open the model picker on startup.
    pub(crate) show_model_onboarding: bool,
    /// Whether onboarding completion has already been announced.
    pub(crate) onboarding_announced: bool,
    /// Whether the onboarding flow is waiting for a manually typed custom model.
    pub(crate) onboarding_custom_model_pending: bool,
    /// Prompt shown while onboarding is collecting connection details.
    pub(crate) onboarding_prompt: Option<String>,
    /// Completed onboarding prompt lines preserved in the transcript area.
    pub(crate) onboarding_prompt_history: Vec<String>,
    /// Whether the onboarding flow is waiting for a base URL input.
    pub(crate) onboarding_base_url_pending: bool,
    /// Whether the onboarding flow is waiting for an API key input.
    pub(crate) onboarding_api_key_pending: bool,
    /// Model selected during onboarding before credentials are finalized.
    pub(crate) onboarding_selected_model: Option<String>,
    /// Whether the selected onboarding model came from manual entry.
    pub(crate) onboarding_selected_model_is_custom: bool,
    /// Base URL entered during onboarding before it is applied.
    pub(crate) onboarding_selected_base_url: Option<String>,
    /// API key entered during onboarding before it is applied.
    pub(crate) onboarding_selected_api_key: Option<String>,
    /// Timestamp of the most recent Ctrl+C press used for interrupt/exit confirmation.
    pub(crate) last_ctrl_c_at: Option<Instant>,
    /// Buffered rapid keypresses that should be applied as one pasted string.
    pub(crate) paste_burst: PasteBurst,
    /// Whether the app should exit after the current loop iteration.
    pub(crate) should_quit: bool,
    /// Whether the UI is rendering inline in the main terminal buffer.
    pub(crate) inline_mode: bool,
    /// Last known terminal width, used for inline transcript wrapping.
    pub(crate) terminal_width: u16,
    /// Whether an inline assistant stream is currently open.
    pub(crate) inline_assistant_stream_open: bool,
    /// Unflushed trailing assistant text for inline scrollback streaming.
    pub(crate) inline_assistant_pending_line: String,
    /// Whether the current inline assistant stream has already emitted its header.
    pub(crate) inline_assistant_header_emitted: bool,
    /// Pending inline transcript blocks waiting to be inserted above the viewport.
    pub(crate) pending_inline_history: Vec<String>,
}

/// Immutable configuration used to launch the interactive terminal UI.
pub struct InteractiveTuiConfig {
    /// Model identifier used for requests and shown in the header.
    pub model: String,
    /// Provider family used for requests and shown in the picker.
    pub provider: ProviderFamily,
    /// Working directory shown in the header and passed to the session.
    pub cwd: PathBuf,
    /// Environment overrides applied to the spawned stdio server process.
    pub server_env: Vec<(String, String)>,
    /// Optional CLI log-level override to forward to the spawned server process.
    pub server_log_level: Option<String>,
    /// Built-in model catalog used for onboarding and model selection.
    pub model_catalog: PresetModelCatalog,
    /// Persisted model entries available for switching in the composer popup.
    pub saved_models: Vec<SavedModelEntry>,
    /// Whether to open the model picker on startup.
    pub show_model_onboarding: bool,
    /// Terminal screen strategy used when launching the UI.
    pub terminal_mode: TerminalMode,
}

#[path = "runtime.rs"]
mod runtime;
#[path = "selection.rs"]
mod selection;
#[path = "worker_events.rs"]
mod worker_events;

/// Runs the interactive alternate-screen terminal UI until the user exits.
pub async fn run_interactive_tui(config: InteractiveTuiConfig) -> Result<AppExit> {
    TuiApp::run(config).await
}
