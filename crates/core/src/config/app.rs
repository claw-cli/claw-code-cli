use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use devo_utils::FileSystemConfigPathResolver;

use crate::AgentsMdConfig;
use crate::SkillsConfig;
use crate::config::{
    AppConfigError, ContextManageConfig, LogRotation, LoggingConfig, LoggingFileConfig,
    SafetyPolicyModelSelection, ServerConfig,
};

/// Stores the fully normalized runtime configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    /// Whether enable auxiliary model.
    pub enable_auxiliary_model: bool,
    /// The policy that selects which model should generate context summaries.
    pub summary_model: SummaryModelSelection,
    /// Safety and policy-model defaults.
    pub safety_policy_model: SafetyPolicyModelSelection,
    /// Policy that Context-window management and compaction defaults.
    pub context: ContextManageConfig,
    /// Transport and server runtime defaults.
    pub server: ServerConfig,
    /// Logging and redaction behavior for diagnostics.
    pub logging: LoggingConfig,
    /// Skill discovery roots and behavior.
    pub skills: SkillsConfig,
    /// Startup update-check defaults.
    pub updates: UpdatesConfig,
    /// TODO: Not sure what's purpose of `project_root_markers`?
    /// Marker names used when discovering a project root.
    pub project_root_markers: Vec<String>,
}

/// Controls how the CLI checks for new releases at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatesConfig {
    /// Whether update checking is enabled at all.
    pub enabled: bool,
    /// Whether the CLI should check for updates during startup.
    pub check_on_startup: bool,
    /// Minimum number of hours between network checks.
    pub check_interval_hours: u64,
}

/// Selects the model used for summary generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SummaryModelSelection {
    /// Use the active turn model for compaction summaries.
    UseTurnModel,
    /// Use a separately configured auxiliary model for compaction summaries.
    UseAxiliaryModel,
}

/// Loads the effective application configuration from the supported config sources.
///
/// The effective config must be resolved from exactly three sources, in this
/// priority order:
///
/// 1. command-line startup arguments
/// 2. `<workspace>/.devo/config.toml` for the currently opened project
/// 3. the user config file under the configured config directory
///
/// When the same field appears in multiple sources, the higher-priority source
/// must win.
pub trait AppConfigLoader {
    /// Loads and validates the effective application config for an optional workspace.
    ///
    /// The user config directory may be supplied explicitly by the process
    /// environment. When it is not explicitly configured, the loader falls back
    /// to the default home-directory-based config location.
    fn load(&self, workspace_root: Option<&Path>) -> Result<AppConfig, AppConfigError>;
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            enable_auxiliary_model: false,
            context: ContextManageConfig {
                preserve_recent_turns: 3,
                auto_compact_percent: Some(90),
                manual_compaction_enabled: true,
            },
            summary_model: SummaryModelSelection::UseTurnModel,
            safety_policy_model: SafetyPolicyModelSelection::UseAxiliaryModel,
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
                file: LoggingFileConfig {
                    directory: None,
                    filename_prefix: "devo".into(),
                    rotation: LogRotation::Daily,
                    max_files: 14,
                },
            },
            skills: SkillsConfig {
                enabled: true,
                user_roots: vec![PathBuf::from("skills")],
                workspace_roots: vec![PathBuf::from("skills")],
                watch_for_changes: true,
            },
            updates: UpdatesConfig {
                enabled: true,
                check_on_startup: true,
                check_interval_hours: 24,
            },
            project_root_markers: vec![".git".into()],
        }
    }
}

impl AppConfig {
    pub fn agents_md_config(&self) -> AgentsMdConfig {
        AgentsMdConfig {
            project_root_markers: self.project_root_markers.clone(),
            ..AgentsMdConfig::default()
        }
    }
}

fn read_config_value(path: &Path) -> Result<toml::Value, AppConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| AppConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    toml::from_str::<toml::Value>(&contents).map_err(|source: toml::de::Error| {
        AppConfigError::Parse {
            path: path.to_path_buf(),
            message: source.to_string(),
        }
    })
}

/// Filesystem-backed loader for project and user config files, plus CLI overrides.
#[derive(Debug, Clone)]
pub struct FileSystemAppConfigLoader {
    /// The user config directory used to locate `config.toml`.
    ///
    /// This path usually comes from the environment-aware config-path resolver.
    /// If the environment does not override it, the resolver falls back to the
    /// default home-directory-based config location.
    config_folder_home: PathBuf,
    /// Command-line overrides applied on top of file-backed config.
    cli_overrides: toml::Value,
}

impl FileSystemAppConfigLoader {
    /// Creates a filesystem-backed loader rooted at the provided user config directory.
    pub fn new(config_folder_home: PathBuf) -> Self {
        Self {
            config_folder_home,
            cli_overrides: toml::Value::Table(Default::default()),
        }
    }

    /// Returns a loader that applies CLI overrides with the highest priority.
    pub fn with_cli_overrides(mut self, cli_overrides: toml::Value) -> Self {
        self.cli_overrides = cli_overrides;
        self
    }

    fn user_config_path(&self) -> PathBuf {
        FileSystemConfigPathResolver::new(self.config_folder_home.clone()).user_config_file()
    }

    fn project_config_path(&self, workspace_root: &Path) -> PathBuf {
        FileSystemConfigPathResolver::new(self.config_folder_home.clone())
            .project_config_file(workspace_root)
    }
}

impl AppConfigLoader for FileSystemAppConfigLoader {
    fn load(&self, workspace_root: Option<&Path>) -> Result<AppConfig, AppConfigError> {
        // Merge order is user < project < CLI so the highest-priority source
        // wins for any overlapping field.
        let mut merged = toml::Value::try_from(AppConfig::default())
            .expect("default app config must serialize to TOML");

        let user_path = self.user_config_path();
        if user_path.exists() {
            merge_toml_values(&mut merged, read_config_value(&user_path)?);
        }

        if let Some(workspace_root) = workspace_root {
            let project_path = self.project_config_path(workspace_root);
            if project_path.exists() {
                merge_toml_values(&mut merged, read_config_value(&project_path)?);
            }
        }

        merge_toml_values(&mut merged, self.cli_overrides.clone());

        let config =
            merged
                .try_into()
                .map_err(|source: toml::de::Error| AppConfigError::Parse {
                    path: PathBuf::from("<merged config>"),
                    message: source.to_string(),
                })?;
        validate_app_config(&config)?;
        Ok(config)
    }
}

fn merge_toml_values(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, value) in overlay_table {
                if let Some(existing) = base_table.get_mut(&key) {
                    merge_toml_values(existing, value);
                } else {
                    base_table.insert(key, value);
                }
            }
        }
        (base_value, overlay_value) => *base_value = overlay_value,
    }
}

fn validate_app_config(config: &AppConfig) -> Result<(), AppConfigError> {
    if let Some(percent) = config.context.auto_compact_percent
        && !(1..=99).contains(&percent)
    {
        return Err(AppConfigError::Validation {
            message: "context.auto_compact_percent must be between 1 and 99".into(),
        });
    }

    if config.context.preserve_recent_turns < 1 {
        return Err(AppConfigError::Validation {
            message: "context.preserve_recent_turns must be at least 1".into(),
        });
    }

    let mut seen = HashSet::new();
    if config.server.listen.iter().any(|addr| !seen.insert(addr)) {
        return Err(AppConfigError::Validation {
            message: "server.listen must not contain duplicate endpoints".into(),
        });
    }

    if config.logging.file.max_files < 1 {
        return Err(AppConfigError::Validation {
            message: "logging.file.max_files must be at least 1".into(),
        });
    }

    if config.logging.file.filename_prefix.trim().is_empty() {
        return Err(AppConfigError::Validation {
            message: "logging.file.filename_prefix must not be empty".into(),
        });
    }

    if config.updates.check_interval_hours < 1 {
        return Err(AppConfigError::Validation {
            message: "updates.check_interval_hours must be at least 1".into(),
        });
    }

    let mut seen_skill_roots = HashSet::new();
    if config
        .skills
        .user_roots
        .iter()
        .any(|root| !seen_skill_roots.insert(root))
    {
        return Err(AppConfigError::Validation {
            message: "skills.user_roots must not contain duplicate paths".into(),
        });
    }

    seen_skill_roots.clear();
    if config
        .skills
        .workspace_roots
        .iter()
        .any(|root| !seen_skill_roots.insert(root))
    {
        return Err(AppConfigError::Validation {
            message: "skills.workspace_roots must not contain duplicate paths".into(),
        });
    }

    Ok(())
}
