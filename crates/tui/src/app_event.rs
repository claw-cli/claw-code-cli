//! Application-level events for the Claw v2 TUI.
//!
use std::path::PathBuf;

use crate::app_command::AppCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectorsSnapshot {
    pub(crate) connectors: Vec<ConnectorInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectorInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) is_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppEvent {
    /// Request a redraw on the next frame.
    Redraw,

    /// Request to exit the TUI.
    Exit(ExitMode),

    /// Submit the current composer text.
    SubmitUserInput { text: String },

    /// Send a command request to the host/worker adapter.
    Command(AppCommand),

    #[allow(dead_code)]
    /// Interrupt the current turn or cancel the active UI surface.
    Interrupt,

    #[allow(dead_code)]
    /// Clear the visible transcript.
    ClearTranscript,

    #[allow(dead_code)]
    /// Open the slash command popup.
    OpenSlashCommandPopup,

    #[allow(dead_code)]
    /// Close the currently active popup or transient view.
    ClosePopup,

    #[allow(dead_code)]
    /// Execute a slash command selected or typed by the user.
    RunSlashCommand { command: String },

    #[allow(dead_code)]
    /// Open the model picker.
    OpenModelPicker,

    #[allow(dead_code)]
    /// Apply a selected model.
    ModelSelected { model: String },

    #[allow(dead_code)]
    /// Open the thinking-mode picker.
    OpenThinkingPicker,

    #[allow(dead_code)]
    /// Apply a selected thinking mode.
    ThinkingSelected { value: Option<String> },

    #[allow(dead_code)]
    /// Async update of the current git branch for status-line rendering.
    StatusLineBranchUpdated {
        cwd: PathBuf,
        branch: Option<String>,
    },

    /// Request a file-search refresh for composer mention popups.
    StartFileSearch(String),

    /// Request a persistent composer-history entry by absolute log offset.
    HistoryEntryRequested { log_id: u64, offset: usize },

    /// Replace the current status message.
    StatusMessageChanged { message: String },

    #[allow(dead_code)]
    /// Apply a user-confirmed status-line item ordering/selection.
    StatusLineSetup { items: Vec<StatusLineItem> },

    #[allow(dead_code)]
    /// Dismiss the status-line setup UI without changing config.
    StatusLineSetupCancelled,

    #[allow(dead_code)]
    /// Apply a user-confirmed terminal-title item ordering/selection.
    TerminalTitleSetup { items: Vec<TerminalTitleItem> },

    #[allow(dead_code)]
    /// Apply a temporary terminal-title preview while the setup UI is open.
    TerminalTitleSetupPreview { items: Vec<TerminalTitleItem> },

    #[allow(dead_code)]
    /// Dismiss the terminal-title setup UI without changing config.
    TerminalTitleSetupCancelled,

    #[allow(dead_code)]
    /// Open the theme picker.
    OpenThemePicker,
    #[allow(dead_code)]
    /// Apply a selected theme.
    ThemeSelected { name: String },
    /// Result of computing a `/diff` command (ANSI-colored diff text).
    DiffResult(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitMode {
    /// Let the host perform orderly shutdown before exiting.
    ShutdownFirst,
    #[allow(dead_code)]
    /// Exit the UI loop immediately.
    Immediate,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusLineItem {
    Model,
    Tokens,
    CurrentDir,
    Custom(String),
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalTitleItem {
    Project,
    Model,
    Spinner,
    Custom(String),
}
