//! Interactive terminal UI for ClawCR.

mod app;
mod events;
mod input;
mod render;
mod terminal;
mod worker;

use std::path::PathBuf;

use anyhow::Result;
use clawcr_provider::ModelProvider;
use clawcr_safety::legacy_permissions::PermissionMode;

pub use app::AppExit;

/// Immutable configuration used to launch the interactive terminal UI.
pub struct InteractiveTuiConfig {
    /// Human-readable provider name displayed in the header.
    pub provider_name: String,
    /// Model identifier used for requests and shown in the header.
    pub model: String,
    /// System prompt supplied to the query loop.
    pub system_prompt: String,
    /// Maximum number of turns allowed in the interactive session.
    pub max_turns: usize,
    /// Permission mode used by tool execution.
    pub permission_mode: PermissionMode,
    /// Working directory shown in the header and passed to the session.
    pub cwd: PathBuf,
    /// Provider instance used for model requests.
    pub provider: Box<dyn ModelProvider>,
    /// Optional prompt submitted immediately after the UI opens.
    pub startup_prompt: Option<String>,
}

/// Runs the interactive alternate-screen terminal UI until the user exits.
pub async fn run_interactive_tui(config: InteractiveTuiConfig) -> Result<AppExit> {
    app::TuiApp::run(config).await
}
