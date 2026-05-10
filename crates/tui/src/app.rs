use std::path::PathBuf;

use crate::events::SavedModelEntry;
use devo_core::PermissionPreset;
use devo_core::PresetModelCatalog;
use devo_core::ProviderWireApi;

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

/// Public startup request passed from the CLI into the TUI crate.
///
/// This type intentionally carries config-shaped values: a model slug, provider fallback,
/// thinking selection, and cwd. `host` resolves the model slug against the catalog before
/// constructing the chat widget's runtime session state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialTuiSession {
    /// Model identifier used for the first requests and initial UI projection.
    pub model: String,
    /// Provider family used for the initial runtime connection and picker fallback.
    pub provider: ProviderWireApi,
    /// Initial thinking selection restored from persisted config.
    pub thinking_selection: Option<String>,
    /// Initial permission preset restored from project-level config.
    pub permission_preset: PermissionPreset,
    /// Working directory used for the initial session.
    pub cwd: PathBuf,
}

/// Runtime wiring used to launch the interactive terminal UI.
pub struct InteractiveTuiConfig {
    /// Initial session request resolved by the host before it reaches internal widgets.
    pub initial_session: InitialTuiSession,
    /// Optional CLI log-level override to forward to the spawned server process.
    pub server_log_level: Option<String>,
    /// Built-in model catalog used for onboarding and model selection.
    pub model_catalog: PresetModelCatalog,
    /// Persisted model entries available for switching in the composer popup.
    pub saved_models: Vec<SavedModelEntry>,
    /// Whether to open the model picker on startup.
    pub show_model_onboarding: bool,
}
