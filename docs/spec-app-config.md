# Application Config Specification

## Purpose

`devo-core` owns the normalized runtime application configuration. This config
is intentionally limited to settings required by the program at runtime.

Model provider data is not part of this config surface. In particular, the app
config does not store:

- default model slugs
- model catalogs or model lists
- provider slugs
- provider base URLs
- provider API keys

Those values are handled by provider-specific configuration code elsewhere.

## Config Layers

The effective config is resolved from these layers, in increasing priority:

1. built-in defaults compiled into the binary
2. user-level config at `DEVO_HOME/config.toml`
3. project-level config at `<workspace>/.devo/config.toml`
4. CLI overrides supplied to the loader

Higher-priority layers replace lower-priority values when the same field is
present. Nested tables merge recursively by field.

## Runtime Schema

```rust
pub struct AppConfig {
    pub enable_auxiliary_model: bool,
    pub summary_model: SummaryModelSelection,
    pub safety_policy_model: SafetyPolicyModelSelection,
    pub context: ContextManageConfig,
    pub server: ServerConfig,
    pub logging: LoggingConfig,
    pub skills: SkillsConfig,
    pub updates: UpdatesConfig,
    pub project_root_markers: Vec<String>,
}
```

```rust
pub enum SummaryModelSelection {
    UseTurnModel,
    UseAxiliaryModel,
}
```

```rust
pub enum SafetyPolicyModelSelection {
    UseTurnModel,
    UseAxiliaryModel,
}
```

```rust
pub struct ContextManageConfig {
    pub preserve_recent_turns: u32,
    pub auto_compact_percent: Option<u8>,
    pub manual_compaction_enabled: bool,
}
```

```rust
pub struct ServerConfig {
    pub listen: Vec<String>,
    pub max_connections: u32,
    pub event_buffer_size: usize,
    pub idle_session_timeout_secs: u64,
    pub persist_ephemeral_sessions: bool,
}
```

```rust
pub struct LoggingConfig {
    pub level: String,
    pub json: bool,
    pub redact_secrets_in_logs: bool,
    pub file: LoggingFileConfig,
}

pub enum LogRotation {
    Never,
    Minutely,
    Hourly,
    Daily,
}

pub struct LoggingFileConfig {
    pub directory: Option<PathBuf>,
    pub filename_prefix: String,
    pub rotation: LogRotation,
    pub max_files: usize,
}

pub struct SkillsConfig {
    pub enabled: bool,
    pub user_roots: Vec<PathBuf>,
    pub workspace_roots: Vec<PathBuf>,
    pub watch_for_changes: bool,
}

pub struct UpdatesConfig {
    pub enabled: bool,
    pub check_on_startup: bool,
    pub check_interval_hours: u64,
}
```

## Partial Layer Format

The filesystem loader reads a partial config layer from TOML and merges it into
the normalized runtime config.

```rust
pub struct AppConfigOverrides {
    pub enable_auxiliary_model: Option<bool>,
    pub summary_model: Option<SummaryModelSelection>,
    pub safety_policy_model: Option<SafetyPolicyModelSelection>,
    pub context: Option<ContextManageOverrides>,
    pub server: Option<ServerOverrides>,
    pub logging: Option<LoggingOverrides>,
    pub skills: Option<SkillsOverrides>,
    pub updates: Option<UpdatesOverrides>,
    pub project_root_markers: Option<Vec<String>>,
}
```

Nested override structs follow the same field structure as the normalized
runtime structs, but every field is optional so file layers can merge cleanly.

## Loader Interface

```rust
pub trait AppConfigLoader {
    fn load(&self, workspace_root: Option<&Path>) -> Result<AppConfig, AppConfigError>;
}
```

`FileSystemAppConfigLoader` resolves the user config directory from
`DEVO_HOME` and reads the user and project TOML files. It can also carry CLI
overrides through `with_cli_overrides(...)`.

## Validation Rules

The loader must reject normalized configs that violate these invariants:

- `context.auto_compact_percent`, if set, must be between 1 and 99
- `context.preserve_recent_turns` must be at least 1
- `server.listen` must not contain duplicate endpoints
- `logging.file.max_files` must be at least 1
- `logging.file.filename_prefix` must not be empty
- `updates.check_interval_hours` must be at least 1
- `skills.user_roots` must not contain duplicate paths
- `skills.workspace_roots` must not contain duplicate paths

## Update Checks

The app config may optionally control startup update checks:

```toml
[updates]
enabled = true
check_on_startup = true
check_interval_hours = 24
```

These settings control whether user-facing CLI commands check GitHub Releases
for a newer `devo` version and how often a fresh network request is allowed.

## File Locations

- user config: `DEVO_HOME/config.toml`
- project config: `<workspace>/.devo/config.toml`

Both files are optional. Missing files are not errors.

## Out Of Scope

This config spec does not cover:

- provider resolution
- model catalogs
- session state
- tool enablement
- transport protocol semantics beyond the server defaults stored here

Those concerns live in their own modules and specs.
