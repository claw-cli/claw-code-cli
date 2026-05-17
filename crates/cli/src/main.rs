use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use clap::builder::PossibleValuesParser;
use clap::builder::TypedValueParser as _;
use devo_core::AppConfig;
use devo_core::AppConfigLoader;
use devo_core::FileSystemAppConfigLoader;
use devo_core::LoggingBootstrap;
use devo_core::LoggingRuntime;
use devo_core::SessionId;
use devo_core::UpdateCheckOutcome;
use devo_core::UpdateChecker;
use devo_core::format_update_notification;
use devo_server::ServerProcessArgs;
use devo_server::ServerTransportMode;
use devo_server::run_server_process;
use devo_utils::find_devo_home;
use tracing_subscriber::filter::LevelFilter;

mod agent_command;
mod doctor_command;
mod prompt_command;

use agent_command::run_agent;
use doctor_command::run_doctor;
use prompt_command::run_prompt;

/// Top-level `devo` command that dispatches to interactive agent mode or one
/// of the supporting runtime subcommands.
///
#[derive(Debug, Parser)]
#[command(name = "devo", version, about = "Devo CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Override the model used for this session.
    #[arg(long, global = true)]
    model: Option<String>,

    /// Override the logging level for this process.
    #[arg(
        long = "log-level",
        global = true,
        value_parser = PossibleValuesParser::new(["trace", "debug", "info", "warn", "error"])
            .try_map(|level| level.parse::<LevelFilter>())
    )]
    log_level: Option<LevelFilter>,
}

fn main() -> Result<()> {
    devo_arg0::run_as(|_paths| async { run_cli().await })
}

fn format_with_separators(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_token_usage_line(exit: &devo_tui::AppExit, color_enabled: bool) -> Option<String> {
    let total = exit.total_input_tokens + exit.total_output_tokens;
    let non_cached_input = exit
        .total_input_tokens
        .saturating_sub(exit.total_cache_read_tokens);
    if total == 0 && exit.total_cache_read_tokens == 0 {
        return None;
    }
    let total_value = format_with_separators(total);
    let input_value = format_with_separators(non_cached_input);
    let output_value = format_with_separators(exit.total_output_tokens);
    let cached_suffix = if exit.total_cache_read_tokens > 0 {
        let cached_value = format_with_separators(exit.total_cache_read_tokens);
        if color_enabled {
            format!(
                " (+ {} {})",
                "\u{1b}[1;33m".to_string() + &cached_value + "\u{1b}[0m",
                "\u{1b}[33mcached\u{1b}[0m"
            )
        } else {
            format!(" (+ {cached_value} cached)")
        }
    } else {
        String::new()
    };
    Some(format!(
        "Token usage: total={} input={}{} output={}",
        if color_enabled {
            format!("\u{1b}[1;36m{total_value}\u{1b}[0m")
        } else {
            total_value
        },
        if color_enabled {
            format!("\u{1b}[1;32m{input_value}\u{1b}[0m")
        } else {
            input_value
        },
        cached_suffix,
        if color_enabled {
            format!("\u{1b}[1;35m{output_value}\u{1b}[0m")
        } else {
            output_value
        },
    ))
}

fn exit_messages(exit: &devo_tui::AppExit, color_enabled: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(line) = format_token_usage_line(exit, color_enabled) {
        lines.push(line);
    }
    if let Some(session_id) = exit.session_id {
        let command = format!("devo resume {session_id}");
        let command = if color_enabled {
            format!("\u{1b}[1;36m{command}\u{1b}[0m")
        } else {
            command
        };
        let prefix = if color_enabled {
            "\u{1b}[2mTo continue this session, run\u{1b}[0m".to_string()
        } else {
            "To continue this session, run".to_string()
        };
        lines.push(format!("{prefix} {command}"));
    }
    lines
}

async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    let log_level = cli.log_level.map(|level| level.to_string());

    match &cli.command {
        Some(Command::Onboard) => {
            maybe_print_startup_update(&cli).await;
            // Resolve logging config early, install the process-wide file subscriber,
            // and keep its non-blocking writer guard alive for the command lifetime.
            let _logging = install_logging(&cli)?;
            let exit = run_agent(/*force_onboarding*/ true, log_level.as_deref(), None).await?;
            for line in exit_messages(&exit, /*color_enabled*/ true) {
                println!("{line}");
            }
            Ok(())
        }
        Some(Command::Prompt { input }) => {
            maybe_print_startup_update(&cli).await;
            let _logging = install_logging(&cli)?;
            run_prompt(input, cli.model.as_deref(), log_level.as_deref()).await
        }
        Some(Command::Doctor) => {
            let _logging = install_logging(&cli)?;
            run_doctor().await
        }
        Some(Command::Resume { session_id }) => {
            maybe_print_startup_update(&cli).await;
            let _logging = install_logging(&cli)?;
            let exit = run_agent(
                /*force_onboarding*/ false,
                log_level.as_deref(),
                Some(*session_id),
            )
            .await?;
            for line in exit_messages(&exit, /*color_enabled*/ true) {
                println!("{line}");
            }
            Ok(())
        }
        Some(Command::Server {
            working_root,
            transport,
        }) => {
            let args = ServerProcessArgs {
                working_root: working_root.clone(),
                transport: *transport,
            };
            let _logging = install_server_logging(&args, &cli)?;
            run_server_process(args).await
        }
        None => {
            maybe_print_startup_update(&cli).await;
            let _logging = install_logging(&cli)?;
            let exit = run_agent(/*force_onboarding*/ false, log_level.as_deref(), None).await?;
            for line in exit_messages(&exit, /*color_enabled*/ true) {
                println!("{line}");
            }
            Ok(())
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the interactive onboarding flow to configure a model provider.
    Onboard,
    /// Resume a saved interactive session by id.
    Resume {
        /// Session identifier printed by Devo at exit time.
        session_id: SessionId,
    },
    /// Send a single prompt to the model and print the response (non-interactive).
    Prompt {
        /// The prompt text to send to the model.
        input: String,
    },
    /// Diagnose configuration, provider connectivity, and system health.
    Doctor,
    /// Start the runtime server process.
    #[command(hide = true)]
    Server {
        /// Optional workspace root used for project-level config resolution.
        #[arg(long)]
        working_root: Option<std::path::PathBuf>,
        /// Override the transport mode used by this server process.
        #[arg(long, value_enum, hide = true, default_value_t = ServerTransportMode::Config)]
        transport: ServerTransportMode,
    },
}

async fn maybe_print_startup_update(cli: &Cli) {
    let Ok(home_dir) = find_devo_home() else {
        return;
    };
    let app_config = FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli))
        .load(Some(
            std::env::current_dir()
                .ok()
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new(".")),
        ))
        .unwrap_or_else(|_| AppConfig::default());
    let Ok(checker) = UpdateChecker::new(home_dir, app_config.updates) else {
        return;
    };

    if let UpdateCheckOutcome::UpdateAvailable(notification) =
        checker.check_for_startup_update().await
    {
        eprintln!("{}", format_update_notification(&notification));
    }
}

fn install_logging(cli: &Cli) -> Result<LoggingRuntime> {
    let home_dir = find_devo_home()?;
    let app_config = devo_core::FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli))
        .load(Some(std::env::current_dir()?.as_path()))
        .unwrap_or_else(|err| {
            eprintln!("warning: failed to load app config for logging: {err}");
            devo_core::AppConfig::default()
        });
    LoggingBootstrap {
        process_name: "cli",
        config: app_config.logging,
        home_dir,
    }
    .install()
    .map_err(Into::into)
}

fn install_server_logging(args: &ServerProcessArgs, cli: &Cli) -> Result<LoggingRuntime> {
    let home_dir = find_devo_home()?;
    let loader = devo_core::FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli));
    let app_config = loader
        .load(args.working_root.as_deref())
        .unwrap_or_else(|err| {
            eprintln!("warning: failed to load app config for logging: {err}");
            devo_core::AppConfig::default()
        });
    LoggingBootstrap {
        process_name: "server",
        config: app_config.logging,
        home_dir,
    }
    .install()
    .map_err(Into::into)
}

fn cli_logging_overrides(cli: &Cli) -> toml::Value {
    let Some(log_level) = cli.log_level else {
        return toml::Value::Table(Default::default());
    };

    toml::Value::Table(toml::map::Map::from_iter([(
        "logging".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "level".to_string(),
            toml::Value::String(log_level.to_string()),
        )])),
    )]))
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use devo_core::SessionId;
    use pretty_assertions::assert_eq;
    use tracing_subscriber::filter::LevelFilter;

    use super::Cli;
    use super::Command;
    use super::cli_logging_overrides;
    use super::exit_messages;
    use super::format_token_usage_line;

    #[test]
    fn cli_parses_supported_log_levels() {
        for (level, expected) in [
            ("trace", LevelFilter::TRACE),
            ("debug", LevelFilter::DEBUG),
            ("info", LevelFilter::INFO),
            ("warn", LevelFilter::WARN),
            ("error", LevelFilter::ERROR),
        ] {
            let cli = Cli::try_parse_from(["devo", "--log-level", level]).expect("parse log level");

            assert!(cli.command.is_none());
            assert_eq!(cli.model, None);
            assert_eq!(cli.log_level, Some(expected));
        }
    }

    #[test]
    fn cli_rejects_unsupported_log_levels() {
        let err = Cli::try_parse_from(["devo", "--log-level", "off"]).expect_err("reject off");

        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn cli_logging_overrides_is_empty_without_log_level() {
        let cli = Cli {
            command: None,
            model: None,
            log_level: None,
        };

        assert_eq!(
            cli_logging_overrides(&cli),
            toml::Value::Table(Default::default())
        );
    }

    #[test]
    fn cli_logging_overrides_sets_logging_level() {
        for (level, expected) in [
            (LevelFilter::TRACE, "trace"),
            (LevelFilter::DEBUG, "debug"),
            (LevelFilter::INFO, "info"),
            (LevelFilter::WARN, "warn"),
            (LevelFilter::ERROR, "error"),
        ] {
            let cli = Cli {
                command: None,
                model: None,
                log_level: Some(level),
            };

            assert_eq!(
                cli_logging_overrides(&cli),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "logging".to_string(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "level".to_string(),
                        toml::Value::String(expected.to_string()),
                    )])),
                )]))
            );
        }
    }

    #[test]
    fn startup_update_check_scope_covers_expected_user_facing_commands() {
        for cli in [
            Cli {
                command: None,
                model: None,
                log_level: None,
            },
            Cli {
                command: Some(Command::Onboard),
                model: None,
                log_level: None,
            },
            Cli {
                command: Some(Command::Prompt {
                    input: "hello".to_string(),
                }),
                model: None,
                log_level: None,
            },
        ] {
            assert_eq!(
                matches!(
                    cli.command,
                    None | Some(Command::Onboard) | Some(Command::Prompt { .. })
                ),
                true
            );
        }
    }

    #[test]
    fn startup_update_check_scope_skips_server_and_doctor() {
        let doctor = Cli {
            command: Some(Command::Doctor),
            model: None,
            log_level: None,
        };
        let server = Cli {
            command: Some(Command::Server {
                working_root: None,
                transport: devo_server::ServerTransportMode::Config,
            }),
            model: None,
            log_level: None,
        };

        assert_eq!(
            matches!(
                doctor.command,
                None | Some(Command::Onboard) | Some(Command::Prompt { .. })
            ),
            false
        );
        assert_eq!(
            matches!(
                server.command,
                None | Some(Command::Onboard) | Some(Command::Prompt { .. })
            ),
            false
        );
    }

    #[test]
    fn cli_parses_resume_subcommand() {
        let session_id = SessionId::new();
        let cli =
            Cli::try_parse_from(["devo", "resume", &session_id.to_string()]).expect("parse resume");

        match cli.command {
            Some(Command::Resume { session_id: actual }) => assert_eq!(actual, session_id),
            other => panic!("expected resume command, got {other:?}"),
        }
    }

    #[test]
    fn exit_messages_includes_usage_and_resume_hint() {
        let session_id = SessionId::new();
        let exit = devo_tui::AppExit {
            session_id: Some(session_id),
            turn_count: 1,
            total_input_tokens: 10,
            total_output_tokens: 2,
            total_cache_read_tokens: 5,
        };

        let lines = exit_messages(&exit, /*color_enabled*/ false);
        assert_eq!(
            lines[0],
            "Token usage: total=12 input=5 (+ 5 cached) output=2"
        );
        assert_eq!(
            lines[1],
            format!("To continue this session, run devo resume {session_id}")
        );
    }

    #[test]
    fn colorized_exit_messages_include_ansi_sequences() {
        let session_id = SessionId::new();
        let exit = devo_tui::AppExit {
            session_id: Some(session_id),
            turn_count: 1,
            total_input_tokens: 10,
            total_output_tokens: 2,
            total_cache_read_tokens: 5,
        };

        let usage = format_token_usage_line(&exit, /*color_enabled*/ true).expect("usage line");
        assert!(usage.contains("\u{1b}["));

        let lines = exit_messages(&exit, /*color_enabled*/ true);
        assert!(lines[1].contains("\u{1b}["));
    }
}
