use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use clawcr_utils::FileSystemConfigPathResolver;
use serde::{Deserialize, Serialize};

use crate::model::{ModelCatalog, ModelConfig, ModelConfigError};

/// Stores the fully normalized runtime configuration consumed by the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    /// The default model slug used when a turn does not request one explicitly.
    pub default_model: Option<String>,
    /// The policy that selects which model should generate context summaries.
    pub summary_model: SummaryModelSelection,
    /// Context-window and compaction defaults.
    pub context: ContextConfig,
    /// Conversation and session-title defaults.
    pub conversation: ConversationConfig,
    /// Safety and policy-model defaults.
    pub safety: SafetyConfig,
    /// Runtime enablement and execution limits for built-in tools.
    pub tools: ToolRuntimeConfig,
    /// Transport and server runtime defaults.
    pub server: ServerConfig,
    /// Logging and redaction behavior for diagnostics.
    pub logging: LoggingConfig,
    /// Marker names used when discovering a project root.
    pub project_root_markers: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            default_model: None,
            summary_model: SummaryModelSelection::UseTurnModel,
            context: ContextConfig {
                preserve_recent_turns: 3,
                auto_compact_percent: Some(90),
                manual_compaction_enabled: true,
                snapshot_backend: SnapshotBackendMode::JsonOnly,
            },
            conversation: ConversationConfig {
                session_titles: SessionTitleConfig {
                    mode: SessionTitleMode::DeriveThenGenerate,
                    generate_async: true,
                    generation_model: TitleModelSelection::UseTurnModel,
                    max_title_chars: 80,
                },
            },
            safety: SafetyConfig {
                policy_model: PolicyModelSelection::UseTurnModel,
            },
            tools: ToolRuntimeConfig {
                enabled_tools: vec![
                    "shell_command".into(),
                    "file_search".into(),
                    "read_file".into(),
                    "apply_patch".into(),
                ],
                shell: ShellToolConfig {
                    default_timeout_ms: 60_000,
                    max_timeout_ms: 300_000,
                    stream_output: true,
                    max_stdout_bytes: 128 * 1024,
                    max_stderr_bytes: 128 * 1024,
                },
                file_search: FileSearchToolConfig {
                    prefer_rg: true,
                    max_results: 200,
                    max_preview_bytes: 4 * 1024,
                },
                max_parallel_read_tools: 4,
            },
            server: ServerConfig {
                listen: Vec::new(),
                max_connections: 32,
                event_buffer_size: 1024,
                idle_session_timeout_secs: 1800,
                persist_ephemeral_sessions: false,
            },
            logging: LoggingConfig {
                level: "info".into(),
                json: false,
                redact_secrets_in_logs: true,
            },
            project_root_markers: vec![".git".into()],
        }
    }
}

/// Stores defaults for context preservation and compaction behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextConfig {
    /// The number of most recent turns that must remain un-compacted.
    pub preserve_recent_turns: u32,
    /// The percentage threshold that triggers automatic compaction.
    pub auto_compact_percent: Option<u8>,
    /// Whether the runtime should allow manual compaction requests.
    pub manual_compaction_enabled: bool,
    /// The snapshot backend used when compaction persists recovery metadata.
    pub snapshot_backend: SnapshotBackendMode,
}

/// Stores conversation-scoped runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationConfig {
    /// The policy used to derive and upgrade session titles.
    pub session_titles: SessionTitleConfig,
}

/// Stores safety-scoped runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// The policy that selects which model should perform model-guided safety classification.
    pub policy_model: PolicyModelSelection,
}

/// Stores runtime configuration for the built-in tool subsystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRuntimeConfig {
    /// The stable names of tools enabled for runtime exposure.
    pub enabled_tools: Vec<String>,
    /// Shell-command execution limits and defaults.
    pub shell: ShellToolConfig,
    /// File-search execution limits and defaults.
    pub file_search: FileSearchToolConfig,
    /// The maximum number of read-only tools that may execute concurrently.
    pub max_parallel_read_tools: u16,
}

/// Stores defaults and limits for shell-command execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellToolConfig {
    /// The default timeout applied when a shell tool call omits one.
    pub default_timeout_ms: u64,
    /// The maximum timeout the runtime will allow for a shell tool call.
    pub max_timeout_ms: u64,
    /// Whether stdout and stderr should be streamed incrementally.
    pub stream_output: bool,
    /// The maximum number of stdout bytes retained in the normalized result.
    pub max_stdout_bytes: usize,
    /// The maximum number of stderr bytes retained in the normalized result.
    pub max_stderr_bytes: usize,
}

/// Stores defaults and limits for file-search execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSearchToolConfig {
    /// Whether the runtime should prefer `rg` when available.
    pub prefer_rg: bool,
    /// The maximum number of search matches returned by one invocation.
    pub max_results: u32,
    /// The maximum number of bytes retained per result preview.
    pub max_preview_bytes: usize,
}

/// Stores transport and connection-management defaults for the runtime server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// The listener addresses the server should bind to by default.
    pub listen: Vec<String>,
    /// The maximum number of simultaneous client connections.
    pub max_connections: u32,
    /// The per-connection event buffer size used for streaming notifications.
    pub event_buffer_size: usize,
    /// The idle timeout applied to loaded sessions, in seconds.
    pub idle_session_timeout_secs: u64,
    /// Whether ephemeral sessions should be persisted despite their transient nature.
    pub persist_ephemeral_sessions: bool,
}

/// Selects the model used for summary generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SummaryModelSelection {
    /// Use the active turn model for compaction summaries.
    UseTurnModel,
    /// Use a separately configured model slug for compaction summaries.
    UseConfiguredModel { model_slug: String },
}

/// Selects the backend used to persist compaction snapshot metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotBackendMode {
    /// Persist only JSON snapshot metadata.
    JsonOnly,
    /// Prefer a git-backed ghost snapshot but fall back to JSON-only when needed.
    PreferGitGhostCommit,
    /// Require a git-backed ghost snapshot in addition to JSON metadata.
    RequireGitGhostCommit,
}

/// Stores the policy used to derive and finalize session titles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleConfig {
    /// The overall session-title generation mode.
    pub mode: SessionTitleMode,
    /// Whether final title generation should run asynchronously after the first turn.
    pub generate_async: bool,
    /// The policy that selects the model used for generated titles.
    pub generation_model: TitleModelSelection,
    /// The maximum visible character length allowed for generated titles.
    pub max_title_chars: u16,
}

/// Controls when automatic title generation is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionTitleMode {
    /// Disable all automatic title derivation and generation.
    ExplicitOnly,
    /// Derive a provisional title first and optionally upgrade it later.
    DeriveThenGenerate,
}

/// Selects the model used for title generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitleModelSelection {
    /// Use the active turn model for title generation.
    UseTurnModel,
    /// Use a separately configured model slug for title generation.
    UseConfiguredModel { model_slug: String },
}

/// Selects the model used for model-guided safety policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyModelSelection {
    /// Use the active turn model for policy classification.
    UseTurnModel,
    /// Use a separately configured model slug for policy classification.
    UseConfiguredModel { model_slug: String },
}

/// Stores logging defaults for the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// The default logging level string.
    pub level: String,
    /// Whether logs should be emitted in JSON format.
    pub json: bool,
    /// Whether secrets should be redacted from logs before emission.
    pub redact_secrets_in_logs: bool,
}

/// Describes one config layer discovered during loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLayerEntry {
    /// The source location or source kind for the layer.
    pub source: ConfigSource,
    /// A short version label for diagnostics.
    pub version: String,
    /// The reason the layer was ignored, if it was discovered but disabled.
    pub disabled_reason: Option<String>,
}

/// Identifies where one config layer originated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// The built-in defaults compiled into the binary.
    BuiltIn,
    /// The user-level config file.
    User { file: PathBuf },
    /// The project-level `.clawcr` config directory.
    Project { dot_clawcr_folder: PathBuf },
    /// A synthetic layer produced by CLI overrides.
    CliOverrides,
}

/// Loads the effective application configuration from filesystem-backed layers.
pub trait AppConfigLoader {
    /// Loads and validates the effective application config for an optional workspace.
    fn load(&self, workspace_root: Option<&Path>) -> Result<AppConfig, AppConfigError>;
}

/// Lists the config layers that were discovered for one load operation.
pub trait AppConfigLayerLoader {
    /// Returns metadata describing the config layers visible for an optional workspace.
    fn load_layers(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<Vec<ConfigLayerEntry>, AppConfigError>;
}

/// Resolves model-selection policies embedded inside `AppConfig`.
pub trait AppConfigResolver {
    /// Resolves the model used for context summarization.
    fn resolve_summary_model<'a>(
        &'a self,
        app_config: &'a AppConfig,
        turn_model: &'a ModelConfig,
        catalog: &'a dyn ModelCatalog,
    ) -> Result<&'a ModelConfig, AppConfigError>;

    /// Resolves the model used for generated session titles.
    fn resolve_title_model<'a>(
        &'a self,
        app_config: &'a AppConfig,
        turn_model: &'a ModelConfig,
        catalog: &'a dyn ModelCatalog,
    ) -> Result<&'a ModelConfig, AppConfigError>;

    /// Resolves the model used for model-guided safety policy evaluation.
    fn resolve_policy_model<'a>(
        &'a self,
        app_config: &'a AppConfig,
        turn_model: &'a ModelConfig,
        catalog: &'a dyn ModelCatalog,
    ) -> Result<&'a ModelConfig, AppConfigError>;
}

/// Filesystem-backed loader for user and project application config files.
#[derive(Debug, Clone)]
pub struct FileSystemAppConfigLoader {
    /// The home directory used to locate the user-level `.clawcr/config.toml`.
    user_home: PathBuf,
}

impl FileSystemAppConfigLoader {
    /// Creates a filesystem-backed loader rooted at the provided user home directory.
    pub fn new(user_home: PathBuf) -> Self {
        Self { user_home }
    }

    fn user_config_path(&self) -> PathBuf {
        FileSystemConfigPathResolver::new(self.user_home.clone()).user_config_file()
    }

    fn project_config_path(&self, workspace_root: &Path) -> PathBuf {
        FileSystemConfigPathResolver::new(self.user_home.clone())
            .project_config_file(workspace_root)
    }
}

impl AppConfigLoader for FileSystemAppConfigLoader {
    fn load(&self, workspace_root: Option<&Path>) -> Result<AppConfig, AppConfigError> {
        let mut merged = RawAppConfig::default();

        let user_path = self.user_config_path();
        if user_path.exists() {
            merged.merge(read_raw_config(&user_path)?);
        }

        if let Some(workspace_root) = workspace_root {
            let project_path = self.project_config_path(workspace_root);
            if project_path.exists() {
                merged.merge(read_raw_config(&project_path)?);
            }
        }

        merged.into_app_config()
    }
}

impl AppConfigLayerLoader for FileSystemAppConfigLoader {
    fn load_layers(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<Vec<ConfigLayerEntry>, AppConfigError> {
        let mut layers = vec![ConfigLayerEntry {
            source: ConfigSource::BuiltIn,
            version: "builtin".into(),
            disabled_reason: None,
        }];

        let user_path = self.user_config_path();
        if user_path.exists() {
            layers.push(ConfigLayerEntry {
                source: ConfigSource::User { file: user_path },
                version: "user".into(),
                disabled_reason: None,
            });
        }

        if let Some(workspace_root) = workspace_root {
            let project_path = self.project_config_path(workspace_root);
            if project_path.exists() {
                layers.push(ConfigLayerEntry {
                    source: ConfigSource::Project {
                        dot_clawcr_folder: project_path
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| workspace_root.to_path_buf()),
                    },
                    version: "project".into(),
                    disabled_reason: None,
                });
            }
        }

        Ok(layers)
    }
}

/// Default resolver that interprets each model-selection enum directly.
#[derive(Debug, Clone, Default)]
pub struct DefaultAppConfigResolver;

impl AppConfigResolver for DefaultAppConfigResolver {
    fn resolve_summary_model<'a>(
        &'a self,
        app_config: &'a AppConfig,
        turn_model: &'a ModelConfig,
        catalog: &'a dyn ModelCatalog,
    ) -> Result<&'a ModelConfig, AppConfigError> {
        resolve_model_selection(&app_config.summary_model, turn_model, catalog)
    }

    fn resolve_title_model<'a>(
        &'a self,
        app_config: &'a AppConfig,
        turn_model: &'a ModelConfig,
        catalog: &'a dyn ModelCatalog,
    ) -> Result<&'a ModelConfig, AppConfigError> {
        resolve_model_selection(
            &app_config.conversation.session_titles.generation_model,
            turn_model,
            catalog,
        )
    }

    fn resolve_policy_model<'a>(
        &'a self,
        app_config: &'a AppConfig,
        turn_model: &'a ModelConfig,
        catalog: &'a dyn ModelCatalog,
    ) -> Result<&'a ModelConfig, AppConfigError> {
        resolve_model_selection(&app_config.safety.policy_model, turn_model, catalog)
    }
}

fn resolve_model_selection<'a, T>(
    selection: &T,
    turn_model: &'a ModelConfig,
    catalog: &'a dyn ModelCatalog,
) -> Result<&'a ModelConfig, AppConfigError>
where
    T: ModelSelectionView,
{
    match selection.model_slug() {
        None => Ok(turn_model),
        Some(model_slug) => catalog
            .get(model_slug)
            .ok_or_else(|| AppConfigError::ModelLookup {
                slug: model_slug.to_string(),
            }),
    }
}

trait ModelSelectionView {
    fn model_slug(&self) -> Option<&str>;
}

impl ModelSelectionView for SummaryModelSelection {
    fn model_slug(&self) -> Option<&str> {
        match self {
            SummaryModelSelection::UseTurnModel => None,
            SummaryModelSelection::UseConfiguredModel { model_slug } => Some(model_slug.as_str()),
        }
    }
}

impl ModelSelectionView for TitleModelSelection {
    fn model_slug(&self) -> Option<&str> {
        match self {
            TitleModelSelection::UseTurnModel => None,
            TitleModelSelection::UseConfiguredModel { model_slug } => Some(model_slug.as_str()),
        }
    }
}

impl ModelSelectionView for PolicyModelSelection {
    fn model_slug(&self) -> Option<&str> {
        match self {
            PolicyModelSelection::UseTurnModel => None,
            PolicyModelSelection::UseConfiguredModel { model_slug } => Some(model_slug.as_str()),
        }
    }
}

/// Enumerates failures that can occur while loading or validating app config.
#[derive(Debug, thiserror::Error)]
pub enum AppConfigError {
    /// Reading a config file from disk failed.
    #[error("config IO failed at {path}: {source}")]
    Io {
        /// The config path that failed to read.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Parsing TOML into the config schema failed.
    #[error("config parse failed at {path}: {message}")]
    Parse { path: PathBuf, message: String },
    /// Cross-field validation rejected the normalized config.
    #[error("invalid app config: {message}")]
    Validation { message: String },
    /// A configured model slug could not be found in the model catalog.
    #[error("referenced model not found: {slug}")]
    ModelLookup { slug: String },
    /// A lower-level model-catalog resolution error occurred.
    #[error(transparent)]
    Model(#[from] ModelConfigError),
}

fn read_raw_config(path: &Path) -> Result<RawAppConfig, AppConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| AppConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    toml::from_str::<RawAppConfig>(&contents).map_err(|source| AppConfigError::Parse {
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawAppConfig {
    default_model: Option<Option<String>>,
    summary_model: Option<SummaryModelSelection>,
    context: Option<RawContextConfig>,
    conversation: Option<RawConversationConfig>,
    safety: Option<RawSafetyConfig>,
    tools: Option<RawToolRuntimeConfig>,
    server: Option<RawServerConfig>,
    logging: Option<RawLoggingConfig>,
    project_root_markers: Option<Vec<String>>,
}

impl RawAppConfig {
    fn merge(&mut self, other: Self) {
        if other.default_model.is_some() {
            self.default_model = other.default_model;
        }
        if other.summary_model.is_some() {
            self.summary_model = other.summary_model;
        }
        merge_optional(&mut self.context, other.context);
        merge_optional(&mut self.conversation, other.conversation);
        merge_optional(&mut self.safety, other.safety);
        merge_optional(&mut self.tools, other.tools);
        merge_optional(&mut self.server, other.server);
        merge_optional(&mut self.logging, other.logging);
        if other.project_root_markers.is_some() {
            self.project_root_markers = other.project_root_markers;
        }
    }

    fn into_app_config(self) -> Result<AppConfig, AppConfigError> {
        let defaults = AppConfig::default();
        let config = AppConfig {
            default_model: self.default_model.unwrap_or(defaults.default_model),
            summary_model: self.summary_model.unwrap_or(defaults.summary_model),
            context: self.context.unwrap_or_default().apply(defaults.context),
            conversation: self
                .conversation
                .unwrap_or_default()
                .apply(defaults.conversation),
            safety: self.safety.unwrap_or_default().apply(defaults.safety),
            tools: self.tools.unwrap_or_default().apply(defaults.tools),
            server: self.server.unwrap_or_default().apply(defaults.server),
            logging: self.logging.unwrap_or_default().apply(defaults.logging),
            project_root_markers: self
                .project_root_markers
                .unwrap_or(defaults.project_root_markers),
        };

        validate_app_config(&config)?;
        Ok(config)
    }
}

fn merge_optional<T: Merge>(slot: &mut Option<T>, incoming: Option<T>) {
    match (slot.as_mut(), incoming) {
        (Some(existing), Some(other)) => existing.merge(other),
        (None, Some(other)) => *slot = Some(other),
        _ => {}
    }
}

trait Merge {
    fn merge(&mut self, other: Self);
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawContextConfig {
    preserve_recent_turns: Option<u32>,
    auto_compact_percent: Option<Option<u8>>,
    manual_compaction_enabled: Option<bool>,
    snapshot_backend: Option<SnapshotBackendMode>,
}

impl Merge for RawContextConfig {
    fn merge(&mut self, other: Self) {
        if other.preserve_recent_turns.is_some() {
            self.preserve_recent_turns = other.preserve_recent_turns;
        }
        if other.auto_compact_percent.is_some() {
            self.auto_compact_percent = other.auto_compact_percent;
        }
        if other.manual_compaction_enabled.is_some() {
            self.manual_compaction_enabled = other.manual_compaction_enabled;
        }
        if other.snapshot_backend.is_some() {
            self.snapshot_backend = other.snapshot_backend;
        }
    }
}

impl RawContextConfig {
    fn apply(self, defaults: ContextConfig) -> ContextConfig {
        ContextConfig {
            preserve_recent_turns: self
                .preserve_recent_turns
                .unwrap_or(defaults.preserve_recent_turns),
            auto_compact_percent: self
                .auto_compact_percent
                .unwrap_or(defaults.auto_compact_percent),
            manual_compaction_enabled: self
                .manual_compaction_enabled
                .unwrap_or(defaults.manual_compaction_enabled),
            snapshot_backend: self.snapshot_backend.unwrap_or(defaults.snapshot_backend),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawConversationConfig {
    session_titles: Option<RawSessionTitleConfig>,
}

impl Merge for RawConversationConfig {
    fn merge(&mut self, other: Self) {
        merge_optional(&mut self.session_titles, other.session_titles);
    }
}

impl RawConversationConfig {
    fn apply(self, defaults: ConversationConfig) -> ConversationConfig {
        ConversationConfig {
            session_titles: self
                .session_titles
                .unwrap_or_default()
                .apply(defaults.session_titles),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawSessionTitleConfig {
    mode: Option<SessionTitleMode>,
    generate_async: Option<bool>,
    generation_model: Option<TitleModelSelection>,
    max_title_chars: Option<u16>,
}

impl Merge for RawSessionTitleConfig {
    fn merge(&mut self, other: Self) {
        if other.mode.is_some() {
            self.mode = other.mode;
        }
        if other.generate_async.is_some() {
            self.generate_async = other.generate_async;
        }
        if other.generation_model.is_some() {
            self.generation_model = other.generation_model;
        }
        if other.max_title_chars.is_some() {
            self.max_title_chars = other.max_title_chars;
        }
    }
}

impl RawSessionTitleConfig {
    fn apply(self, defaults: SessionTitleConfig) -> SessionTitleConfig {
        SessionTitleConfig {
            mode: self.mode.unwrap_or(defaults.mode),
            generate_async: self.generate_async.unwrap_or(defaults.generate_async),
            generation_model: self.generation_model.unwrap_or(defaults.generation_model),
            max_title_chars: self.max_title_chars.unwrap_or(defaults.max_title_chars),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawSafetyConfig {
    policy_model: Option<PolicyModelSelection>,
}

impl Merge for RawSafetyConfig {
    fn merge(&mut self, other: Self) {
        if other.policy_model.is_some() {
            self.policy_model = other.policy_model;
        }
    }
}

impl RawSafetyConfig {
    fn apply(self, defaults: SafetyConfig) -> SafetyConfig {
        SafetyConfig {
            policy_model: self.policy_model.unwrap_or(defaults.policy_model),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawToolRuntimeConfig {
    enabled_tools: Option<Vec<String>>,
    shell: Option<RawShellToolConfig>,
    file_search: Option<RawFileSearchToolConfig>,
    max_parallel_read_tools: Option<u16>,
}

impl Merge for RawToolRuntimeConfig {
    fn merge(&mut self, other: Self) {
        if other.enabled_tools.is_some() {
            self.enabled_tools = other.enabled_tools;
        }
        merge_optional(&mut self.shell, other.shell);
        merge_optional(&mut self.file_search, other.file_search);
        if other.max_parallel_read_tools.is_some() {
            self.max_parallel_read_tools = other.max_parallel_read_tools;
        }
    }
}

impl RawToolRuntimeConfig {
    fn apply(self, defaults: ToolRuntimeConfig) -> ToolRuntimeConfig {
        ToolRuntimeConfig {
            enabled_tools: self.enabled_tools.unwrap_or(defaults.enabled_tools),
            shell: self.shell.unwrap_or_default().apply(defaults.shell),
            file_search: self
                .file_search
                .unwrap_or_default()
                .apply(defaults.file_search),
            max_parallel_read_tools: self
                .max_parallel_read_tools
                .unwrap_or(defaults.max_parallel_read_tools),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawShellToolConfig {
    default_timeout_ms: Option<u64>,
    max_timeout_ms: Option<u64>,
    stream_output: Option<bool>,
    max_stdout_bytes: Option<usize>,
    max_stderr_bytes: Option<usize>,
}

impl Merge for RawShellToolConfig {
    fn merge(&mut self, other: Self) {
        if other.default_timeout_ms.is_some() {
            self.default_timeout_ms = other.default_timeout_ms;
        }
        if other.max_timeout_ms.is_some() {
            self.max_timeout_ms = other.max_timeout_ms;
        }
        if other.stream_output.is_some() {
            self.stream_output = other.stream_output;
        }
        if other.max_stdout_bytes.is_some() {
            self.max_stdout_bytes = other.max_stdout_bytes;
        }
        if other.max_stderr_bytes.is_some() {
            self.max_stderr_bytes = other.max_stderr_bytes;
        }
    }
}

impl RawShellToolConfig {
    fn apply(self, defaults: ShellToolConfig) -> ShellToolConfig {
        ShellToolConfig {
            default_timeout_ms: self
                .default_timeout_ms
                .unwrap_or(defaults.default_timeout_ms),
            max_timeout_ms: self.max_timeout_ms.unwrap_or(defaults.max_timeout_ms),
            stream_output: self.stream_output.unwrap_or(defaults.stream_output),
            max_stdout_bytes: self.max_stdout_bytes.unwrap_or(defaults.max_stdout_bytes),
            max_stderr_bytes: self.max_stderr_bytes.unwrap_or(defaults.max_stderr_bytes),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawFileSearchToolConfig {
    prefer_rg: Option<bool>,
    max_results: Option<u32>,
    max_preview_bytes: Option<usize>,
}

impl Merge for RawFileSearchToolConfig {
    fn merge(&mut self, other: Self) {
        if other.prefer_rg.is_some() {
            self.prefer_rg = other.prefer_rg;
        }
        if other.max_results.is_some() {
            self.max_results = other.max_results;
        }
        if other.max_preview_bytes.is_some() {
            self.max_preview_bytes = other.max_preview_bytes;
        }
    }
}

impl RawFileSearchToolConfig {
    fn apply(self, defaults: FileSearchToolConfig) -> FileSearchToolConfig {
        FileSearchToolConfig {
            prefer_rg: self.prefer_rg.unwrap_or(defaults.prefer_rg),
            max_results: self.max_results.unwrap_or(defaults.max_results),
            max_preview_bytes: self.max_preview_bytes.unwrap_or(defaults.max_preview_bytes),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawServerConfig {
    listen: Option<Vec<String>>,
    max_connections: Option<u32>,
    event_buffer_size: Option<usize>,
    idle_session_timeout_secs: Option<u64>,
    persist_ephemeral_sessions: Option<bool>,
}

impl Merge for RawServerConfig {
    fn merge(&mut self, other: Self) {
        if other.listen.is_some() {
            self.listen = other.listen;
        }
        if other.max_connections.is_some() {
            self.max_connections = other.max_connections;
        }
        if other.event_buffer_size.is_some() {
            self.event_buffer_size = other.event_buffer_size;
        }
        if other.idle_session_timeout_secs.is_some() {
            self.idle_session_timeout_secs = other.idle_session_timeout_secs;
        }
        if other.persist_ephemeral_sessions.is_some() {
            self.persist_ephemeral_sessions = other.persist_ephemeral_sessions;
        }
    }
}

impl RawServerConfig {
    fn apply(self, defaults: ServerConfig) -> ServerConfig {
        ServerConfig {
            listen: self.listen.unwrap_or(defaults.listen),
            max_connections: self.max_connections.unwrap_or(defaults.max_connections),
            event_buffer_size: self.event_buffer_size.unwrap_or(defaults.event_buffer_size),
            idle_session_timeout_secs: self
                .idle_session_timeout_secs
                .unwrap_or(defaults.idle_session_timeout_secs),
            persist_ephemeral_sessions: self
                .persist_ephemeral_sessions
                .unwrap_or(defaults.persist_ephemeral_sessions),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawLoggingConfig {
    level: Option<String>,
    json: Option<bool>,
    redact_secrets_in_logs: Option<bool>,
}

impl Merge for RawLoggingConfig {
    fn merge(&mut self, other: Self) {
        if other.level.is_some() {
            self.level = other.level;
        }
        if other.json.is_some() {
            self.json = other.json;
        }
        if other.redact_secrets_in_logs.is_some() {
            self.redact_secrets_in_logs = other.redact_secrets_in_logs;
        }
    }
}

impl RawLoggingConfig {
    fn apply(self, defaults: LoggingConfig) -> LoggingConfig {
        LoggingConfig {
            level: self.level.unwrap_or(defaults.level),
            json: self.json.unwrap_or(defaults.json),
            redact_secrets_in_logs: self
                .redact_secrets_in_logs
                .unwrap_or(defaults.redact_secrets_in_logs),
        }
    }
}

fn validate_app_config(config: &AppConfig) -> Result<(), AppConfigError> {
    if let Some(percent) = config.context.auto_compact_percent {
        if !(1..=99).contains(&percent) {
            return Err(AppConfigError::Validation {
                message: "context.auto_compact_percent must be between 1 and 99".into(),
            });
        }
    }

    if config.context.preserve_recent_turns < 1 {
        return Err(AppConfigError::Validation {
            message: "context.preserve_recent_turns must be at least 1".into(),
        });
    }

    if !(20..=120).contains(&config.conversation.session_titles.max_title_chars) {
        return Err(AppConfigError::Validation {
            message: "conversation.session_titles.max_title_chars must be between 20 and 120"
                .into(),
        });
    }

    if config.tools.file_search.max_results < 1 {
        return Err(AppConfigError::Validation {
            message: "tools.file_search.max_results must be at least 1".into(),
        });
    }

    if config.tools.shell.default_timeout_ms > config.tools.shell.max_timeout_ms {
        return Err(AppConfigError::Validation {
            message: "tools.shell.default_timeout_ms must be <= tools.shell.max_timeout_ms".into(),
        });
    }

    let known_tools = ["shell_command", "file_search", "read_file", "apply_patch"];
    if let Some(unknown) = config
        .tools
        .enabled_tools
        .iter()
        .find(|tool| !known_tools.contains(&tool.as_str()))
    {
        return Err(AppConfigError::Validation {
            message: format!("tools.enabled_tools contains unknown tool '{unknown}'"),
        });
    }

    let mut seen = HashSet::new();
    if config.server.listen.iter().any(|addr| !seen.insert(addr)) {
        return Err(AppConfigError::Validation {
            message: "server.listen must not contain duplicate endpoints".into(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        AppConfig, AppConfigLoader, AppConfigResolver, DefaultAppConfigResolver,
        FileSystemAppConfigLoader, PolicyModelSelection, SummaryModelSelection,
        TitleModelSelection,
    };
    use crate::model::{
        InMemoryModelCatalog, InputModality, ModelConfig, ModelVisibility, ReasoningLevel,
        TruncationPolicyConfig,
    };

    fn test_model(slug: &str) -> ModelConfig {
        ModelConfig {
            slug: slug.into(),
            display_name: slug.into(),
            description: None,
            default_reasoning_level: ReasoningLevel::Medium,
            supported_reasoning_levels: vec![ReasoningLevel::Medium],
            base_instructions: String::new(),
            context_window: 200_000,
            effective_context_window_percent: 90,
            auto_compact_token_limit: None,
            truncation_policy: TruncationPolicyConfig {
                default_max_chars: 8_000,
                tool_output_max_chars: 16_000,
                user_input_max_chars: 32_000,
                binary_placeholder: "[binary]".into(),
                preserve_json_shape: true,
            },
            input_modalities: vec![InputModality::Text],
            supports_image_detail_original: false,
            visibility: ModelVisibility::Visible,
            supported_in_api: true,
            priority: 1,
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("clawcr-{name}-{nanos}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn loader_merges_user_and_project_layers() {
        let root = unique_temp_dir("config-merge");
        let home = root.join("home");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(home.join(".clawcr")).expect("home config dir");
        std::fs::create_dir_all(workspace.join(".clawcr")).expect("workspace config dir");

        std::fs::write(
            home.join(".clawcr").join("config.toml"),
            "logging.level = 'debug'\n[tools.file_search]\nmax_results = 50\n",
        )
        .expect("write user config");
        std::fs::write(
            workspace.join(".clawcr").join("config.toml"),
            "project_root_markers = ['.git', 'Cargo.toml']\n[tools]\nenabled_tools = ['shell_command', 'file_search']\n",
        )
        .expect("write project config");

        let loader = FileSystemAppConfigLoader::new(home);
        let config = loader.load(Some(&workspace)).expect("load config");

        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.tools.file_search.max_results, 50);
        assert_eq!(
            config.project_root_markers,
            vec![".git".to_string(), "Cargo.toml".to_string()]
        );
        assert_eq!(
            config.tools.enabled_tools,
            vec!["shell_command".to_string(), "file_search".to_string()]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn validation_rejects_invalid_shell_timeouts() {
        let raw = "[tools.shell]\ndefault_timeout_ms = 1000\nmax_timeout_ms = 10\n";
        let parsed = toml::from_str::<super::RawAppConfig>(raw).expect("parse");
        let result = parsed.into_app_config();
        assert!(result.is_err());
    }

    #[test]
    fn resolver_supports_configured_and_turn_models() {
        let turn_model = test_model("turn-model");
        let summary_model = test_model("summary-model");
        let catalog = InMemoryModelCatalog::new(vec![turn_model.clone(), summary_model.clone()]);
        let resolver = DefaultAppConfigResolver;

        let config = AppConfig {
            summary_model: SummaryModelSelection::UseConfiguredModel {
                model_slug: "summary-model".into(),
            },
            safety: super::SafetyConfig {
                policy_model: PolicyModelSelection::UseTurnModel,
            },
            conversation: super::ConversationConfig {
                session_titles: super::SessionTitleConfig {
                    mode: super::SessionTitleMode::DeriveThenGenerate,
                    generate_async: true,
                    generation_model: TitleModelSelection::UseTurnModel,
                    max_title_chars: 80,
                },
            },
            ..Default::default()
        };

        let resolved_summary = resolver
            .resolve_summary_model(&config, &turn_model, &catalog)
            .expect("resolve summary");
        let resolved_title = resolver
            .resolve_title_model(&config, &turn_model, &catalog)
            .expect("resolve title");
        let resolved_policy = resolver
            .resolve_policy_model(&config, &turn_model, &catalog)
            .expect("resolve policy");

        assert_eq!(resolved_summary.slug, "summary-model");
        assert_eq!(resolved_title.slug, "turn-model");
        assert_eq!(resolved_policy.slug, "turn-model");
    }
}
