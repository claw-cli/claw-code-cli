use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pretty_assertions::assert_eq;

use super::{
    AppConfig, AppConfigLoader, ContextManageConfig, FileSystemAppConfigLoader, LogRotation,
    LoggingConfig, SafetyPolicyModelSelection, SummaryModelSelection, UpdatesConfig,
};
use crate::SkillsConfig;

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("devo-{name}-{nanos}"));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

#[test]
fn loader_merges_user_project_and_cli_layers() {
    let root = unique_temp_dir("config-merge");
    let home = root.join("home").join(".devo");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).expect("home config dir");
    std::fs::create_dir_all(workspace.join(".devo")).expect("workspace config dir");

    std::fs::write(
        home.join("config.toml"),
        "default_model = 'ignored'\n[anthropic]\nmodel = 'also-ignored'\n[context]\npreserve_recent_turns = 5\n[logging]\nlevel = 'debug'\n[logging.file]\nmax_files = 30\n",
    )
    .expect("write user config");
    std::fs::write(
        workspace.join(".devo").join("config.toml"),
        "enable_auxiliary_model = true\nproject_root_markers = ['.git', 'Cargo.toml']\n[context]\nauto_compact_percent = 80\n[logging]\njson = true\n[logging.file]\ndirectory = 'diagnostics'\nfilename_prefix = 'agent'\n[skills]\nenabled = true\nworkspace_roots = ['project-skills']\nwatch_for_changes = false\n",
    )
    .expect("write project config");

    let cli_overrides: toml::Value = r#"
enable_auxiliary_model = false
summary_model = "UseAxiliaryModel"
safety_policy_model = "UseTurnModel"
project_root_markers = [".workspace"]

[server]
listen = ["stdio://"]

[logging]
level = "trace"

[logging.file]
directory = "cli-logs"
rotation = "Hourly"
max_files = 2

[skills]
enabled = false
user_roots = ["custom-user-skills"]

[updates]
enabled = false
check_interval_hours = 48
"#
    .parse()
    .expect("parse cli overrides");

    let loader = FileSystemAppConfigLoader::new(home).with_cli_overrides(cli_overrides);
    let config = loader.load(Some(&workspace)).expect("load config");

    assert_eq!(
        config,
        AppConfig {
            enable_auxiliary_model: false,
            summary_model: SummaryModelSelection::UseAxiliaryModel,
            safety_policy_model: SafetyPolicyModelSelection::UseTurnModel,
            context: ContextManageConfig {
                preserve_recent_turns: 5,
                auto_compact_percent: Some(80),
                manual_compaction_enabled: true,
            },
            server: super::ServerConfig {
                listen: vec!["stdio://".into()],
                max_connections: 32,
                event_buffer_size: 1024,
                idle_session_timeout_secs: 1800,
                persist_ephemeral_sessions: false,
            },
            logging: LoggingConfig {
                level: "trace".into(),
                json: true,
                redact_secrets_in_logs: true,
                file: super::LoggingFileConfig {
                    directory: Some(PathBuf::from("cli-logs")),
                    filename_prefix: "agent".into(),
                    rotation: LogRotation::Hourly,
                    max_files: 2,
                },
            },
            skills: SkillsConfig {
                enabled: false,
                user_roots: vec![PathBuf::from("custom-user-skills")],
                workspace_roots: vec![PathBuf::from("project-skills")],
                watch_for_changes: false,
            },
            updates: UpdatesConfig {
                enabled: false,
                check_on_startup: true,
                check_interval_hours: 48,
            },
            project_root_markers: vec![".workspace".into()],
        }
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn loader_rejects_invalid_context_thresholds() {
    let root = unique_temp_dir("config-validation");
    let home = root.join("home").join(".devo");
    std::fs::create_dir_all(&home).expect("home config dir");
    std::fs::write(
        home.join("config.toml"),
        "[context]\npreserve_recent_turns = 0\n",
    )
    .expect("write user config");

    let loader = FileSystemAppConfigLoader::new(home);
    let result = loader.load(None);

    assert!(matches!(
        result,
        Err(super::AppConfigError::Validation { .. })
    ));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn loader_rejects_duplicate_skill_roots() {
    let root = unique_temp_dir("config-skill-roots");
    let home = root.join("home").join(".devo");
    std::fs::create_dir_all(&home).expect("home config dir");
    std::fs::write(
        home.join("config.toml"),
        "[skills]\nuser_roots = ['skills', 'skills']\n",
    )
    .expect("write user config");

    let loader = FileSystemAppConfigLoader::new(home);
    let result = loader.load(None);

    assert!(matches!(
        result,
        Err(super::AppConfigError::Validation { .. })
    ));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn default_app_config_enables_startup_update_checks() {
    assert_eq!(
        AppConfig::default().updates,
        UpdatesConfig {
            enabled: true,
            check_on_startup: true,
            check_interval_hours: 24,
        }
    );
}

#[test]
fn loader_rejects_invalid_update_check_interval() {
    let root = unique_temp_dir("config-update-interval");
    let home = root.join("home").join(".devo");
    std::fs::create_dir_all(&home).expect("home config dir");
    std::fs::write(
        home.join("config.toml"),
        "[updates]\ncheck_interval_hours = 0\n",
    )
    .expect("write user config");

    let loader = FileSystemAppConfigLoader::new(home);
    let result = loader.load(None);

    assert!(matches!(
        result,
        Err(super::AppConfigError::Validation { .. })
    ));

    let _ = std::fs::remove_dir_all(root);
}
